use hangar_app::branch::Changes;
use serde_json::json;

#[test]
fn a_branchs_changes_are_read_as_the_service_gives_them() {
    let c: Changes = serde_json::from_value(json!({
        "base": "main",
        "mergeBase": "2d194c25aa",
        "commits": [{"hash": "6464f3a", "subject": "Flush the audit trail", "author": "dev", "time": 1.7e9}],
        "changes": [
            {"path": "src/audit.rs", "status": "A"},
            {"path": "src/ledger.rs", "status": "R", "from": "src/old.rs"}
        ]
    }))
    .unwrap();
    assert_eq!(c.base, "main");
    assert_eq!(c.merge_base, "2d194c25aa");
    assert_eq!(c.commits[0].subject, "Flush the audit trail");
    assert_eq!(c.changes[1].from.as_deref(), Some("src/old.rs"));
}
