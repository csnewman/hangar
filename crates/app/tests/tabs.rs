//! The editor's tabs: closing as IntelliJ does (Cmd-W, the tab menu's Close
//! Others and Close to one side), Shift-Cmd-T opening the last closed
//! again, the tab shown kept in view however many there are, and splitting
//! the editors in two.

use std::sync::Arc;

use chrono::Utc;
use gpui::{AppContext, Entity, TestAppContext, VisualTestContext, px, size};
use hangar_app::api::Environment;
use hangar_app::envtab::EnvTab;
use hangar_app::place::Update;
use hangar_app::places::Places;
use hangar_app::shell::Shell;
use hangar_app::store::MemoryTokens;

fn running() -> Environment {
    Environment {
        id: "e1".into(),
        short_id: "abc123".into(),
        name: "ledger-1".into(),
        owner: "alice".into(),
        template: "t".into(),
        phase: "running".into(),
        desired: "running".into(),
        reason: None,
        progress: None,
        worker: None,
        worker_online: Some(true),
        cpus: None,
        memory_mib: None,
        editor_path: Some("/home/dev".into()),
        ports_public: false,
        display: None,
        image_update: None,
        web_names: vec![],
        task: None,
        agents: vec![],
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

/// A tab on a running environment whose service is out of reach, with
/// a.rs, b.rs, c.rs and d.rs open, d.rs shown.
fn four(cx: &mut TestAppContext) -> (Entity<EnvTab>, &mut VisualTestContext) {
    // The editors' connections run on threads of their own.
    cx.executor().allow_parking();
    cx.update(gpui_component::init);
    cx.update(hangar_app::envtab::bind_keys);
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let id = places.update(cx, |p, cx| {
        let id = p.add_server("http://127.0.0.1:9", "hgr_t", cx).unwrap();
        p.apply(&id, Update::Snapshot(vec![running()]), cx);
        id
    });
    let (shell, cx) = cx.add_window_view(|window, cx| Shell::new(places.clone(), window, cx));
    cx.update(|window, cx| shell.update(cx, |s, cx| s.open(&id, "e1", window, cx)));
    let tab = shell.read_with(cx, |s, _| s.active_view()).unwrap();
    cx.update(|window, cx| {
        tab.update(cx, |t, cx| {
            for f in ["a.rs", "b.rs", "c.rs", "d.rs"] {
                t.open_file("/home/dev", f, window, cx);
            }
        })
    });
    cx.run_until_parked();
    (tab, cx)
}

fn names(tab: &Entity<EnvTab>, cx: &mut VisualTestContext) -> Vec<String> {
    tab.read_with(cx, |t, cx| t.open_files(cx).0)
        .into_iter()
        .map(|p| p.rsplit('/').next().unwrap().to_string())
        .collect()
}

#[gpui::test]
fn a_closed_tab_opens_again(cx: &mut TestAppContext) {
    let (tab, cx) = four(cx);
    let (close, reopen) = if cfg!(target_os = "macos") {
        ("cmd-w", "cmd-shift-t")
    } else {
        ("ctrl-f4", "ctrl-shift-t")
    };
    cx.update(|window, cx| window.focus(&gpui::Focusable::focus_handle(tab.read(cx), cx), cx));
    cx.simulate_keystrokes(close);
    assert_eq!(
        names(&tab, cx),
        ["a.rs", "b.rs", "c.rs"],
        "the tab shown closes"
    );
    cx.simulate_keystrokes(reopen);
    assert_eq!(
        names(&tab, cx),
        ["a.rs", "b.rs", "c.rs", "d.rs"],
        "and opens again"
    );
}

#[gpui::test]
fn close_others_and_to_either_side(cx: &mut TestAppContext) {
    let (tab, cx) = four(cx);
    cx.update(|_, cx| tab.update(cx, |t, cx| t.close_tabs(|j| j >= 1, cx)));
    assert_eq!(
        names(&tab, cx),
        ["b.rs", "c.rs", "d.rs"],
        "to the left of b.rs"
    );
    cx.update(|_, cx| tab.update(cx, |t, cx| t.close_tabs(|j| j <= 1, cx)));
    assert_eq!(names(&tab, cx), ["b.rs", "c.rs"], "to the right of c.rs");
    cx.update(|_, cx| tab.update(cx, |t, cx| t.close_tabs(|j| j == 0, cx)));
    assert_eq!(names(&tab, cx), ["b.rs"], "all but b.rs");
    cx.update(|window, cx| {
        tab.update(cx, |t, cx| {
            t.reopen_closed(window, cx);
            t.reopen_closed(window, cx);
        })
    });
    assert_eq!(
        names(&tab, cx),
        ["b.rs", "c.rs", "d.rs"],
        "the last closed first"
    );
}

/// With more tabs than the bar has room for, the tab of the file shown is
/// scrolled into view, as each is opened and when an early one is shown
/// again.
#[gpui::test]
fn the_tab_shown_stays_in_view(cx: &mut TestAppContext) {
    let (tab, cx) = four(cx);
    cx.simulate_resize(size(px(1100.), px(700.)));
    let files: Vec<String> = (0..30).map(|i| format!("file-{i:02}.rs")).collect();
    cx.update(|window, cx| {
        tab.update(cx, |t, cx| {
            for f in &files {
                t.open_file("/home/dev", f, window, cx);
            }
        })
    });
    cx.run_until_parked();
    // The scroll is applied as the frame after the change is laid out.
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let bar = |cx: &mut VisualTestContext| {
        let first = cx.debug_bounds("file-tab-0").unwrap();
        let last = cx.debug_bounds("file-tab-33").unwrap();
        (first, last)
    };
    let in_view = |b: gpui::Bounds<gpui::Pixels>, cx: &mut VisualTestContext| {
        let window = cx.update(|window, _| window.bounds().size.width);
        b.left() >= px(0.) && b.right() <= window
    };
    let (first, last) = bar(cx);
    assert!(in_view(last, cx), "the last opened is in view: {last:?}");
    assert!(
        !in_view(first, cx),
        "the first has scrolled away: {first:?}"
    );

    cx.update(|window, cx| tab.update(cx, |t, cx| t.open_file("/home/dev", "a.rs", window, cx)));
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let (first, _) = bar(cx);
    assert!(
        in_view(first, cx),
        "shown again, the first is in view: {first:?}"
    );
}

/// The tab menu's Split and Move Right makes a group on the right, which is
/// then the one in use; a click in a group uses it, what is opened goes
/// there, Open in Opposite Group copies a file across, a group closes with
/// its last tab, and Unsplit gathers both.
#[gpui::test]
fn the_editors_split_in_two(cx: &mut TestAppContext) {
    let (tab, cx) = four(cx);
    let used = |tab: &Entity<EnvTab>, cx: &mut VisualTestContext| names(tab, cx);

    // Close, Close Others, Close Tabs to the Left and to the Right, Close
    // All, Close All but Pinned, Pin Tab, Reopen Closed Tab, Split Right,
    // Split Down, Split and Move Right.
    let b = cx.debug_bounds("file-tab-1").unwrap().center();
    cx.simulate_mouse_down(b, gpui::MouseButton::Right, gpui::Modifiers::none());
    cx.simulate_mouse_up(b, gpui::MouseButton::Right, gpui::Modifiers::none());
    cx.run_until_parked();
    let to_move_right = ["down"; 11].join(" ");
    cx.simulate_keystrokes(&format!("{to_move_right} enter"));
    cx.run_until_parked();
    assert!(tab.read_with(cx, |t, _| t.split_in_two()));
    assert_eq!(used(&tab, cx), ["b.rs"], "moved to the right, in use");
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(cx.debug_bounds("right-tab-0").is_some());
    assert!(
        cx.debug_bounds("file-tab-2").is_some(),
        "c.rs and d.rs stay left"
    );
    assert!(cx.debug_bounds("file-tab-3").is_none());

    // A click on the left uses it; a file opened goes there.
    let a = cx.debug_bounds("file-tab-0").unwrap().center();
    cx.simulate_click(a, gpui::Modifiers::none());
    assert_eq!(used(&tab, cx), ["a.rs", "c.rs", "d.rs"]);
    cx.update(|window, cx| tab.update(cx, |t, cx| t.open_file("/home/dev", "e.rs", window, cx)));
    assert_eq!(used(&tab, cx), ["a.rs", "c.rs", "d.rs", "e.rs"]);

    // A copy of a.rs on the right, which is then in use.
    cx.update(|window, cx| tab.update(cx, |t, cx| t.split(0, false, false, window, cx)));
    assert_eq!(used(&tab, cx), ["b.rs", "a.rs"]);
    assert_eq!(tab.read_with(cx, |t, cx| t.open_files(cx).1), Some(1));

    // Closing the right's tabs closes the group.
    cx.update(|_, cx| tab.update(cx, |t, cx| t.close_tabs(|_| false, cx)));
    assert!(!tab.read_with(cx, |t, _| t.split_in_two()));
    assert_eq!(used(&tab, cx), ["a.rs", "c.rs", "d.rs", "e.rs"]);

    // Split again, then Unsplit: each file once.
    cx.update(|window, cx| tab.update(cx, |t, cx| t.split(1, false, false, window, cx)));
    assert_eq!(used(&tab, cx), ["c.rs"]);
    cx.update(|window, cx| tab.update(cx, |t, cx| t.unsplit(window, cx)));
    assert!(!tab.read_with(cx, |t, _| t.split_in_two()));
    assert_eq!(used(&tab, cx), ["c.rs", "a.rs", "d.rs", "e.rs"]);
}

/// Drags a tab from one place to another, the mouse held, stopping on the
/// way so what shows only while dragging (the zone that splits) is drawn.
fn drag(
    cx: &mut VisualTestContext,
    from: &'static str,
    to: &dyn Fn(&mut VisualTestContext) -> gpui::Point<gpui::Pixels>,
) {
    use gpui::{Modifiers, MouseButton};
    let start = cx.debug_bounds(from).unwrap().center();
    cx.simulate_mouse_move(start, None, Modifiers::none());
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    for dx in [8., 20., 40.] {
        cx.simulate_mouse_move(
            start + gpui::point(px(dx), px(0.)),
            MouseButton::Left,
            Modifiers::none(),
        );
    }
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let end = to(cx);
    cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
}

/// A tab dragged onto the editor's right half splits it; dragged onto a
/// tab of the other group it moves there, the emptied group going; onto a
/// tab of its own group it changes places.
#[gpui::test]
fn tabs_are_dragged_between_and_within_groups(cx: &mut TestAppContext) {
    let (tab, cx) = four(cx);
    drag(cx, "file-tab-0", &|cx| {
        cx.debug_bounds("split-zone")
            .expect("the zone that splits, while dragging")
            .center()
    });
    assert!(tab.read_with(cx, |t, _| t.split_in_two()));
    assert_eq!(names(&tab, cx), ["a.rs"], "a.rs on the right, in use");

    drag(cx, "right-tab-0", &|cx| {
        cx.debug_bounds("file-tab-1").unwrap().center()
    });
    assert!(
        !tab.read_with(cx, |t, _| t.split_in_two()),
        "the emptied group goes"
    );
    assert_eq!(names(&tab, cx), ["b.rs", "a.rs", "c.rs", "d.rs"]);

    drag(cx, "file-tab-0", &|cx| {
        cx.debug_bounds("file-tab-3").unwrap().center()
    });
    assert_eq!(names(&tab, cx), ["a.rs", "c.rs", "b.rs", "d.rs"]);
}

/// Split Down puts the second group under the first.
#[gpui::test]
fn the_editors_split_down(cx: &mut TestAppContext) {
    let (tab, cx) = four(cx);
    cx.update(|window, cx| tab.update(cx, |t, cx| t.split(1, true, true, window, cx)));
    cx.run_until_parked();
    assert!(tab.read_with(cx, |t, _| t.split_in_two()));
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let (top, bottom) = (
        cx.debug_bounds("editors-left").unwrap(),
        cx.debug_bounds("editors-right").unwrap(),
    );
    assert!(bottom.top() >= top.bottom(), "one under the other");
    assert_eq!(top.left(), bottom.left());
}

/// A pinned tab goes first and shows its pin; Close Others and Close All
/// but Pinned leave it, and unpinned it can be closed.
#[gpui::test]
fn pinned_tabs_stay(cx: &mut TestAppContext) {
    let (tab, cx) = four(cx);
    cx.update(|_, cx| tab.update(cx, |t, cx| t.pin_tab(2, true, cx)));
    assert_eq!(names(&tab, cx), ["c.rs", "a.rs", "b.rs", "d.rs"]);
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(cx.debug_bounds("file-pin-0").is_some(), "its pin");
    // Close Others on b.rs.
    cx.update(|_, cx| tab.update(cx, |t, cx| t.close_unpinned(|j| j == 2, cx)));
    assert_eq!(names(&tab, cx), ["c.rs", "b.rs"]);
    cx.update(|_, cx| tab.update(cx, |t, cx| t.close_unpinned(|_| false, cx)));
    assert_eq!(names(&tab, cx), ["c.rs"], "Close All but Pinned");
    cx.update(|_, cx| tab.update(cx, |t, cx| t.pin_tab(0, false, cx)));
    cx.update(|_, cx| tab.update(cx, |t, cx| t.close_unpinned(|_| false, cx)));
    assert!(names(&tab, cx).is_empty(), "unpinned, it goes");
}
