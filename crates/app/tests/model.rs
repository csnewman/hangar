//! Home's cards and the sidebar's rows: what each environment's state shows,
//! and what needs the user coming first.

use chrono::{DateTime, Duration, Utc};
use hangar_app::api::{AgentSession, Environment, StartProgress, Task};
use hangar_app::model::{self, Action, LineKind, Rank, Tone};
use hangar_app::place::{Connection, PlaceConfig, PlaceState};

fn now() -> DateTime<Utc> {
    "2026-11-07T12:00:00Z".parse().unwrap()
}

fn env(name: &str, phase: &str) -> Environment {
    Environment {
        id: format!("id-{name}"),
        short_id: "abc123".into(),
        name: name.into(),
        owner: "alice".into(),
        template: "t".into(),
        phase: phase.into(),
        desired: "running".into(),
        reason: None,
        progress: None,
        worker: None,
        worker_online: Some(true),
        cpus: None,
        memory_mib: None,
        task: None,
        agents: vec![],
        created_at: now() - Duration::days(3),
        updated_at: now() - Duration::hours(2),
    }
}

fn agent(id: &str, state: &str, needs_you: bool, ago: i64) -> AgentSession {
    AgentSession {
        id: id.into(),
        agent: "claude".into(),
        terminal: None,
        state: state.into(),
        step: Some("Edit src/main.go".into()),
        message: Some("Claude needs your permission to use Bash".into()),
        since: now() - Duration::minutes(ago),
        needs_you,
    }
}

fn place() -> PlaceConfig {
    PlaceConfig {
        id: "p".into(),
        name: "Acme".into(),
        url: "https://hangar.acme.test".into(),
    }
}

#[test]
fn each_state_is_a_card() {
    let mut asking = env("storefront-2", "running");
    asking.task = Some(Task {
        id: "t".into(),
        text: "Fix rounding in checkout totals".into(),
        set_by: "agent".into(),
        set_at: now(),
        done_at: None,
    });
    asking.agents = vec![
        agent("a", "asking", true, 3),
        agent("b", "working", false, 1),
    ];
    let c = model::card(&place(), &asking, now());
    assert_eq!(c.title, "Fix rounding in checkout totals");
    assert_eq!(c.subtitle, "storefront-2");
    assert!(c.agent_task);
    assert_eq!(
        (c.rank, c.tone, c.action),
        (Rank::Asking, Tone::Ask, Some(Action::Answer))
    );
    assert_eq!(c.status, "Asking · 3m");
    assert_eq!(c.lines[0].kind, LineKind::Question);
    assert_eq!(c.lines.last().unwrap().text, "1 more session");
    assert_eq!(c.session.as_deref(), Some("a"));

    let mut finished = env("ledger-1", "running");
    finished.agents = vec![agent("a", "finished", true, 12)];
    let c = model::card(&place(), &finished, now());
    assert_eq!(
        (c.title.as_str(), c.rank, c.action),
        ("ledger-1", Rank::Finished, Some(Action::Review))
    );
    assert_eq!(
        c.subtitle, "t",
        "without a task, the template is under its name"
    );
    assert_eq!(c.status, "Finished · 12m");

    let mut working = env("auth-1", "running");
    working.agents = vec![agent("a", "working", false, 14)];
    let c = model::card(&place(), &working, now());
    assert_eq!(
        (c.rank, c.status.as_str(), c.lines[0].text.as_str()),
        (Rank::Working, "Claude working · 14m", "Edit src/main.go")
    );
    assert_eq!(c.action, None);

    // Seen, a finished session is idle: the environment is just running.
    let mut idle = env("docs-1", "running");
    idle.agents = vec![agent("a", "idle", false, 30)];
    let c = model::card(&place(), &idle, now());
    assert_eq!((c.rank, c.action), (Rank::Running, Some(Action::Suspend)));
    assert_eq!(c.lines[0].text, "Claude is waiting for a prompt");
    assert_eq!(
        model::card(&place(), &env("bare", "running"), now()).lines[0].text,
        "No agent running"
    );

    let mut failed = env("mobile-1", "failed");
    failed.reason = Some("No worker has room for 16 vCPUs".into());
    let c = model::card(&place(), &failed, now());
    assert_eq!(
        (c.rank, c.tone, c.lines[0].text.as_str()),
        (
            Rank::Failed,
            Tone::Failed,
            "No worker has room for 16 vCPUs"
        )
    );

    let mut starting = env("search-1", "starting");
    starting.progress = Some(StartProgress {
        step: "boot".into(),
        done: None,
        total: None,
        unit: None,
    });
    assert_eq!(
        model::card(&place(), &starting, now()).lines[0].text,
        "Booting"
    );
    let c = model::card(&place(), &env("mobile-2", "suspended"), now());
    assert_eq!(
        (c.rank, c.action, c.status.as_str()),
        (Rank::Suspended, Some(Action::Resume), "Suspended · 2h")
    );
    assert_eq!(
        model::card(&place(), &env("x", "stopped"), now()).action,
        Some(Action::Start)
    );
    assert_eq!(
        model::card(&place(), &env("x", "stopping"), now()).status,
        "Stopping · 2h"
    );
}

#[test]
fn what_needs_you_comes_first() {
    let mut a = env("a-running", "running");
    a.agents = vec![agent("s", "idle", false, 1)];
    let b = env("b-suspended", "suspended");
    let mut c = env("c-asking", "running");
    c.agents = vec![agent("s", "asking", true, 1)];
    let mut d = env("d-finished", "running");
    d.agents = vec![agent("s", "finished", true, 1)];
    let e = env("e-failed", "failed");
    let state = PlaceState {
        connection: Connection::Online,
        environments: vec![a, b, c, d, e],
        ..Default::default()
    };
    let places = vec![(place(), state)];

    let home: Vec<String> = model::home(&places, now())
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(
        home,
        [
            "c-asking",
            "d-finished",
            "e-failed",
            "a-running",
            "b-suspended"
        ]
    );

    let groups = model::sidebar(&places, now());
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].name, "Acme");
    let rows: Vec<String> = groups[0].rows.iter().map(|c| c.name.clone()).collect();
    // Needing you, most first; then the rest by name.
    assert_eq!(
        rows,
        [
            "c-asking",
            "d-finished",
            "e-failed",
            "a-running",
            "b-suspended"
        ]
    );
}

#[test]
fn ages() {
    for (secs, want) in [
        (5, "now"),
        (125, "2m"),
        (7300, "2h"),
        (90000, "yesterday"),
        (400000, "4d"),
    ] {
        assert_eq!(model::age(now() - Duration::seconds(secs), now()), want);
    }
}
