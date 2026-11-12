//! Environment Settings: what can change when, what a change is called,
//! and the window itself, opened on ⌘; over an environment's tab.

use std::sync::Arc;

use chrono::Utc;
use gpui::{AppContext, TestAppContext, VisualTestContext};
use hangar_app::api::{Details, Environment, MachineSettings, Stats};
use hangar_app::envsettings::{Page, changeable, changes, live_memory, memory, parse_gib, saved};
use hangar_app::place::Update;
use hangar_app::places::Places;
use hangar_app::shell::Shell;
use hangar_app::store::MemoryTokens;

fn env(phase: &str, desired: &str) -> Environment {
    Environment {
        id: "e1".into(),
        short_id: "abc123".into(),
        name: "ledger-1".into(),
        owner: "alice".into(),
        template: "t".into(),
        phase: phase.into(),
        desired: desired.into(),
        reason: None,
        progress: None,
        worker: Some("w1".into()),
        worker_online: Some(true),
        cpus: Some(2),
        memory_mib: Some(4096),
        editor_path: Some("/home/dev".into()),
        ports_public: false,
        display: Some("desktop".into()),
        image_update: None,
        web_names: vec![],
        details: Details {
            gpu: Some("virtual".into()),
            swap_mib: None,
            ..Default::default()
        },
        task: None,
        agents: vec![],
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn stats(used: i64, min: Option<i64>, max: Option<i64>) -> Stats {
    Stats {
        memory_used_mib: used,
        machine_memory_min_mib: min,
        machine_memory_max_mib: max,
        ..Default::default()
    }
}

#[test]
fn a_stopped_machine_changes_and_a_running_ones_memory_alone() {
    assert!(changeable(&env("stopped", "stopped")));
    assert!(!changeable(&env("running", "running")));
    assert!(!changeable(&env("suspended", "suspended")));
    let mut placed = env("pending", "running");
    placed.worker = None;
    assert!(changeable(&placed), "not yet placed");

    let running = env("running", "running");
    assert_eq!(
        live_memory(&running, Some(&stats(1000, Some(1024), Some(16384)))),
        Some((1512, 16384)),
        "no lower than what its programs use plus 512 MiB"
    );
    assert_eq!(
        live_memory(&running, Some(&stats(100, Some(1024), Some(16384)))),
        Some((1024, 16384)),
        "no lower than it booted with"
    );
    assert_eq!(
        live_memory(&running, Some(&stats(100, None, None))),
        None,
        "a machine that cannot be resized"
    );
    assert_eq!(
        live_memory(
            &env("stopped", "stopped"),
            Some(&stats(0, Some(1024), Some(16384)))
        ),
        None
    );
    let mut away = env("running", "running");
    away.worker_online = Some(false);
    assert_eq!(
        live_memory(&away, Some(&stats(100, Some(1024), Some(16384)))),
        None
    );
}

#[test]
fn changes_are_said_as_the_web_says_them() {
    let from = saved(&env("stopped", "stopped"));
    assert_eq!(
        from,
        MachineSettings {
            cpus: 2,
            memory_mib: 4096,
            display: "desktop".into(),
            gpu: "virtual".into(),
            dax: false,
            swap_mib: None,
        }
    );
    let to = MachineSettings {
        cpus: 4,
        memory_mib: 6144,
        display: "none".into(),
        swap_mib: Some(0),
        ..from.clone()
    };
    assert_eq!(
        changes(&from, &to),
        [
            "vCPUs 2 → 4",
            "memory 4 GB → 6 GB",
            "display desktop → headless",
            "swap the worker's default → none",
        ]
    );
    assert!(changes(&from, &from).is_empty());
}

#[test]
fn sizes_are_read_and_written_in_gb() {
    assert_eq!(parse_gib("4"), Some(4096));
    assert_eq!(parse_gib("1.5 GB"), Some(1536));
    assert_eq!(parse_gib("2GiB"), Some(2048));
    assert_eq!(parse_gib("0"), None);
    assert_eq!(parse_gib("lots"), None);
    assert_eq!(memory(512), "512 MB");
    assert_eq!(memory(4096), "4 GB");
    assert_eq!(memory(1536), "1.5 GB");
}

#[test]
fn a_search_finds_pages_by_what_is_on_them() {
    let found = |q: &str| -> Vec<Page> { Page::ALL.into_iter().filter(|p| p.matches(q)).collect() };
    assert_eq!(found(""), Page::ALL);
    assert_eq!(found("swap"), [Page::Resources]);
    assert_eq!(found("delete"), [Page::Danger]);
    assert_eq!(found("ssh"), [Page::Access]);
    assert_eq!(found("ports"), [Page::Ports]);
}

/// A window on a stopped environment of a place that never connects.
fn open(cx: &mut TestAppContext) -> (gpui::Entity<Shell>, &mut VisualTestContext) {
    cx.update(gpui_component::init);
    cx.update(hangar_app::envtab::bind_keys);
    cx.update(hangar_app::shell::bind_keys);
    cx.update(hangar_app::envsettings::bind_keys);
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let id = places.update(cx, |p, cx| {
        let id = p.add_server("http://127.0.0.1:9", "hgr_t", cx).unwrap();
        p.apply(&id, Update::Snapshot(vec![env("stopped", "stopped")]), cx);
        id
    });
    let (shell, cx) = cx.add_window_view(|window, cx| Shell::new(places.clone(), window, cx));
    cx.update(|window, cx| shell.update(cx, |s, cx| s.open(&id, "e1", window, cx)));
    cx.run_until_parked();
    (shell, cx)
}

#[gpui::test]
fn its_key_opens_it_on_general_and_a_change_is_drafted(cx: &mut TestAppContext) {
    let (shell, cx) = open(cx);
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-;"
    } else {
        "ctrl-alt-shift-s"
    });
    cx.run_until_parked();
    let view = shell
        .read_with(cx, |s, _| s.env_settings())
        .expect("Environment Settings open");
    assert_eq!(view.read_with(cx, |v, _| v.page()), Page::General);

    let node = cx
        .debug_bounds("env-settings-node-resources")
        .expect("the tree's Resources");
    cx.simulate_click(node.center(), gpui::Modifiers::none());
    assert_eq!(view.read_with(cx, |v, _| v.page()), Page::Resources);

    // Headless, drafted until Apply.
    let headless = cx.debug_bounds("display-none").expect("Headless");
    cx.simulate_click(headless.center(), gpui::Modifiers::none());
    let pending = cx.update(|_, cx| view.read(cx).pending(cx));
    assert_eq!(pending, ["display desktop → headless"]);

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(
        shell.read_with(cx, |s, _| s.env_settings()).is_none(),
        "Escape closes it, nothing applied"
    );
}

/// While it runs, its memory is a slider between what it can be resized
/// between, starting at what it has; the rest wait for it to stop.
#[gpui::test]
fn a_running_ones_memory_is_a_slider(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(hangar_app::shell::bind_keys);
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let id = places.update(cx, |p, cx| {
        let id = p.add_server("http://127.0.0.1:9", "hgr_t", cx).unwrap();
        p.apply(&id, Update::Snapshot(vec![env("running", "running")]), cx);
        id
    });
    let (shell, cx) = cx.add_window_view(|window, cx| Shell::new(places.clone(), window, cx));
    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.open_env_settings(&id, "e1", Page::Resources, window, cx)
        })
    });
    let view = shell.read_with(cx, |s, _| s.env_settings()).unwrap();
    cx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.take_stats(Some(stats(519, Some(1031), Some(16384))), cx)
        })
    });
    cx.run_until_parked();
    let draft = cx.update(|_, cx| view.read(cx).draft(cx)).unwrap();
    assert_eq!(draft.memory_mib, 4096, "the memory it has");
    assert!(
        cx.debug_bounds("display-none").is_some(),
        "the rest shown, to change once it stops"
    );
    assert!(cx.update(|_, cx| view.read(cx).pending(cx)).is_empty());
}
