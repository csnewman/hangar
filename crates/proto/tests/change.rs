mod support;

use hangar_proto::{Change, ChangeSet, Error, MapMode, Text};
use serde_json::json;
use support::Rng;

const SEEDS: u64 = 3000;

fn splice(t: &Text, changes: &[Change]) -> Text {
    let mut units = t.units().to_vec();
    for c in changes.iter().rev() {
        units.splice(c.from..c.to, c.insert.units().iter().copied());
    }
    Text::from(String::from_utf16(&units).unwrap().as_str())
}

#[test]
fn of_applies_each_change() {
    for seed in 0..SEEDS {
        let mut rng = Rng::new(seed);
        let doc = support::text(&mut rng, 12);
        let changes = support::sorted_changes(&mut rng, &doc, 4);
        let set = ChangeSet::of(changes.clone(), doc.len()).unwrap();
        assert_eq!(set.len(), doc.len(), "seed {seed}");
        let got = set.apply(&doc).unwrap();
        assert_eq!(got, splice(&doc, &changes), "seed {seed}");
        assert_eq!(got.len(), set.new_len(), "seed {seed}");
    }
}

#[test]
fn compose_is_applying_both() {
    for seed in 0..SEEDS {
        let mut rng = Rng::new(seed);
        let doc = support::text(&mut rng, 12);
        let a = support::set(&mut rng, &doc, 4);
        let mid = a.apply(&doc).unwrap();
        let b = support::set(&mut rng, &mid, 4);
        let both = a.compose(&b).unwrap();
        assert_eq!(
            both.apply(&doc).unwrap(),
            b.apply(&mid).unwrap(),
            "seed {seed}"
        );
    }
}

#[test]
fn mapping_either_way_converges() {
    for seed in 0..SEEDS {
        let mut rng = Rng::new(seed);
        let doc = support::text(&mut rng, 12);
        let a = support::set(&mut rng, &doc, 4);
        let b = support::set(&mut rng, &doc, 4);
        let ab = a.compose(&b.map(&a, false).unwrap()).unwrap();
        let ba = b.compose(&a.map(&b, true).unwrap()).unwrap();
        assert_eq!(
            ab.apply(&doc).unwrap(),
            ba.apply(&doc).unwrap(),
            "seed {seed}"
        );
    }
}

#[test]
fn invert_undoes() {
    for seed in 0..SEEDS {
        let mut rng = Rng::new(seed);
        let doc = support::text(&mut rng, 12);
        let a = support::set(&mut rng, &doc, 4);
        let after = a.apply(&doc).unwrap();
        assert_eq!(
            a.invert(&doc).unwrap().apply(&after).unwrap(),
            doc,
            "seed {seed}"
        );
    }
}

#[test]
fn json_round_trips() {
    for seed in 0..SEEDS {
        let mut rng = Rng::new(seed);
        let doc = support::text(&mut rng, 12);
        let a = support::set(&mut rng, &doc, 4);
        let back = ChangeSet::from_json(&a.to_json()).unwrap();
        assert_eq!(
            back.apply(&doc).unwrap(),
            a.apply(&doc).unwrap(),
            "seed {seed}"
        );
        assert_eq!(back.to_json(), a.to_json(), "seed {seed}");
    }
}

#[test]
fn kept_text_maps_to_itself() {
    for seed in 0..SEEDS {
        let mut rng = Rng::new(seed);
        let doc = support::text(&mut rng, 12);
        let a = support::set(&mut rng, &doc, 3);
        let after = a.apply(&doc).unwrap();
        for pos in 0..doc.len() {
            // Some only when the unit after pos is kept; text inserted at
            // pos goes before it.
            if let Some(to) = a.map_pos(pos, 1, MapMode::TrackAfter).unwrap() {
                assert_eq!(doc.units()[pos], after.units()[to], "seed {seed} pos {pos}");
            }
        }
    }
}

#[test]
fn json_form() {
    let doc = Text::from("hello\nworld");
    let set = ChangeSet::of(
        [
            Change {
                from: 0,
                to: 5,
                insert: Text::from("hi"),
            },
            Change {
                from: 6,
                to: 6,
                insert: Text::from("big\r\n"),
            },
        ],
        doc.len(),
    )
    .unwrap();
    assert_eq!(set.to_json(), json!([[5, "hi"], 1, [0, "big", ""], 5]));
    assert_eq!(set.apply(&doc).unwrap().to_string(), "hi\nbig\nworld");
}

#[test]
fn utf16_positions() {
    let doc = Text::from("😀x");
    assert_eq!(doc.len(), 3);
    let set = ChangeSet::of(
        [Change {
            from: 2,
            to: 3,
            insert: Text::from("y"),
        }],
        3,
    )
    .unwrap();
    assert_eq!(set.apply(&doc).unwrap().to_string(), "😀y");
}

#[test]
fn refuses_what_does_not_fit() {
    let doc = Text::from("abc");
    let set = ChangeSet::empty(4);
    assert_eq!(
        set.apply(&doc),
        Err(Error::Length {
            expected: 4,
            got: 3
        })
    );
    assert!(ChangeSet::empty(3).compose(&ChangeSet::empty(4)).is_err());
    assert!(ChangeSet::empty(3).map(&set, false).is_err());
    assert!(ChangeSet::empty(3).map_pos(4, 0, MapMode::Simple).is_err());
    let out_of_range = Change {
        from: 2,
        to: 5,
        insert: Text::new(),
    };
    assert!(ChangeSet::of([out_of_range], 3).is_err());
}

#[test]
fn refuses_malformed_json() {
    for bad in [
        json!({}),
        json!([-1]),
        json!([1.5]),
        json!([[]]),
        json!([["a"]]),
        json!([[1, 2]]),
        json!([9007199254740992u64]),
        json!(["x"]),
    ] {
        assert!(ChangeSet::from_json(&bad).is_err(), "{bad}");
    }
}

#[test]
fn malformed_sets_fail_without_panicking() {
    // Sets that are valid JSON but nonsense together: every operation either
    // works or says why, and none loops.
    let shapes = [
        json!([0, [0], 0]),
        json!([[0, "a"], [0, "b"], 0]),
        json!([3, [0, "x"], [2]]),
        json!([[4, ""], 1]),
        json!([0, 0, 0, 5]),
    ];
    for a in &shapes {
        for b in &shapes {
            let a = ChangeSet::from_json(a).unwrap();
            let b = ChangeSet::from_json(b).unwrap();
            let _ = a.compose(&b);
            let _ = a.map(&b, false);
            let _ = a.map(&b, true);
            let _ = a.apply(&Text::from("abcde"));
        }
    }
}
