//! A file's outline from its language server's documentSymbol answer, in
//! either shape: nested DocumentSymbols, or flat SymbolInformation.

use hangar_app::structure::{at_line, has_children, markdown, outline, places, visible};
use serde_json::json;

fn at(line: u32, ch: u32) -> serde_json::Value {
    json!({ "start": { "line": line, "character": ch }, "end": { "line": line, "character": ch + 1 } })
}

#[test]
fn nested_symbols_in_order_under_their_parents() {
    let answer = json!([
        { "name": "SessionStore", "kind": 23, "range": at(3, 0), "selectionRange": at(3, 11),
          "children": [
            { "name": "refresh", "kind": 6, "range": at(40, 4), "selectionRange": at(40, 17) },
            { "name": "revoke", "kind": 6, "range": at(60, 4), "selectionRange": at(60, 11) },
          ] },
        { "name": "TTL", "kind": 14, "range": at(1, 0), "selectionRange": at(1, 6) },
    ]);
    let o = outline(&answer);
    let rows: Vec<(usize, &str, &str, u32, u32)> = o
        .iter()
        .map(|s| (s.depth, s.kind, s.name.as_str(), s.line, s.column))
        .collect();
    assert_eq!(
        rows,
        [
            (0, "struct", "SessionStore", 3, 11),
            (1, "fn", "refresh", 40, 17),
            (1, "fn", "revoke", 60, 11),
            (0, "const", "TTL", 1, 6),
        ]
    );
}

#[test]
fn flat_symbols_at_their_locations() {
    let answer = json!([
        { "name": "main", "kind": 12, "location": { "uri": "file:///a.rs", "range": at(9, 3) } },
    ]);
    let o = outline(&answer);
    assert_eq!(
        (o[0].depth, o[0].kind, o[0].line, o[0].column),
        (0, "fn", 9, 3)
    );
    assert!(outline(&json!(null)).is_empty());
}

#[test]
fn a_name_that_starts_with_its_kind_is_not_said_twice() {
    let answer = json!([
        { "name": "impl Ledger", "kind": 19, "range": at(4, 0), "selectionRange": at(4, 5) },
        { "name": "implement", "kind": 12, "range": at(9, 0), "selectionRange": at(9, 3) },
    ]);
    let o = outline(&answer);
    assert_eq!((o[0].kind, o[0].name.as_str()), ("impl", "Ledger"));
    assert_eq!((o[1].kind, o[1].name.as_str()), ("fn", "implement"));
}

fn span(a: u32, b: u32) -> serde_json::Value {
    json!({ "start": { "line": a, "character": 0 }, "end": { "line": b, "character": 1 } })
}

#[test]
fn the_caret_is_in_the_innermost_symbol_spanning_its_line() {
    let answer = json!([
        { "name": "Ledger", "kind": 23, "range": span(0, 20), "selectionRange": at(0, 7),
          "detail": "struct Ledger",
          "children": [
            { "name": "record", "kind": 6, "range": span(5, 8), "selectionRange": at(5, 7),
              "detail": "fn(&mut self, amount: i64)" },
            { "name": "add", "kind": 6, "range": span(10, 12), "selectionRange": at(10, 7) },
          ] },
        { "name": "main", "kind": 12, "range": span(22, 26), "selectionRange": at(22, 3) },
    ]);
    let o = outline(&answer);
    assert_eq!(o[1].detail.as_deref(), Some("fn(&mut self, amount: i64)"));
    assert_eq!(o[1].path, "/struct:Ledger/fn:record");
    assert_eq!(at_line(&o, 6), Some(1));
    assert_eq!(at_line(&o, 9), Some(0));
    assert_eq!(at_line(&o, 21), None);
    assert_eq!(at_line(&o, 26), Some(3));
    assert!(has_children(&o, 0));
    assert!(!has_children(&o, 1));

    let names =
        |rows: Vec<usize>| -> Vec<&str> { rows.iter().map(|i| o[*i].name.as_str()).collect() };
    let mut folded = std::collections::HashSet::new();
    assert_eq!(
        names(visible(&o, &folded, false)),
        ["Ledger", "record", "add", "main"]
    );
    // Sorted, each level by name, children kept under their parents.
    assert_eq!(
        names(visible(&o, &folded, true)),
        ["Ledger", "add", "record", "main"]
    );
    folded.insert("/struct:Ledger".to_string());
    assert_eq!(names(visible(&o, &folded, false)), ["Ledger", "main"]);

    let p = places(&o);
    assert_eq!(
        p[1],
        (
            "record".into(),
            "Ledger · fn(&mut self, amount: i64)".into(),
            5,
            7
        )
    );
    assert_eq!(p[3], ("main".into(), "fn".into(), 22, 3));
}

#[test]
fn markdown_headings_nest_by_level_and_fenced_code_is_passed_over() {
    let text = "# Title\n\nintro\n\n## Install\n\n```sh\n# not a heading\n```\n\n### From source\n\n## Use ##\n#hashtag\n";
    let o = markdown(text);
    let rows: Vec<(usize, &str, u32)> = o
        .iter()
        .map(|s| (s.depth, s.name.as_str(), s.line))
        .collect();
    assert_eq!(
        rows,
        [
            (0, "Title", 0),
            (1, "Install", 4),
            (2, "From source", 10),
            (1, "Use", 12)
        ]
    );
    // Install spans to the next heading of its level.
    assert_eq!(o[1].lines, 4..12);
    assert_eq!(o[0].lines, 0..14);
    assert_eq!(o[2].path, "/Title/Install/From source");
}

#[test]
fn flat_symbols_nest_by_the_ranges_holding_them() {
    let loc = |a: u32, b: u32| json!({ "uri": "file:///w/main.rs", "range": span(a, b) });
    let answer = json!([
        { "name": "Ledger", "kind": 23, "location": loc(0, 2) },
        { "name": "entries", "kind": 8, "location": loc(1, 1) },
        { "name": "impl Ledger", "kind": 19, "location": loc(4, 12) },
        { "name": "record", "kind": 6, "location": loc(5, 7) },
        { "name": "main", "kind": 12, "location": loc(14, 19) },
    ]);
    let o = outline(&answer);
    let rows: Vec<(usize, &str)> = o.iter().map(|s| (s.depth, s.path.as_str())).collect();
    assert_eq!(
        rows,
        [
            (0, "/struct:Ledger"),
            (1, "/struct:Ledger/field:entries"),
            (0, "/impl:Ledger"),
            (1, "/impl:Ledger/fn:record"),
            (0, "/fn:main"),
        ]
    );
}
