//! Home's cards and the sidebar's rows: what each environment's state shows,
//! and what needs the user coming first.

use chrono::{DateTime, Duration, Utc};
use hangar_app::api::{AgentSession, Environment, StartProgress, Task};
use hangar_app::model::{self, Action, LineKind, Mark, Rank, Tone};
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
        editor_path: None,
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
        (Rank::Asking, Tone::Wait, Some(Action::Answer))
    );
    assert_eq!(c.status, "Asking · 3m");
    assert_eq!(c.lines[0].kind, LineKind::Question);
    assert_eq!(c.lines[0].text, "Claude asks: edit src/main.go?");
    assert_eq!(c.lines[1].text, "Claude needs your permission to use Bash");
    assert_eq!(c.summary, "Claude is asking: edit src/main.go? · 3m");
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
    assert_eq!(c.summary, "Claude · edit src/main.go");
    assert_eq!(c.action, None);

    // Seen, an asking session still asks, but does not need the user.
    let mut seen = env("pay-1", "running");
    seen.agents = vec![agent("a", "asking", false, 4)];
    let c = model::card(&place(), &seen, now());
    assert_eq!((c.rank, c.tone), (Rank::Working, Tone::Wait));
    assert_eq!(c.status, "Asking · 4m");
    assert!(!c.rank.needs_you());

    // Seen, a finished session is idle: the environment is just running.
    let mut idle = env("docs-1", "running");
    idle.agents = vec![agent("a", "idle", false, 30)];
    let c = model::card(&place(), &idle, now());
    assert_eq!(
        (c.rank, c.tone, c.action),
        (Rank::Running, Tone::Good, Some(Action::Suspend))
    );
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
        (Rank::Failed, Tone::Bad, "No worker has room for 16 vCPUs")
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
        (c.rank, c.tone, c.action),
        (Rank::Suspended, Tone::Ring, Some(Action::Resume))
    );
    assert_eq!(c.summary, "Suspended 2h ago");
    // Running and stopped are told apart by colour, not only by the words.
    let stopped = model::card(&place(), &env("x", "stopped"), now());
    assert_eq!(stopped.tone, Tone::Idle);
    assert_eq!(
        stopped.action,
        Some(Action::Start)
    );
    assert_eq!(
        model::card(&place(), &env("x", "stopping"), now()).status,
        "Stopping"
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

/// A place that is not online shows what was last known of it, offering
/// nothing to do.
#[test]
fn an_offline_place_is_last_known() {
    let state = PlaceState {
        connection: Connection::Offline {
            error: "refused".into(),
        },
        environments: vec![env("a", "running")],
        synced_at: Some(now() - Duration::hours(1)),
        ..Default::default()
    };
    let places = vec![(place(), state)];
    let cards = model::home(&places, now());
    assert!(cards[0].stale);
    assert_eq!(cards[0].action, None);
    assert_eq!(cards[0].summary, "Last known: running · 1h ago");
    assert!(
        model::idle(&cards).is_empty(),
        "a stale environment is not suspended"
    );
    let groups = model::sidebar(&places, now());
    assert_eq!(model::connection(&groups[0].connection).1, Some("Offline"));
}

/// A dot's ring is the machine and its centre the agent, so a running
/// machine's agent at work, asking or done each reads apart from the
/// machine merely running, and from one that is off.
#[test]
fn marks_show_the_machine_and_its_agent_apart() {
    let mark = |e: &Environment| model::card(&place(), e, now()).mark;
    let with = |state: &str, needs_you: bool| {
        let mut e = env("e", "running");
        e.agents = vec![agent("a", state, needs_you, 2)];
        mark(&e)
    };
    let on = |fill| Mark {
        ring: Tone::Good,
        fill,
    };
    assert_eq!(mark(&env("e", "running")), on(None));
    assert_eq!(with("idle", false), on(None));
    assert_eq!(with("working", false), on(Some(Tone::Busy)));
    assert_eq!(with("asking", true), on(Some(Tone::Wait)));
    assert_eq!(with("finished", true), on(Some(Tone::Good)));
    assert_eq!(mark(&env("e", "starting")), Mark::hollow(Tone::Busy));
    assert_eq!(mark(&env("e", "suspended")), Mark::hollow(Tone::Ring));
    assert_eq!(mark(&env("e", "stopped")), Mark::solid(Tone::Idle));
    assert_eq!(mark(&env("e", "failed")), Mark::solid(Tone::Bad));
}
