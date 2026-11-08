//! The panels' sizes: dragged by their edges within limits, reset, kept
//! with the stripes' arrangement, and the tool windows put away and brought
//! back.

use std::sync::Arc;

use gpui::{AppContext, Modifiers, MouseButton, TestAppContext, point, px, size};
use hangar_app::envtab::Tool;
use hangar_app::layout::{Edge, Layout, Sizes};
use hangar_app::places::Places;
use hangar_app::shell::Shell;
use hangar_app::store::MemoryTokens;
use hangar_app::tools::{Group, Stripes};

#[test]
fn sizes_stay_within_limits() {
    let mut s = Sizes::default();
    s.set(Edge::Left, 10.);
    assert_eq!(s.left, 150.);
    s.set(Edge::Bottom, 5000.);
    assert_eq!(s.bottom, 900.);
    s.set(Edge::Right, 333.4);
    assert_eq!(s.right, 333.);
    s.reset(Edge::Right);
    assert_eq!(s.right, Sizes::default().right);

    // A split is a fraction of its side, kept off its ends.
    assert_eq!(s.left_split, 0.5);
    s.set(Edge::LeftSplit, 0.05);
    assert_eq!(s.left_split, 0.15);
    s.set(Edge::RightSplit, 0.6666);
    assert_eq!(s.right_split, 0.667);

    let area = gpui::Bounds::new(point(px(100.), px(50.)), size(px(1000.), px(600.)));
    assert_eq!(
        Sizes::dragged(Edge::Left, point(px(400.), px(0.)), area, 38.),
        262.
    );
    assert_eq!(
        Sizes::dragged(Edge::Right, point(px(800.), px(0.)), area, 38.),
        262.
    );
    assert_eq!(
        Sizes::dragged(Edge::Bottom, point(px(0.), px(400.)), area, 24.),
        226.
    );
    assert_eq!(
        Sizes::dragged(Edge::LeftSplit, point(px(0.), px(200.)), area, 0.),
        0.25
    );
}

/// Dragging the sidebar's edge makes it as wide as where it is let go.
#[gpui::test]
fn the_sidebar_is_dragged_wider(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        Layout::init(None, cx);
    });
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let (_shell, cx) = cx.add_window_view(|window, cx| Shell::new(places.clone(), window, cx));
    cx.simulate_resize(size(px(1200.), px(800.)));
    cx.run_until_parked();
    let edge = cx.update(|_, cx| Layout::sizes(cx).sidebar);
    let y = px(400.);
    cx.simulate_mouse_move(point(px(edge - 2.), y), None, Modifiers::none());
    cx.simulate_mouse_down(
        point(px(edge - 2.), y),
        MouseButton::Left,
        Modifiers::none(),
    );
    for x in [edge + 10., edge + 40., 330.] {
        cx.simulate_mouse_move(point(px(x), y), MouseButton::Left, Modifiers::none());
    }
    cx.simulate_mouse_up(point(px(330.), y), MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| Layout::sizes(cx).sidebar), 330.);
}

/// Hide All puts every tool window away and brings the same ones back;
/// Hide Active hides the one last opened.
#[gpui::test]
fn tool_windows_are_hidden_and_brought_back(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let (shell, cx) = cx.add_window_view(|window, cx| Shell::new(places.clone(), window, cx));
    cx.update(|window, cx| shell.update(cx, |s, cx| s.open("p", "e1", window, cx)));
    let tab = shell.read_with(cx, |s, _| s.active_view()).unwrap();
    tab.update(cx, |t, cx| {
        t.hide_all(cx);
        assert!(
            !t.is_open(Tool::Project) && !t.is_open(Tool::Terminal) && !t.is_open(Tool::Agents)
        );
        t.hide_all(cx);
        assert!(t.is_open(Tool::Project) && t.is_open(Tool::Terminal) && t.is_open(Tool::Agents));
        t.toggle(Tool::Git, cx);
        t.hide_active(cx);
        assert!(!t.is_open(Tool::Git) && t.is_open(Tool::Project));
    });
}

/// A layout file from before the stripes were kept reads with them where
/// they start; a change is written a moment later and read back.
#[gpui::test]
fn the_layout_is_kept_with_the_stripes(cx: &mut TestAppContext) {
    let dir = std::env::temp_dir().join(format!("hangar-app-layout-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("layout.json");
    std::fs::write(&file, r#"{"sidebar": 300}"#).unwrap();
    cx.update(|cx| Layout::init(Some(file.clone()), cx));
    cx.update(|cx| {
        assert_eq!(Layout::sizes(cx).sidebar, 300.);
        assert_eq!(Layout::sizes(cx).left, Sizes::default().left);
        assert_eq!(Layout::stripes(cx), Stripes::default());
        Layout::arrange(cx, |s| {
            s.moved(Tool::Git, Group::RightFoot, None);
        });
    });
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(600));
    cx.run_until_parked();
    cx.update(|cx| Layout::init(Some(file.clone()), cx));
    cx.update(|cx| {
        assert_eq!(Layout::sizes(cx).sidebar, 300.);
        assert_eq!(Layout::stripes(cx).group(Group::RightFoot), [Tool::Git]);
    });
    let _ = std::fs::remove_dir_all(&dir);
}
