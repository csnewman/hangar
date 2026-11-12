//! The bell's notices: what has come to need the user since the places
//! were last seen.

use chrono::{DateTime, Duration, Utc};
use hangar_app::api::{AgentSession, Environment};
use hangar_app::model::Tone;
use hangar_app::notices::Notices;
use hangar_app::place::{Connection, PlaceConfig, PlaceState};

fn now() -> DateTime<Utc> {
    "2026-11-07T12:00:00Z".parse().unwrap()
}

fn env(name: &str, phase: &str, agents: Vec<AgentSession>) -> Environment {
    Environment {
        id: format!("id-{name}"),
        short_id: "abc123".into(),
        name: name.into(),
        owner: "alice".into(),
        template: "t".into(),
        phase: phase.into(),
        desired: "running".into(),
        reason: (phase == "failed").then(|| "No worker has room for 16 CPUs".into()),
        progress: None,
        worker: None,
        worker_online: Some(true),
        cpus: None,
        memory_mib: None,
        editor_path: None,
        ports_public: false,
        display: None,
        web_names: vec![],
        task: None,
        agents,
        created_at: now() - Duration::days(1),
        updated_at: now() - Duration::hours(1),
    }
}

fn asking() -> AgentSession {
    AgentSession {
        id: "s".into(),
        agent: "claude".into(),
        terminal: None,
        state: "asking".into(),
        step: Some("Run the migration".into()),
        message: None,
        since: now(),
        needs_you: true,
    }
}

fn places(connection: Connection, envs: Vec<Environment>) -> Vec<(PlaceConfig, PlaceState)> {
    vec![(
        PlaceConfig {
            id: "p".into(),
            name: "Acme".into(),
            url: "https://hangar.acme.test".into(),
        },
        PlaceState {
            connection,
            environments: envs,
            ..Default::default()
        },
    )]
}

#[test]
fn what_needs_you_at_first_is_listed_but_not_news() {
    let mut n = Notices::default();
    let news = n.observe(
        &places(
            Connection::Online,
            vec![
                env("a", "running", vec![asking()]),
                env("b", "running", vec![]),
            ],
        ),
        now(),
    );
    assert!(news.is_empty());
    assert_eq!(n.list().len(), 1);
    assert_eq!(n.list()[0].what, "Claude is asking");
    assert_eq!(n.list()[0].detail, "Claude asks: run the migration?");
    assert_eq!(n.unread(), 1);
}

#[test]
fn an_agent_coming_to_ask_and_a_failure_are_news_once() {
    let mut n = Notices::default();
    n.observe(
        &places(
            Connection::Online,
            vec![env("a", "running", vec![]), env("b", "starting", vec![])],
        ),
        now(),
    );
    let later = places(
        Connection::Online,
        vec![
            env("a", "running", vec![asking()]),
            env("b", "failed", vec![]),
        ],
    );
    let news = n.observe(&later, now());
    let said: Vec<(&str, &str)> = news
        .iter()
        .map(|x| (x.title.as_str(), x.what.as_str()))
        .collect();
    assert_eq!(said, [("a", "Claude is asking"), ("b", "failed to start")]);
    assert_eq!(news[1].tone, Tone::Bad);
    assert_eq!(news[1].detail, "No worker has room for 16 CPUs");
    // The same again is not news.
    assert!(n.observe(&later, now()).is_empty());
    assert_eq!(n.list().len(), 2);
    n.read_all();
    assert_eq!(n.unread(), 0);
}

#[test]
fn a_place_asking_to_be_signed_in_is_news() {
    let mut n = Notices::default();
    n.observe(&places(Connection::Online, vec![]), now());
    let news = n.observe(&places(Connection::SignIn, vec![]), now());
    assert_eq!(news.len(), 1);
    assert_eq!(
        (news[0].title.as_str(), news[0].what.as_str()),
        ("Acme", "sign in again")
    );
    assert!(
        n.observe(&places(Connection::SignIn, vec![]), now())
            .is_empty()
    );
}
