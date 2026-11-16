//! The window's tabs kept between runs: written to their file a moment
//! after they change, read back at the next start, and sessions put in
//! the order kept.

use std::time::Duration;

use gpui::TestAppContext;
use hangar_app::kept::{Kept, Open, ordered};
use hangar_app::shell::Opened;

#[test]
fn sessions_come_in_the_order_kept_then_the_rest_as_listed() {
    let listed = vec!["s1", "s2", "s3", "s4"];
    let kept = vec!["s3".to_string(), "gone".into(), "s1".into()];
    assert_eq!(
        ordered(listed, &kept, |s| s),
        ["s3", "s1", "s2", "s4"],
        "a session kept but ended is left out, and new ones follow"
    );
    assert_eq!(ordered(vec!["a", "b"], &[], |s| s), ["a", "b"]);
}

#[gpui::test]
fn the_tabs_are_written_and_read_back(cx: &mut TestAppContext) {
    let dir = std::env::temp_dir().join(format!("hangar-app-kept-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let file = dir.join("open.json");
    let tabs = vec![
        Opened {
            place: "p1".into(),
            env: "e1".into(),
        },
        Opened {
            place: "p2".into(),
            env: "e7".into(),
        },
    ];
    cx.update(|cx| {
        Kept::init(Some(file.clone()), cx);
        Kept::set_tabs(&tabs, Some(1), cx);
        Kept::set_terminals("p1", "e1", vec!["s2".into(), "s1".into()], cx);
    });
    assert!(!file.exists(), "nothing written at once");
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    let written: Open = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(written.tabs, tabs);
    assert_eq!(written.active, Some(1));

    // The next run reads them.
    cx.update(|cx| {
        Kept::init(Some(file.clone()), cx);
        assert_eq!(Kept::open(cx), written);
        assert_eq!(Kept::terminals("p1", "e1", cx), ["s2", "s1"]);
        assert!(Kept::terminals("p1", "e2", cx).is_empty());
        // An environment whose last session goes keeps no list.
        Kept::set_terminals("p1", "e1", vec![], cx);
        assert!(Kept::open(cx).terminals.is_empty());
    });
    let _ = std::fs::remove_dir_all(&dir);
}
