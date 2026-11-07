//! What the app shows of the places' environments: Home's cards and the
//! sidebar's rows, ranked by what needs the user most. Pure, so the rules
//! are tested apart from drawing.

use chrono::{DateTime, Utc};

use crate::api::{AgentSession, Environment};
use crate::place::{Connection, PlaceConfig, PlaceState};

/// How much an environment needs the user, most first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rank {
    Asking,
    Finished,
    Failed,
    Working,
    Starting,
    Running,
    Busy,
    Suspended,
    Stopped,
}

impl Rank {
    /// Whether it is among what needs the user.
    pub fn needs_you(self) -> bool {
        self <= Rank::Failed
    }
}

/// The colour a status is drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Wants a decision.
    Ask,
    /// Done, to be looked at.
    Done,
    Failed,
    /// Something is running.
    Busy,
    Quiet,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineKind {
    /// What an agent asks.
    Question,
    Plain,
    Muted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub kind: LineKind,
}

/// The one thing a card offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Answer,
    Review,
    Details,
    Suspend,
    Resume,
    Start,
}

impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Action::Answer => "Answer",
            Action::Review => "Review",
            Action::Details => "Details",
            Action::Suspend => "Suspend",
            Action::Resume => "Resume",
            Action::Start => "Start",
        }
    }
}

/// An environment as Home shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub place: String,
    pub place_name: String,
    pub env: String,
    /// Its task, or its name when it has none.
    pub title: String,
    /// Whether the title is a task its agent set.
    pub agent_task: bool,
    pub name: String,
    /// What is said under the title: its name under a task, else its
    /// template.
    pub subtitle: String,
    pub lines: Vec<Line>,
    pub rank: Rank,
    pub tone: Tone,
    /// "Asking · 3m".
    pub status: String,
    pub action: Option<Action>,
    /// The agent's session the card is about, if any.
    pub session: Option<String>,
}

/// The session that says most about an environment: the most urgent, then
/// the latest.
fn lead(agents: &[AgentSession]) -> Option<&AgentSession> {
    let rank = |a: &AgentSession| match a.state.as_str() {
        "asking" if a.needs_you => 0,
        "finished" if a.needs_you => 1,
        "asking" => 2,
        "ended" => 5,
        "idle" | "finished" => 4,
        _ => 3,
    };
    agents
        .iter()
        .filter(|a| a.state != "ended")
        .min_by(|a, b| rank(a).cmp(&rank(b)).then(b.since.cmp(&a.since)))
}

fn agent_name(agent: &str) -> &str {
    match agent {
        "claude" => "Claude",
        "codex" => "Codex",
        other => other,
    }
}

/// How long ago a time was, in a few characters.
pub fn age(since: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (now - since).num_seconds().max(0);
    match secs {
        0..60 => "now".into(),
        60..3600 => format!("{}m", secs / 60),
        3600..86400 => format!("{}h", secs / 3600),
        86400..172800 => "yesterday".into(),
        _ => format!("{}d", secs / 86400),
    }
}

fn start_step(step: &str) -> &str {
    match step {
        "download" => "Downloading the image",
        "unpack" => "Unpacking the image",
        "snapshot" => "Keeping a copy to roll back to",
        "disks" => "Making its disks",
        "boot" => "Booting",
        "network" => "Waiting for the network",
        "profile" => "Bringing your settings",
        "workspace" => "Setting up the workspace",
        _ => "Starting",
    }
}

fn line(text: impl Into<String>, kind: LineKind) -> Line {
    Line {
        text: text.into(),
        kind,
    }
}

