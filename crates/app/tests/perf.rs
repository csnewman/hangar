use hangar_app::api::{ActivityEvent, Actor, Process, Target};
use hangar_app::perf::{Sort, describe, notable, ordered};
use serde_json::json;

fn proc(pid: i64, ppid: i64, name: &str, cpu: f64, rss: i64) -> Process {
    Process {
        pid,
        ppid,
        name: name.into(),
        cpu_percent: cpu,
        rss_bytes: rss,
        ..Default::default()
    }
}

#[test]
fn processes_are_ordered_by_what_they_use_or_as_a_tree() {
    let list = [
        proc(1, 0, "init", 0., 10),
        proc(40, 1, "node", 50., 300),
        proc(41, 40, "worker", 80., 100),
        proc(50, 1, "bash", 1., 200),
    ];
    let pids = |o: Vec<(usize, usize)>| -> Vec<(i64, usize)> {
        o.into_iter().map(|(i, d)| (list[i].pid, d)).collect()
    };
    assert_eq!(
        pids(ordered(&list, Sort::Cpu, false)),
        [(41, 0), (40, 0), (50, 0), (1, 0)]
    );
    assert_eq!(
        pids(ordered(&list, Sort::Memory, false)),
        [(40, 0), (50, 0), (41, 0), (1, 0)]
    );
    // Each under the one that started it, siblings in the order asked.
    assert_eq!(
        pids(ordered(&list, Sort::Cpu, true)),
        [(1, 0), (40, 1), (41, 2), (50, 1)]
    );
    assert_eq!(
        pids(ordered(&list, Sort::Name, true)),
        [(1, 0), (50, 1), (40, 1), (41, 2)]
    );
}

fn event(action: &str, details: serde_json::Value) -> ActivityEvent {
    ActivityEvent {
        id: 1,
        at: chrono::Utc::now(),
        actor: Actor {
            kind: "user".into(),
            id: None,
            name: "alice".into(),
        },
        action: action.into(),
        target: Target {
            kind: "environment".into(),
            id: "e1".into(),
            name: "dev".into(),
        },
        details,
    }
}

#[test]
fn activity_is_told_in_words_and_looks_are_left_out() {
    assert_eq!(
        describe(&event("environment.start", json!({ "from": "suspended" }))),
        ("resumed it".to_string(), None)
    );
    assert_eq!(
        describe(&event(
            "environment.update_settings",
            json!({ "memory_mib": { "from": 2048, "to": 4096 }, "cpus": { "from": 2, "to": 2 } })
        )),
        (
            "changed its settings".to_string(),
            Some("memory 2 GB → 4 GB".to_string())
        )
    );
    assert_eq!(
        describe(&event(
            "environment.signal_process",
            json!({ "signal": "KILL", "pid": 42, "error": "" })
        )),
        ("sent KILL to process 42".to_string(), None)
    );
    assert_eq!(
        describe(&event("environment.something_new", json!({}))).0,
        "something new"
    );
    assert!(!notable(&event("environment.terminal_attach", json!({}))));
    assert!(!notable(&event(
        "environment.phase",
        json!({ "to": "running" })
    )));
    assert!(notable(&event(
        "environment.phase",
        json!({ "to": "failed" })
    )));
    assert!(notable(&event("environment.stop", json!({}))));
}
