//! A workspace/symbol answer as Go to Symbol's results, in either of the
//! shapes servers give it.

use hangar_app::search::{Target, symbols};
use serde_json::json;

#[test]
fn symbols_with_their_places() {
    let answer = json!([
        { "name": "refresh", "kind": 6, "containerName": "SessionStore",
          "location": { "uri": "file:///home/dev/app/src/session.rs",
                        "range": { "start": { "line": 40, "character": 17 }, "end": { "line": 40, "character": 24 } } } },
        { "name": "Session", "kind": 23,
          "location": { "uri": "file:///home/dev/app/src/session.rs" } },
    ]);
    let s = symbols(&answer);
    assert_eq!(s.len(), 2);
    assert_eq!(s[0].0, "refresh");
    assert_eq!(s[0].1, "SessionStore · session.rs");
    assert_eq!(
        s[0].2,
        Target::At {
            root: "/".into(),
            path: "/home/dev/app/src/session.rs".into(),
            line: 40,
            column: 17
        }
    );
    assert_eq!(
        s[1].1, "session.rs",
        "a WorkspaceSymbol without a range or container"
    );
    assert!(symbols(&json!(null)).is_empty());
}