/// An environment as Home shows it, at now.
pub fn card(place: &PlaceConfig, e: &Environment, now: DateTime<Utc>) -> Card {
    let mut lines = Vec::new();
    let lead = lead(&e.agents);
    let (rank, tone, label, since, action) = match (e.phase.as_str(), lead) {
        ("failed", _) => {
            lines.push(line(
                e.reason
                    .clone()
                    .unwrap_or_else(|| "It could not start.".into()),
                LineKind::Plain,
            ));
            (
                Rank::Failed,
                Tone::Failed,
                "Failed".to_string(),
                e.updated_at,
                Some(Action::Details),
            )
        }
        ("running", Some(a)) if a.state == "asking" && a.needs_you => {
            lines.push(line(
                a.message
                    .clone()
                    .unwrap_or_else(|| format!("{} wants a decision", agent_name(&a.agent))),
                LineKind::Question,
            ));
            if let Some(step) = &a.step {
                lines.push(line(step.clone(), LineKind::Muted));
            }
            (
                Rank::Asking,
                Tone::Ask,
                "Asking".into(),
                a.since,
                Some(Action::Answer),
            )
        }
        ("running", Some(a)) if a.state == "finished" && a.needs_you => {
            lines.push(line(
                format!("{} finished its turn", agent_name(&a.agent)),
                LineKind::Plain,
            ));
            (
                Rank::Finished,
                Tone::Done,
                "Finished".into(),
                a.since,
                Some(Action::Review),
            )
        }
        ("running", Some(a))
            if a.state != "idle" && a.state != "finished" && a.state != "asking" =>
        {
            lines.push(line(
                a.step.clone().unwrap_or_else(|| "Thinking".into()),
                LineKind::Plain,
            ));
            let label = format!("{} working", agent_name(&a.agent));
            (Rank::Working, Tone::Busy, label, a.since, None)
        }
        ("running", lead) => {
            lines.push(line(
                match lead {
                    Some(a) => format!("{} is waiting for a prompt", agent_name(&a.agent)),
                    None => "No agent running".into(),
                },
                LineKind::Plain,
            ));
            (
                Rank::Running,
                Tone::Quiet,
                "Running".into(),
                e.updated_at,
                Some(Action::Suspend),
            )
        }
        ("pending" | "starting", _) => {
            let step = e
                .progress
                .as_ref()
                .map(|p| start_step(&p.step))
                .unwrap_or("Waiting for a worker");
            lines.push(line(step, LineKind::Plain));
            (
                Rank::Starting,
                Tone::Busy,
                "Starting".into(),
                e.updated_at,
                None,
            )
        }
        ("suspended", _) => {
            lines.push(line("Its memory is kept on disk", LineKind::Muted));
            (
                Rank::Suspended,
                Tone::Quiet,
                "Suspended".into(),
                e.updated_at,
                Some(Action::Resume),
            )
        }
        ("stopped", _) => (
            Rank::Stopped,
            Tone::Quiet,
            "Stopped".into(),
            e.updated_at,
            Some(Action::Start),
        ),
        (phase, _) => {
            let mut label = phase.to_string();
            if let Some(c) = label.get_mut(0..1) {
                c.make_ascii_uppercase();
            }
            (Rank::Busy, Tone::Busy, label, e.updated_at, None)
        }
    };
    let others = e
        .agents
        .iter()
        .filter(|a| a.state != "ended")
        .count()
        .saturating_sub(1);
    if others > 0 && e.phase == "running" {
        let s = if others == 1 { "" } else { "s" };
        lines.push(line(format!("{others} more session{s}"), LineKind::Muted));
    }
    if e.worker_online == Some(false) && e.phase == "running" {
        lines.push(line("Its worker has not reported lately", LineKind::Muted));
    }
    Card {
        place: place.id.clone(),
        place_name: place.name.clone(),
        env: e.id.clone(),
        title: e
            .task
            .as_ref()
            .map(|t| t.text.clone())
            .unwrap_or_else(|| e.name.clone()),
        agent_task: e.task.as_ref().is_some_and(|t| t.set_by == "agent"),
        name: e.name.clone(),
        subtitle: match &e.task {
            Some(_) => e.name.clone(),
            None => e.template.clone(),
        },
        lines,
        rank,
        tone,
        status: format!("{label} · {}", age(since, now)),
        action,
        session: lead.map(|a| a.id.clone()),
    }
}

/// Home's cards: every place's environments, what needs the user first.
pub fn home(places: &[(PlaceConfig, PlaceState)], now: DateTime<Utc>) -> Vec<Card> {
    let mut cards: Vec<Card> = places
        .iter()
        .flat_map(|(p, s)| s.environments.iter().map(move |e| card(p, e, now)))
        .collect();
    cards.sort_by(|a, b| a.rank.cmp(&b.rank).then_with(|| a.name.cmp(&b.name)));
    cards
}

/// One place in the sidebar, its environments needing the user first.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub place: String,
    pub name: String,
    pub connection: Connection,
    pub rows: Vec<Card>,
}

pub fn sidebar(places: &[(PlaceConfig, PlaceState)], now: DateTime<Utc>) -> Vec<Group> {
    places
        .iter()
        .map(|(p, s)| {
            let mut rows: Vec<Card> = s.environments.iter().map(|e| card(p, e, now)).collect();
            rows.sort_by(|a, b| {
                b.rank
                    .needs_you()
                    .cmp(&a.rank.needs_you())
                    .then_with(|| match a.rank.needs_you() {
                        true => a.rank.cmp(&b.rank),
                        false => std::cmp::Ordering::Equal,
                    })
                    .then_with(|| a.name.cmp(&b.name))
            });
            Group {
                place: p.id.clone(),
                name: p.name.clone(),
                connection: s.connection.clone(),
                rows,
            }
        })
        .collect()
}
