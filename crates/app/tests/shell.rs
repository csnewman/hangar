//! The window's tabs, and adding a server place.

use std::sync::Arc;

use gpui::{AppContext, TestAppContext};
use hangar_app::envtab::{Tool, moved};
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

/// Search Everywhere opens on ⌘K, and on Shift pressed twice, though not
/// on a capital typed between.
#[gpui::test]
fn search_everywhere_opens_on_its_keys(cx: &mut TestAppContext) {
    use gpui::Modifiers;
    cx.update(gpui_component::init);
    cx.update(hangar_app::shell::bind_keys);
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let (shell, cx) = cx.add_window_view(|window, cx| Shell::new(places.clone(), window, cx));
    let shift = |down: bool| Modifiers {
        shift: down,
        ..Default::default()
    };
    cx.simulate_modifiers_change(shift(true));
    cx.simulate_modifiers_change(shift(false));
    cx.simulate_modifiers_change(shift(true));
    cx.simulate_modifiers_change(shift(false));
    shell.read_with(cx, |s, _| assert!(s.search().is_some(), "Shift twice"));

    cx.simulate_keystrokes("escape");
    shell.read_with(cx, |s, _| assert!(s.search().is_none(), "Escape leaves"));

    cx.simulate_modifiers_change(shift(true));
    cx.simulate_keystrokes("shift-a");
    cx.simulate_modifiers_change(shift(false));
    cx.simulate_modifiers_change(shift(true));
    shell.read_with(cx, |s, _| {
        assert!(s.search().is_none(), "a capital between")
    });
    cx.simulate_modifiers_change(shift(false));

    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-k"
    } else {
        "ctrl-k"
    });
    shell.read_with(cx, |s, _| assert!(s.search().is_some(), "⌘K"));
}

/// Go to Symbol is the search on symbols alone, on IntelliJ's key.
#[gpui::test]
fn go_to_symbol_opens_on_its_key(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(hangar_app::shell::bind_keys);
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let (shell, cx) = cx.add_window_view(|window, cx| Shell::new(places.clone(), window, cx));
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "alt-cmd-o"
    } else {
        "ctrl-alt-shift-n"
    });
    let search = shell
        .read_with(cx, |s, _| s.search())
        .expect("the search open");
    assert_eq!(search.read_with(cx, |s, _| s.only()), Some("Symbols"));
}

/// A tab dragged onto another takes its place; the one shown stays shown.
#[gpui::test]
fn a_tab_is_dragged_to_another_place(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let (shell, cx) = cx.add_window_view(|window, cx| Shell::new(places.clone(), window, cx));
    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            for e in ["e1", "e2", "e3"] {
                s.open("p", e, window, cx);
            }
        })
    });
    cx.run_until_parked();
    let from = cx.debug_bounds("env-tab-0").expect("the first").center();
    let to = cx.debug_bounds("env-tab-2").expect("the third").center();
    let none = gpui::Modifiers::none();
    let left = gpui::MouseButton::Left;
    cx.simulate_mouse_down(from, left, none);
    cx.simulate_mouse_move(from + gpui::point(gpui::px(10.), gpui::px(0.)), left, none);
    cx.simulate_mouse_move(to, left, none);
    cx.simulate_mouse_up(to, left, none);
    cx.run_until_parked();
    shell.read_with(cx, |s, _| {
        let (tabs, active) = s.tabs();
        let order: Vec<&str> = tabs.iter().map(|o| o.env.as_str()).collect();
        assert_eq!(order, ["e2", "e3", "e1"]);
        assert_eq!(active, Some(1), "e3, still shown");
    });
}

#[test]
fn an_index_follows_its_item() {
    // Moving the item at 1 to 3 in [a, b, c, d]: [a, c, d, b].
    assert_eq!(
        (0..4).map(|i| moved(i, 1, 3)).collect::<Vec<_>>(),
        [0, 3, 1, 2]
    );
    // And back.
    assert_eq!(
        (0..4).map(|i| moved(i, 3, 1)).collect::<Vec<_>>(),
        [0, 2, 3, 1]
    );
}

/// The title bar beside the tabs moves the window when pressed and moved,
/// as a title bar does; gpui's test window, which cannot move, says so by
/// panicking when asked to.
#[gpui::test]
#[should_panic(expected = "not implemented")]
fn the_strip_beside_the_tabs_moves_the_window(cx: &mut TestAppContext) {
    use gpui::{Modifiers, MouseButton, point, px};
    cx.update(gpui_component::init);
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let (shell, cx) = cx.add_window_view(|window, cx| Shell::new(places.clone(), window, cx));
    cx.update(|window, cx| shell.update(cx, |s, cx| s.open("p", "e1", window, cx)));
    cx.run_until_parked();
    let strip = cx.debug_bounds("tab-strip").expect("the tab strip");
    let tab = cx.debug_bounds("env-tab-0").expect("the tab");
    assert!(strip.right() - tab.right() > px(40.), "room beside the tab");
    let at = point(strip.right() - px(20.), strip.center().y);
    cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(
        at + point(px(8.), px(0.)),
        MouseButton::Left,
        Modifiers::none(),
    );
}

/// A tab pressed and moved is the tab's, to drag among the others; the
/// window stays where it is.
#[gpui::test]
fn a_tab_pressed_and_moved_leaves_the_window(cx: &mut TestAppContext) {
    use gpui::{Modifiers, MouseButton, point, px};
    cx.update(gpui_component::init);
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let (shell, cx) = cx.add_window_view(|window, cx| Shell::new(places.clone(), window, cx));
    cx.update(|window, cx| shell.update(cx, |s, cx| s.open("p", "e1", window, cx)));
    cx.run_until_parked();
    let at = cx.debug_bounds("env-tab-0").expect("the tab").center();
    cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(
        at + point(px(8.), px(0.)),
        MouseButton::Left,
        Modifiers::none(),
    );
    cx.simulate_mouse_up(
        at + point(px(8.), px(0.)),
        MouseButton::Left,
        Modifiers::none(),
    );
}
