//! The same random cases through this crate and through CodeMirror itself
//! (`@codemirror/state` and `@codemirror/collab`, from the web UI's
//! node_modules), compared exactly: the JSON of every set, every mapped
//! position, and what a client and the authority make of a run of pushes.
//! Skipped without node or those modules, unless HANGAR_REQUIRE_CODEMIRROR is
//! set, as CI sets it.

mod support;

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use hangar_proto::collab::{self, Authority, Client};
use hangar_proto::{ChangeSet, MapMode, Text};
use ropey::Rope;
use serde_json::{Value, json};
use support::Rng;

const MODES: [MapMode; 4] = [
    MapMode::Simple,
    MapMode::TrackDel,
    MapMode::TrackBefore,
    MapMode::TrackAfter,
];

#[test]
fn matches_codemirror() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let modules = dir.join("../../web/node_modules");
    let required = std::env::var_os("HANGAR_REQUIRE_CODEMIRROR").is_some();
    for m in ["@codemirror/state", "@codemirror/collab"] {
        if !modules.join(m).exists() {
            assert!(!required, "{m} is missing: npm ci in web/");
            eprintln!("skipped: no {m}");
            return;
        }
    }

    let (mut set_cases, mut set_ours) = (vec![], vec![]);
    for seed in 0..2000 {
        let (case, ours) = set_case(&mut Rng::new(seed));
        set_cases.push(case);
        set_ours.push(ours);
    }
    let (mut collab_cases, mut collab_ours) = (vec![], vec![]);
    for seed in 0..2000 {
        let (case, ours) = collab_case(&mut Rng::new(seed));
        collab_cases.push(case);
        collab_ours.push(ours);
    }

    let mut node = match Command::new("node")
        .arg(dir.join("tests/codemirror.mjs"))
        .arg(&modules)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
    {
        Ok(node) => node,
        Err(err) => {
            assert!(!required, "running node: {err}");
            eprintln!("skipped: no node ({err})");
            return;
        }
    };
    let input = json!({ "sets": set_cases, "collab": collab_cases });
    node.stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&input).unwrap())
        .unwrap();
    let out = node.wait_with_output().unwrap();
    assert!(out.status.success(), "node failed");
    let theirs: Value = serde_json::from_slice(&out.stdout).unwrap();

    compare("sets", &set_cases, &set_ours, &theirs["sets"]);
    compare("collab", &collab_cases, &collab_ours, &theirs["collab"]);
}

fn compare(what: &str, cases: &[Value], ours: &[Value], theirs: &Value) {
    let theirs = theirs.as_array().unwrap();
    assert_eq!(theirs.len(), ours.len());
    for (seed, (ours, theirs)) in ours.iter().zip(theirs).enumerate() {
        for (key, value) in ours.as_object().unwrap() {
            assert_eq!(
                value, &theirs[key],
                "{what} seed {seed}: {key} differs\ncase: {}",
                cases[seed]
            );
        }
    }
}

fn set_case(rng: &mut Rng) -> (Value, Value) {
    let doc = support::text(rng, 12);
    let changes = support::changes(rng, &doc, 4);
    let a = support::set(rng, &doc, 4);
    let b = support::set(rng, &doc, 4);
    let after = a.apply(&doc).unwrap();
    let next = support::set(rng, &after, 4);

    let mut positions = vec![];
    for p in 0..=doc.len() {
        for assoc in [-1, 1] {
            for mode in MODES {
                positions.push(json!(a.map_pos(p, assoc, mode).unwrap()));
            }
        }
    }
    let ours = json!({
        "of": ChangeSet::of(changes.clone(), doc.len()).unwrap().to_json(),
        "apply": after.to_string(),
        "map": a.map(&b, false).unwrap().to_json(),
        "mapBefore": a.map(&b, true).unwrap().to_json(),
        "compose": a.compose(&next).unwrap().to_json(),
        "invert": a.invert(&doc).unwrap().to_json(),
        "positions": positions,
    });
    let case = json!({
        "doc": doc.to_string(),
        "changes": changes.iter().map(|c| json!({
            "from": c.from,
            "to": c.to,
            // As ChangeSet.of takes it: one string, lines joined by \n.
            "insert": c.insert.lines().join("\n"),
        })).collect::<Vec<_>>(),
        "a": a.to_json(),
        "b": b.to_json(),
        "next": next.to_json(),
    });
    (case, ours)
}

/// A client, "me", opens a document others are editing, makes changes, and
/// pushes some of them, perhaps more than once, all based on the version it
/// opened at, while others' changes land; then it receives everything since.
fn collab_case(rng: &mut Rng) -> (Value, Value) {
    let mut authority = Authority::new(Rope::from_str(&support::text(rng, 10).to_string()));
    let mut others = 0;
    let mut other_push = |rng: &mut Rng, authority: &mut Authority| {
        let text = Text::from(authority.text());
        let set = support::set(rng, &text, 3);
        others += 1;
        let v = authority.version();
        authority.push(v, &format!("o{others}"), &[set]).unwrap();
    };
    for _ in 0..rng.below(3) {
        other_push(rng, &mut authority);
    }

    let version = authority.version();
    let base = Text::from(authority.text());
    let mut text = base.clone();
    let mut local = vec![];
    for _ in 0..=rng.below(3) {
        // An editor records no empty change, so never pushes one.
        let set = support::set(rng, &text, 3);
        if !set.is_empty() {
            text = set.apply(&text).unwrap();
            local.push(set);
        }
    }

    let mut pushes = vec![];
    let mut rebased = vec![];
    for _ in 0..rng.below(6) {
        if rng.chance(35) && !local.is_empty() {
            let pushed = &local[..=rng.below(local.len())];
            let over = authority.since(version).unwrap().to_vec();
            let ours = collab::rebase(pushed, "me", &over).unwrap();
            rebased.push(json!(ours));
            authority.push(version, "me", pushed).unwrap();
            pushes.push(json!({ "pushed": pushed, "over": over }));
        } else {
            other_push(rng, &mut authority);
        }
    }

    let received = authority.since(version).unwrap().to_vec();
    let mut client = Client::new("me", version);
    for set in &local {
        client.local(set.clone());
    }
    let change = client.receive(version, &received).unwrap();
    let change = change.unwrap_or_else(|| ChangeSet::empty(text.len()));
    let doc = change.apply(&text).unwrap();

    let ours = json!({
        "rebased": rebased,
        "changes": change.to_json(),
        "pending": client.pending(),
        "version": client.version(),
        "doc": doc.to_string(),
    });
    let case = json!({
        "doc": base.to_string(),
        "version": version,
        "me": "me",
        "local": local,
        "pushes": pushes,
        "received": received,
    });
    (case, ours)
}
