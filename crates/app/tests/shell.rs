//! The window's tabs, and adding a server place.

use std::sync::Arc;

use gpui::{AppContext, TestAppContext};
use hangar_app::envtab::Tool;
use hangar_app::place::Connection;
use hangar_app::places::Places;
use hangar_app::shell::Shell;
use hangar_app::store::{MemoryTokens, Tokens};

#[gpui::test]
fn an_environment_opens_in_one_tab(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let (shell, cx) = cx.add_window_view(|window, cx| Shell::new(places.clone(), window, cx));
    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.open("p", "e1", window, cx);
            s.open("p", "e2", window, cx);
            s.open("p", "e1", window, cx);
        })
    });
    shell.read_with(cx, |s, _| {
        let (tabs, active) = s.tabs();
        assert_eq!(tabs.len(), 2, "opening it again shows its tab");
        assert_eq!(active, Some(0));
    });
    cx.update(|_, cx| shell.update(cx, |s, cx| s.show_home(cx)));
    shell.read_with(cx, |s, _| assert_eq!(s.tabs().1, None));
}

#[gpui::test]
fn a_server_is_added_with_its_token(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let tokens = Arc::new(MemoryTokens::default());
    let places = cx.new(|cx| Places::offline(tokens.clone(), cx));
    places.update(cx, |p, cx| {
        assert!(p.add_server("not an address", "hgr_x", cx).is_err());
        assert!(p.add_server("hangar.example.test", " ", cx).is_err());
        let id = p.add_server("http://127.0.0.1:9/", " hgr_x ", cx).unwrap();
        assert_eq!(id, "127.0.0.1:9");
        let (config, state) = p.get(&id).unwrap();
        assert_eq!(config.url, "http://127.0.0.1:9");
        assert_eq!(state.connection, Connection::Connecting);
    });
    assert_eq!(tokens.get("127.0.0.1:9").as_deref(), Some("hgr_x"));
}

/// A tool window opens in its side's slot, replacing the one there, and
/// closes when opened again; the groups below the line have slots of their
/// own.
#[gpui::test]
fn tool_windows_share_their_slots(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let (shell, cx) = cx.add_window_view(|window, cx| Shell::new(places.clone(), window, cx));
    cx.update(|window, cx| shell.update(cx, |s, cx| s.open("p", "e1", window, cx)));
    let tab = shell.read_with(cx, |s, _| s.active_view()).unwrap();
    tab.update(cx, |t, cx| {
        assert!(t.is_open(Tool::Project) && t.is_open(Tool::Terminal) && t.is_open(Tool::Agents));
        t.toggle(Tool::Commit, cx);
        assert!(t.is_open(Tool::Commit) && !t.is_open(Tool::Project));
        t.toggle(Tool::Structure, cx);
        assert!(t.is_open(Tool::Commit) && t.is_open(Tool::Structure));
        t.toggle(Tool::Git, cx);
        assert!(t.is_open(Tool::Git) && !t.is_open(Tool::Terminal));
        t.toggle(Tool::Git, cx);
        assert!(!t.is_open(Tool::Git));
    });
}
