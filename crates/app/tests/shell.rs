//! The window's tabs, and adding a server place.

use std::sync::Arc;

use gpui::{AppContext, TestAppContext};
use hangar_app::place::Connection;
use hangar_app::places::Places;
use hangar_app::shell::Shell;
use hangar_app::store::{MemoryTokens, Tokens};

#[gpui::test]
fn an_environment_opens_in_one_tab(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let places = cx.new(|cx| Places::new(None, Arc::new(MemoryTokens::default()), vec![], cx));
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
    let places = cx.new(|cx| Places::new(None, tokens.clone(), vec![], cx));
    places.update(cx, |p, cx| {
        assert!(p.add_server("not an address", "hgr_x", cx).is_err());
        assert!(p.add_server("hangar.example.test", " ", cx).is_err());
        // Nothing listens there: it is added, and tries.
        let id = p.add_server("http://127.0.0.1:9/", " hgr_x ", cx).unwrap();
        assert_eq!(id, "127.0.0.1:9");
        let (config, state) = p.get(&id).unwrap();
        assert_eq!(config.url, "http://127.0.0.1:9");
        assert_eq!(state.connection, Connection::Connecting);
    });
    assert_eq!(tokens.get("127.0.0.1:9").as_deref(), Some("hgr_x"));
}
