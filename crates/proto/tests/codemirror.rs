//! The same random cases through this crate and through `@codemirror/state`,
//! from the web UI's node_modules, compared exactly: the JSON of every set and
//! every mapped position. Skipped without node or those modules, unless
//! HANGAR_REQUIRE_CODEMIRROR is set, as CI sets it.

mod support;

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use hangar_proto::{ChangeSet, MapMode, Text};
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
    let module = dir.join("../../web/node_modules/@codemirror/state/dist/index.js");
    let required = std::env::var_os("HANGAR_REQUIRE_CODEMIRROR").is_some();
    if !module.exists() {
        assert!(!required, "{} is missing: npm ci in web/", module.display());
        eprintln!("skipped: no {}", module.display());
        return;
    }

    let mut cases = vec![];
    let mut ours = vec![];
    for seed in 0..2000 {
        let mut rng = Rng::new(seed);
        let doc = support::text(&mut rng, 12);
        let changes = support::changes(&mut rng, &doc, 4);
        let a = support::set(&mut rng, &doc, 4);
        let b = support::set(&mut rng, &doc, 4);
        let after = a.apply(&doc).unwrap();
        let next = support::set(&mut rng, &after, 4);

        let mut positions = vec![];
        for p in 0..=doc.len() {
            for assoc in [-1, 1] {
                for mode in MODES {
                    positions.push(json!(a.map_pos(p, assoc, mode).unwrap()));
                }
            }
        }
        ours.push(json!({
            "of": ChangeSet::of(changes.clone(), doc.len()).unwrap().to_json(),
            "apply": after.to_string(),
            "map": a.map(&b, false).unwrap().to_json(),
            "mapBefore": a.map(&b, true).unwrap().to_json(),
            "compose": a.compose(&next).unwrap().to_json(),
            "invert": a.invert(&doc).unwrap().to_json(),
            "positions": positions,
        }));
        cases.push(json!({
            "doc": doc.to_string(),
            "changes": changes.iter().map(|c| json!({
                "from": c.from, "to": c.to, "insert": text_json(&c.insert),
            })).collect::<Vec<_>>(),
            "a": a.to_json(),
            "b": b.to_json(),
            "next": next.to_json(),
        }));
    }

    let mut node = match Command::new("node")
        .arg(dir.join("tests/codemirror.mjs"))
        .arg(&module)
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
    let input = serde_json::to_vec(&cases).unwrap();
    node.stdin.take().unwrap().write_all(&input).unwrap();
    let out = node.wait_with_output().unwrap();
    assert!(out.status.success(), "node failed");
    let theirs: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap();

    assert_eq!(theirs.len(), ours.len());
    for (seed, (ours, theirs)) in ours.iter().zip(&theirs).enumerate() {
        for key in [
            "of",
            "apply",
            "map",
            "mapBefore",
            "compose",
            "invert",
            "positions",
        ] {
            assert_eq!(
                ours[key], theirs[key],
                "seed {seed}: {key} differs\ncase: {}",
                cases[seed]
            );
        }
    }
}

// Inserted text as CodeMirror's ChangeSet.of takes it, a string with `\n`
// between lines.
fn text_json(t: &Text) -> Value {
    Value::from(t.lines().join("\n"))
}
