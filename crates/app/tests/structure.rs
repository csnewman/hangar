//! A file's outline from its language server's documentSymbol answer, in
//! either shape: nested DocumentSymbols, or flat SymbolInformation.

use hangar_app::structure::outline;
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
