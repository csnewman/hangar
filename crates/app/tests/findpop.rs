//! Find in Files' pop-up in an environment's tab: it takes what is typed
//! as it opens, and stays open when clicked in, closing on a click
//! outside it or Escape.

use std::sync::Arc;

use chrono::Utc;
use gpui::{AppContext, Focusable, Modifiers, TestAppContext, VisualTestContext, point, px};
use hangar_app::api::Environment;
use hangar_app::envtab::EnvTab;
use hangar_app::place::Update;
use hangar_app::places::Places;
use hangar_app::shell::Shell;
use hangar_app::store::MemoryTokens;

fn running(id: &str) -> Environment {
    Environment {
        id: id.into(),
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
        task: None,
        agents: vec![],
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

/// A window on a running environment of a place that never connects: its
/// Code service unreached, which the pop-up does not need to open.
fn open(cx: &mut TestAppContext) -> (gpui::Entity<EnvTab>, &mut VisualTestContext) {
    cx.update(gpui_component::init);
    cx.update(hangar_app::envtab::bind_keys);
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let id = places.update(cx, |p, cx| {
        let id = p.add_server("http://127.0.0.1:9", "hgr_t", cx).unwrap();
        p.apply(&id, Update::Snapshot(vec![running("e1")]), cx);
        id
    });
    let (shell, cx) = cx.add_window_view(|window, cx| Shell::new(places.clone(), window, cx));
    cx.update(|window, cx| shell.update(cx, |s, cx| s.open(&id, "e1", window, cx)));
    cx.run_until_parked();
    let tab = shell.read_with(cx, |s, _| s.active_view()).unwrap();
    (tab, cx)
}

#[gpui::test]
fn the_query_takes_what_is_typed_as_it_opens(cx: &mut TestAppContext) {
    let (tab, cx) = open(cx);
    cx.update(|window, cx| tab.update(cx, |t, cx| t.find_in_files(window, cx)));
    cx.run_until_parked();
    let popup = tab
        .read_with(cx, |t, _| t.find_popup())
        .expect("the pop-up");
    cx.update(|window, cx| {
        assert!(
            popup.focus_handle(cx).is_focused(window),
            "the query has the focus"
        );
    });
}

#[gpui::test]
fn a_click_in_it_leaves_it_open_and_one_outside_closes_it(cx: &mut TestAppContext) {
    let (tab, cx) = open(cx);
    cx.update(|window, cx| tab.update(cx, |t, cx| t.find_in_files(window, cx)));
    cx.run_until_parked();
    let bounds = cx.debug_bounds("find-popup").expect("the pop-up drawn");
    cx.simulate_click(bounds.center(), Modifiers::none());
    assert!(
        tab.read_with(cx, |t, _| t.find_popup()).is_some(),
        "a click in the pop-up leaves it open"
    );
    let below = point(bounds.center().x, bounds.bottom() + px(20.));
    cx.simulate_click(below, Modifiers::none());
    assert!(
        tab.read_with(cx, |t, _| t.find_popup()).is_none(),
        "a click outside closes it"
    );
}

/// IntelliJ's keys: Shift-Cmd-F finds in files and Shift-Cmd-R replaces in
/// files from the code editor too, whose own binding of Shift-Cmd-F
/// (replacing in the file) they take the place of; Cmd-R replaces in the
/// file.
#[gpui::test]
fn find_and_replace_in_files_have_intellijs_keys(cx: &mut TestAppContext) {
    let (tab, cx) = open(cx);
    cx.update(|window, cx| {
        tab.update(cx, |t, cx| t.open_file("/home/dev", "notes.md", window, cx))
    });
    cx.run_until_parked();
    let doc = tab
        .read_with(cx, |t, _| t.active_doc())
        .expect("the file open");
    let editor = doc.read_with(cx, |d, _| d.editor().clone());
    let in_editor = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| window.focus(&editor.focus_handle(cx), cx));
        cx.run_until_parked();
    };
    let (find, replace) = if cfg!(target_os = "macos") {
        ("cmd-shift-f", "cmd-shift-r")
    } else {
        ("ctrl-shift-f", "ctrl-shift-r")
    };
    in_editor(cx);
    cx.simulate_keystrokes(find);
    let popup = tab
        .read_with(cx, |t, _| t.find_popup())
        .expect("Find in Files from the editor");
    assert!(!popup.read_with(cx, |p, _| p.replacing()));
    cx.update(|window, cx| tab.update(cx, |t, cx| t.close_find(window, cx)));
    in_editor(cx);
    cx.simulate_keystrokes(replace);
    let popup = tab
        .read_with(cx, |t, _| t.find_popup())
        .expect("Replace in Files from the editor");
    assert!(popup.read_with(cx, |p, _| p.replacing()));
}
