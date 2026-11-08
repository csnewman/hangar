//! Others in a shared document: a person's colour as the web UI chooses it,
//! and a caret made at an earlier version moved to where it is now.

use hangar_app::docedit::DocSync;
use hangar_app::presence::{Recent, colour_for, parse_colour};
use hangar_proto::collab::Update;
use hangar_proto::{Change, ChangeSet, Text};

/// The same names get the same colours as in the browser (CodeTab.tsx's
/// colourFor, run under Node for these).
#[test]
fn a_person_has_the_web_uis_colour() {
    assert_eq!(colour_for("admin"), "#d6336c");
    assert_eq!(colour_for("alice"), "#2a78d6");
    assert_eq!(colour_for("ünïcode"), "#1f9d55");
    assert!(parse_colour("#d6336c").is_some());
    assert!(parse_colour("red").is_none());
}

fn insert(at: usize, text: &str, len: usize) -> ChangeSet {
    ChangeSet::of(
        [Change {
            from: at,
            to: at,
            insert: Text::from(text),
        }],
        len,
    )
    .unwrap()
}

#[test]
fn a_caret_made_earlier_moves_over_what_changed_since() {
    let mut r = Recent::default();
    // "hello" at version 0; "> " put before it (v0 -> v1), "!" after (v1 -> v2).
    r.add(0, insert(0, "> ", 5));
    r.add(1, insert(7, "!", 7));
    assert_eq!(r.place(2, 2, 3), Some(3), "made at the version shown");
    assert_eq!(r.place(0, 2, 2), Some(4), "within hello, moved by > ");
    // At the end of hello is where ! went in: a caret goes after what is
    // put in at it, as the web UI's do.
    assert_eq!(r.place(0, 2, 5), Some(8), "the end of hello");
    assert_eq!(r.place(1, 2, 7), Some(8), "after the !, inserted at it");
    assert_eq!(r.place(3, 2, 0), None, "from a version not yet received");
    let mut long = Recent::default();
    long.add(5, insert(0, "x", 5));
    assert_eq!(long.place(0, 6, 1), None, "from before what is kept");
}

/// A caret is moved over this user's changes not yet confirmed too, so it
/// stays on the same text in the editor.
#[test]
fn a_caret_moves_over_changes_not_yet_confirmed() {
    let mut s = DocSync::new("me", 0);
    s.receive(
        0,
        &[Update {
            changes: insert(0, "ab", 3),
            client: "them".into(),
        }],
    )
    .unwrap();
    assert_eq!(s.place(0, 1), Some(3), "over theirs");
    let before = gpui_component::input::Rope::from("ab123");
    let after = gpui_component::input::Rope::from("XXab123");
    s.typed(&before, &after, 0, 0, 2, std::time::Instant::now());
    assert_eq!(s.place(1, 3), Some(5), "over mine, unconfirmed");
}
