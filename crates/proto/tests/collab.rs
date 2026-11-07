mod support;

use hangar_proto::collab::{Authority, Client, Update};
use hangar_proto::{Change, ChangeSet, Error, Text};
use ropey::Rope;
use support::Rng;

struct Editor {
    client: Client,
    text: Text,
}

impl Editor {
    fn new(id: &str, authority: &Authority) -> Self {
        Editor {
            client: Client::new(id, authority.version()),
            text: Text::from(authority.text()),
        }
    }

    fn edit(&mut self, rng: &mut Rng) {
        let set = support::set(rng, &self.text, 3);
        self.text = set.apply(&self.text).unwrap();
        self.client.local(set);
    }

    fn push(&self, authority: &mut Authority) {
        let pending = self.client.pending();
        if !pending.is_empty() {
            authority
                .push(self.client.version(), self.client.id(), pending)
                .unwrap();
        }
    }

    fn receive(&mut self, version: u64, updates: &[Update]) {
        if let Some(change) = self.client.receive(version, updates).unwrap() {
            self.text = change.apply(&self.text).unwrap();
        }
    }

    fn catch_up(&mut self, authority: &Authority) {
        let version = self.client.version();
        let updates = authority.since(version).unwrap().to_vec();
        self.receive(version, &updates);
    }
}

fn disk_change(rng: &mut Rng, authority: &mut Authority) {
    let set = support::set(rng, &Text::from(authority.text()), 2);
    let v = authority.version();
    authority.push(v, "disk", &[set]).unwrap();
}

#[test]
fn editors_converge() {
    for seed in 0..500 {
        let mut rng = Rng::new(seed);
        let mut authority =
            Authority::new(Rope::from_str(&support::text(&mut rng, 10).to_string()));
        let mut editors: Vec<Editor> = ["a", "b", "c"]
            .iter()
            .map(|id| Editor::new(id, &authority))
            .collect();
        // The text at every version, to check each editor against.
        let mut history = vec![Text::from(authority.text())];
        for step in 0..80 {
            // A steady stream of changes on disk, as when Claude edits the
            // file: every push is behind one of them more often than not.
            if rng.chance(30) {
                disk_change(&mut rng, &mut authority);
            }
            let editor = &mut editors[rng.below(3)];
            match rng.below(5) {
                0 | 1 => editor.edit(&mut rng),
                // Pushed twice, as a client resending after a lost reply.
                2 => {
                    editor.push(&mut authority);
                    if rng.chance(30) {
                        editor.push(&mut authority);
                    }
                }
                3 => {
                    // Only part of what has happened has reached it.
                    let version = editor.client.version();
                    let all = authority.since(version).unwrap();
                    let some = all[..rng.below(all.len() + 1)].to_vec();
                    editor.receive(version, &some);
                }
                _ => {
                    // Some of it again, with what it already had.
                    let version = editor.client.version().saturating_sub(2);
                    if let Ok(updates) = authority.since(version) {
                        let updates = updates.to_vec();
                        editor.receive(version, &updates);
                    }
                }
            }
            let known = history.len() as u64 - 1;
            for u in authority.since(known).unwrap() {
                let next = u.changes.apply(history.last().unwrap()).unwrap();
                history.push(next);
            }
            // Each editor's text is the authority's at its version with its
            // unconfirmed changes on top.
            for editor in &editors {
                let mut text = history[editor.client.version() as usize].clone();
                for p in editor.client.pending() {
                    text = p.apply(&text).unwrap();
                }
                assert_eq!(text, editor.text, "seed {seed} step {step}");
            }
        }
        for editor in &mut editors {
            editor.push(&mut authority);
        }
        for editor in &mut editors {
            editor.catch_up(&authority);
            assert!(editor.client.pending().is_empty(), "seed {seed}");
            assert_eq!(editor.text, Text::from(authority.text()), "seed {seed}");
        }
    }
}

#[test]
fn a_push_behind_is_rebased() {
    let mut authority = Authority::new(Rope::from_str("abc"));
    let insert = |at, s| {
        ChangeSet::of(
            [Change {
                from: at,
                to: at,
                insert: Text::from(s),
            }],
            3,
        )
        .unwrap()
    };
    authority.push(0, "a", &[insert(3, "d")]).unwrap();
    authority.push(0, "b", &[insert(0, "z")]).unwrap();
    assert_eq!(authority.version(), 2);
    assert_eq!(authority.text().to_string(), "zabcd");
}

#[test]
fn a_push_already_taken_is_dropped() {
    let mut authority = Authority::new(Rope::from_str("abc"));
    let mut a = Editor::new("a", &authority);
    a.edit(&mut Rng::new(3));
    a.push(&mut authority);
    a.push(&mut authority);
    assert_eq!(authority.version(), 1);
    a.catch_up(&authority);
    assert!(a.client.pending().is_empty());
    assert_eq!(a.text, Text::from(authority.text()));
}

#[test]
fn a_change_that_does_not_fit_takes_nothing() {
    let mut authority = Authority::new(Rope::from_str("abc"));
    let err = authority.push(0, "a", &[ChangeSet::empty(3), ChangeSet::empty(7)]);
    assert!(err.is_err());
    assert_eq!(authority.version(), 0);
    assert_eq!(authority.text().to_string(), "abc");
}

#[test]
fn half_a_surrogate_pair_is_refused() {
    let mut authority = Authority::new(Rope::from_str("😀x"));
    let split = ChangeSet::of(
        [Change {
            from: 1,
            to: 1,
            insert: Text::from("y"),
        }],
        3,
    )
    .unwrap();
    assert_eq!(
        authority.push(0, "a", &[split]),
        Err(Error::Surrogate { pos: 1 })
    );
    assert_eq!(authority.text().to_string(), "😀x");
}

#[test]
fn updates_received_twice_are_skipped() {
    let mut authority = Authority::new(Rope::from_str("abc"));
    let mut a = Editor::new("a", &authority);
    let mut b = Editor::new("b", &authority);
    b.edit(&mut Rng::new(7));
    b.push(&mut authority);
    let updates = authority.since(0).unwrap().to_vec();
    a.receive(0, &updates);
    a.receive(0, &updates);
    assert_eq!(a.text, Text::from(authority.text()));
    assert_eq!(a.client.version(), authority.version());
}

#[test]
fn a_gap_is_an_error() {
    let mut client = Client::new("a", 2);
    assert!(client.receive(5, &[]).is_err());
    let authority = Authority::new(Rope::new());
    assert!(authority.since(1).is_err());
}

#[test]
fn history_beyond_the_budget_is_forgotten() {
    let mut authority = Authority::with_budget(Rope::from_str("abc"), 1000);
    let mut rng = Rng::new(1);
    for _ in 0..200 {
        disk_change(&mut rng, &mut authority);
    }
    assert!(matches!(authority.since(0), Err(Error::Forgotten { .. })));
    let mut stale = Editor::new("s", &Authority::new(Rope::from_str("abc")));
    stale.edit(&mut rng);
    let pending = stale.client.pending().to_vec();
    assert!(matches!(
        authority.push(0, "s", &pending),
        Err(Error::Forgotten { .. })
    ));
    // What is kept still brings a recent client up to date.
    let mut recent = Editor::new("r", &authority);
    disk_change(&mut rng, &mut authority);
    recent.catch_up(&authority);
    assert_eq!(recent.text, Text::from(authority.text()));
}
