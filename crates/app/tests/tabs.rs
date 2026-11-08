//! The editor's tabs: closing as IntelliJ does (Cmd-W, the tab menu's Close
//! Others and Close to one side), and Shift-Cmd-T opening the last closed
//! again.

use std::sync::Arc;

use chrono::Utc;
use gpui::{AppContext, Entity, TestAppContext, VisualTestContext};
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
