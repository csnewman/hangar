//! Annotate with Git Blame: each line's commit, said as when and by whom,
//! and in full in its tooltip; a line not yet committed says so.

use chrono::{Local, TimeZone};
use hangar_app::blame::{Blame, UNCOMMITTED};
use serde_json::json;

#[test]
fn each_line_says_who_changed_it_and_when() {
    let b: Blame = serde_json::from_value(json!({
        "commits": {
            "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678": { "author": "Ada", "time": 1.3e9, "summary": "Fix the rounding" },
            UNCOMMITTED: { "author": "Not Committed Yet", "time": 0, "summary": "" },
        },
        "lines": ["a1b2c3d4e5f60718293a4b5c6d7e8f9012345678", UNCOMMITTED],
    }))
    .unwrap();
    let now = Local.with_ymd_and_hms(2026, 11, 11, 12, 0, 0).unwrap();
    let when = hangar_app::gitlog::when(1.3e9, now);
    assert_eq!(b.label(0, now), Some(format!("{when}  Ada")));
    assert_eq!(
        b.about(0).as_deref(),
        Some("a1b2c3d4 Ada\nFix the rounding")
    );
    assert_eq!(b.label(1, now).as_deref(), Some("Not committed"));
    assert_eq!(b.label(2, now), None, "past the end");
}
