//! Linking places as the app does it: each place's state from what it says
//! of its links, the person they are linked as, and the calls that link
//! them or unlink one.

use chrono::{Duration, Utc};
use hangar_app::linking::{self, Known, State, Step};
use serde_json::json;

const A: &str = "a0000000-0000-4000-8000-000000000001";
const B: &str = "b0000000-0000-4000-8000-000000000002";
const C: &str = "c0000000-0000-4000-8000-000000000003";

fn known(id: &str, name: &str, place: &str, person: &str, items: serde_json::Value) -> Known {
    Known {
        id: id.into(),
        name: name.into(),
        kind: "server",
        url: format!("https://{id}.example"),
        username: Some("admin".into()),
        links: Ok(serde_json::from_value(json!({
            "place": {
                "id": place,
                "public_key": format!("key-{place}"),
                "url": format!("https://{id}.example"),
                "levels": [1, 1],
            },
            "person": person,
            "allowed": true,
            "items": items,
        }))
        .expect("links")),
    }
}

fn link(place: &str, name: &str, extra: serde_json::Value) -> serde_json::Value {
    let mut l = json!({
        "place": place,
        "name": name,
        "url": "https://elsewhere.example",
        "public_key": format!("key-{place}"),
        "created_at": "2026-11-18T10:00:00Z",
    });
    l.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    l
}

#[test]
fn unlinked_places_link_as_the_first_ones_person() {
    let a = known("home", "Home server", A, "pa", json!([]));
    let b = known("lab", "Homelab", B, "pb", json!([]));
    let steps = linking::link_steps(&[&a, &b]).expect("steps");
    assert_eq!(
        steps,
        vec![
            Step::Put {
                at: "home".into(),
                other: B.into(),
                name: "Homelab".into(),
                public_key: format!("key-{B}"),
                url: "https://lab.example".into(),
                person: "pa".into(),
            },
            Step::Put {
                at: "lab".into(),
                other: A.into(),
                name: "Home server".into(),
                public_key: format!("key-{A}"),
                url: "https://home.example".into(),
                person: "pa".into(),
            },
        ]
    );
}

#[test]
fn a_new_place_joins_as_the_linked_ones_person_and_current_links_stay() {
    let a = known(
        "home",
        "Home server",
        A,
        "pa",
        json!([{"place": B, "name": "Homelab", "url": "https://lab.example", "public_key": format!("key-{B}"), "created_at": "2026-11-18T10:00:00Z"}]),
    );
    let b = known(
        "lab",
        "Homelab",
        B,
        "pa",
        json!([{"place": A, "name": "Home server", "url": "https://home.example", "public_key": format!("key-{A}"), "created_at": "2026-11-18T10:00:00Z"}]),
    );
    let c = known("rack", "Rack", C, "pc", json!([]));
    let steps = linking::link_steps(&[&c, &a, &b]).expect("steps");
    assert_eq!(steps.len(), 4, "rack both ways with each, nothing else");
    assert!(
        steps
            .iter()
            .all(|s| matches!(s, Step::Put { person, .. } if person == "pa"))
    );
    assert!(
        steps
            .iter()
            .all(|s| matches!(s, Step::Put { at, other, .. } if at == "rack" || other == C))
    );
}

#[test]
fn two_places_linked_as_different_people_are_not_merged() {
    let a = known(
        "home",
        "Home server",
        A,
        "pa",
        json!([link(C, "x", json!({}))]),
    );
    let b = known("lab", "Homelab", B, "pb", json!([link(C, "y", json!({}))]));
    let e = linking::link_steps(&[&a, &b]).expect_err("refused");
    assert!(e.contains("Home server and Homelab"), "{e}");
}

#[test]
fn a_place_that_cannot_link_stops_it() {
    let a = known("home", "Home server", A, "pa", json!([]));
    let mut b = known("acme", "Acme Hangar", B, "pb", json!([]));
    if let Ok(l) = &mut b.links {
        l.allowed = false;
    }
    let e = linking::link_steps(&[&a, &b]).expect_err("refused");
    assert!(e.starts_with("Acme Hangar:"), "{e}");
    let rows = linking::rows(&[a, b], Utc::now());
    assert_eq!(rows[1].state, State::NotAllowed);
}

#[test]
fn states_follow_what_each_place_reached_lately() {
    let now = Utc::now();
    let recent = (now - Duration::seconds(30)).to_rfc3339();
    let old = (now - Duration::minutes(30)).to_rfc3339();
    let a = known(
        "home",
        "Home server",
        A,
        "pa",
        json!([link(
            B,
            "lab",
            json!({"reached_at": recent, "heard_at": recent})
        )]),
    );
    let b = known(
        "lab",
        "Homelab",
        B,
        "pa",
        json!([link(
            A,
            "home",
            json!({"reached_at": old, "reach_error": "no response"})
        )]),
    );
    let c = known(
        "dev",
        "devbox",
        C,
        "pa",
        json!([link(A, "home", json!({"reach_error": "could not connect"}))]),
    );
    let mut d = known("off", "Offline one", C, "pa", json!([]));
    d.links = Err("no response".into());
    let rows = linking::rows(&[a, b, c, d], now);
    assert_eq!(rows[0].state, State::Linked);
    assert_eq!(
        rows[0].reaches,
        vec!["Homelab".to_string()],
        "named as the app names it"
    );
    assert_eq!(rows[1].state, State::Linked, "reached by Home server");
    assert_eq!(
        rows[1].reached_by,
        vec!["Home server".to_string()],
        "as Home server says"
    );
    assert_eq!(
        rows[1].reaches,
        vec!["Home server".to_string()],
        "Home server heard it"
    );
    assert_eq!(rows[2].state, State::Waiting("could not connect".into()));
    assert_eq!(rows[3].state, State::Unknown("no response".into()));
}

#[test]
fn unlinking_a_place_removes_both_halves() {
    let a = known(
        "home",
        "Home server",
        A,
        "pa",
        json!([link(B, "lab", json!({})), link(C, "dev", json!({}))]),
    );
    let b = known(
        "lab",
        "Homelab",
        B,
        "pa",
        json!([link(A, "home", json!({}))]),
    );
    let c = known(
        "dev",
        "devbox",
        C,
        "pa",
        json!([link(A, "home", json!({}))]),
    );
    let steps = linking::unlink_steps(&a, &[a.clone(), b, c]);
    assert_eq!(
        steps,
        vec![
            Step::Delete {
                at: "home".into(),
                other: B.into()
            },
            Step::Delete {
                at: "home".into(),
                other: C.into()
            },
            Step::Delete {
                at: "lab".into(),
                other: A.into()
            },
            Step::Delete {
                at: "dev".into(),
                other: A.into()
            },
        ]
    );
}
