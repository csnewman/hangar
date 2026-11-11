//! The stripes' icons: dragged between groups and sides, their tool windows
//! moving with them, and the arrangement kept as names.

use std::sync::Arc;

use chrono::Utc;
use gpui::{AppContext, Modifiers, MouseButton, TestAppContext, point, px, size};
use hangar_app::api::Environment;
use hangar_app::layout::Layout;
use hangar_app::place::Update;
use hangar_app::places::Places;
use hangar_app::shell::Shell;
use hangar_app::store::MemoryTokens;
use hangar_app::tools::{Group, Slot, Stripes, Tool};

#[test]
fn tools_start_where_intellij_puts_them() {
    let s = Stripes::default();
    assert_eq!(
        s.group(Group::LeftTop),
        [Tool::Project, Tool::Filesystem, Tool::Commit]
    );
    assert_eq!(s.group(Group::LeftBelow), [Tool::Structure]);
    assert_eq!(s.group(Group::RightTop), [Tool::Agents]);
    assert_eq!(s.group(Group::RightBelow), [Tool::Ports]);
    assert!(s.group(Group::RightFoot).is_empty());
    assert_eq!(s.slot(Tool::Git), Slot::Bottom);
    assert!(!s.side_empty(false));
}

#[test]
fn a_tool_moves_before_another_or_to_a_group_end() {
    let mut s = Stripes::default();
    assert!(s.moved(Tool::Commit, Group::LeftTop, Some(Tool::Project)));
    assert_eq!(
        s.group(Group::LeftTop),
        [Tool::Commit, Tool::Project, Tool::Filesystem]
    );

    assert!(s.moved(Tool::Git, Group::RightFoot, None));
    assert_eq!(s.group(Group::RightFoot), [Tool::Git]);
    assert_eq!(s.slot(Tool::Git), Slot::Bottom);
    assert!(!s.group(Group::LeftFoot).contains(&Tool::Git));

    assert!(s.moved(Tool::Structure, Group::RightTop, Some(Tool::Agents)));
    assert_eq!(s.group(Group::RightTop), [Tool::Structure, Tool::Agents]);
    assert_eq!(s.slot(Tool::Structure), Slot::Right);
    assert!(s.group(Group::LeftBelow).is_empty());

    // Onto itself, or to where it is already, nothing moves.
    assert!(!s.moved(Tool::Agents, Group::RightTop, Some(Tool::Agents)));
    assert!(!s.moved(Tool::Agents, Group::RightTop, None));

    // Emptied, the right stripe goes.
    for t in [Tool::Structure, Tool::Agents, Tool::Ports, Tool::Git] {
        s.moved(t, Group::LeftFoot, None);
    }
    assert!(s.side_empty(false));
}

#[test]
fn the_arrangement_is_kept_by_name() {
    let mut s = Stripes::default();
    s.moved(Tool::Docker, Group::RightBelow, None);
    let kept = serde_json::to_value(&s).unwrap();
    assert_eq!(kept["right_below"], serde_json::json!(["Ports", "Docker"]));
    let back: Stripes = serde_json::from_value(kept).unwrap();
    assert_eq!(back, s);

    // A name no tool has is dropped, a tool named twice keeps its first
    // place, and a tool not named goes back where it starts.
    let read: Stripes = serde_json::from_value(serde_json::json!({
        "left": ["Commit", "Gone"],
        "right": ["Terminal", "Commit"],
    }))
    .unwrap();
    assert_eq!(
        read.group(Group::LeftTop),
        [Tool::Commit, Tool::Project, Tool::Filesystem]
    );
    assert_eq!(read.group(Group::RightTop), [Tool::Terminal, Tool::Agents]);
    assert_eq!(read.slot(Tool::Git), Slot::Bottom);
    for t in Tool::ALL {
        let holding = Group::ALL
            .iter()
            .filter(|g| read.group(**g).contains(&t))
            .count();
        assert_eq!(holding, 1, "{t:?}");
    }
}

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
        web_names: vec![],
        task: None,
        agents: vec![],
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

/// Structure's icon, dragged onto Agents' on the right stripe, goes before
/// it, and its open window moves to the right side with it.
#[gpui::test]
fn an_icon_is_dragged_to_the_other_side(cx: &mut TestAppContext) {
    // The environment's connections run on threads of their own.
    cx.executor().allow_parking();
    cx.update(|cx| {
        gpui_component::init(cx);
        Layout::init(None, cx);
    });
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let id = places.update(cx, |p, cx| {
        let id = p.add_server("http://127.0.0.1:9", "hgr_t", cx).unwrap();
        p.apply(&id, Update::Snapshot(vec![running()]), cx);
        id
    });
    let (shell, cx) = cx.add_window_view(|window, cx| Shell::new(places.clone(), window, cx));
    cx.simulate_resize(size(px(1400.), px(900.)));
    cx.update(|window, cx| shell.update(cx, |s, cx| s.open(&id, "e1", window, cx)));
    let tab = shell.read_with(cx, |s, _| s.active_view()).unwrap();
    tab.update(cx, |t, cx| t.toggle(Tool::Structure, cx));
    cx.run_until_parked();

    let from = cx.debug_bounds("stripe-Structure").unwrap().center();
    let to = cx.debug_bounds("stripe-Agents").unwrap().center();
    cx.simulate_mouse_move(from, None, Modifiers::none());
    cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    for i in 1..=8 {
        let f = i as f32 / 8.;
        let at = point(from.x + (to.x - from.x) * f, from.y + (to.y - from.y) * f);
        cx.simulate_mouse_move(at, MouseButton::Left, Modifiers::none());
    }
    cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();

    let stripes = cx.update(|_, cx| Layout::stripes(cx));
    assert_eq!(
        stripes.group(Group::RightTop),
        [Tool::Structure, Tool::Agents]
    );
    assert!(stripes.group(Group::LeftBelow).is_empty());
    tab.read_with(cx, |t, _| assert!(t.is_open(Tool::Structure)));
    // Toggled again, it closes from the right side, where it is.
    tab.update(cx, |t, cx| t.toggle(Tool::Structure, cx));
    tab.read_with(cx, |t, _| assert!(!t.is_open(Tool::Structure)));

    // Let go on the empty space below the line, it goes to that group's end.
    let from = cx.debug_bounds("stripe-Git").unwrap().center();
    let to = cx.debug_bounds("stripe-end-RightBelow").unwrap().center();
    cx.simulate_mouse_move(from, None, Modifiers::none());
    cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    for i in 1..=8 {
        let f = i as f32 / 8.;
        let at = point(from.x + (to.x - from.x) * f, from.y + (to.y - from.y) * f);
        cx.simulate_mouse_move(at, MouseButton::Left, Modifiers::none());
    }
    cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    let stripes = cx.update(|_, cx| Layout::stripes(cx));
    assert_eq!(stripes.group(Group::RightBelow), [Tool::Ports, Tool::Git]);
}
