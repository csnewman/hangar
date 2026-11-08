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

/// The colour a status is drawn in: the design's status dots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Wants a decision.
    Wait,
    /// Done, to be looked at; or online.
    Good,
    Bad,
    /// Something is running.
    Busy,
    /// Nothing is happening.
    Idle,
    /// Kept for later: suspended. Drawn hollow.
    Ring,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineKind {
    /// What an agent asks.
    Question,
    Plain,
    /// Quieter detail.
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

/// An environment's dot: its ring is its machine (green running, amber
/// starting or changing, grey suspended or stopped, red failed) and its
/// centre what is in it: its agent's state while it runs (amber working,
/// indigo asking, green finished), its kept memory while it is suspended,
/// and nothing otherwise. A running machine with nothing happening and a
/// stopped one are both empty rings, green and grey.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    pub ring: Tone,
    /// None is an empty ring.
    pub fill: Option<Tone>,
}

impl Mark {
    pub fn empty(t: Tone) -> Mark {
        Mark {
            ring: t,
            fill: None,
        }
    }

    fn of(rank: Rank, tone: Tone) -> Mark {
        match rank {
            Rank::Asking | Rank::Finished | Rank::Working => Mark {
                ring: Tone::Good,
                fill: Some(tone),
            },
            Rank::Suspended => Mark {
                ring: Tone::Idle,
                fill: Some(Tone::Idle),
            },
            Rank::Running => Mark::empty(Tone::Good),
            Rank::Stopped => Mark::empty(Tone::Idle),
            Rank::Starting | Rank::Busy | Rank::Failed => Mark::empty(tone),
        }
    }
}

/// An environment as Home and the sidebar show it.
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
    /// The one colour it is summed up in, as a notice shows it.
    pub tone: Tone,
    /// Its dot, machine and agent apart.
    pub mark: Mark,
    /// The card's footer: "Asking · 3m".
    pub status: String,
    /// The sidebar row's second line: "Claude is asking: run the
    /// migration? · 3m".
    pub summary: String,
    pub action: Option<Action>,
    /// The agent's session the card is about, if any.
    pub session: Option<String>,
    /// Whether it is as last known, its place not being online.
    pub stale: bool,
}

/// The session that says most about an environment: the most urgent, then
/// the latest.
fn lead(agents: &[AgentSession]) -> Option<&AgentSession> {
    let rank = |a: &AgentSession| match a.state.as_str() {
        "asking" if a.needs_you => 0,
        "finished" if a.needs_you => 1,
        "asking" => 2,
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

/// "2h ago", "yesterday", "just now".
pub fn ago(since: DateTime<Utc>, now: DateTime<Utc>) -> String {
    match age(since, now).as_str() {
        "now" => "just now".into(),
        "yesterday" => "yesterday".into(),
        a => format!("{a} ago"),
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

/// A step as the middle of a sentence: "Run the tests" becomes "run the
/// tests", and "API docs" stays as it is.
fn lower_first(s: &str) -> String {
    let mut chars = s.chars();
    let (Some(first), second) = (chars.next(), s.chars().nth(1)) else {
        return s.to_string();
    };
    if second.is_some_and(char::is_uppercase) {
        return s.to_string();
    }
    first.to_lowercase().chain(chars).collect()
}

fn line(text: impl Into<String>, kind: LineKind) -> Line {
    Line {
        text: text.into(),
        kind,
    }
}

fn capitalised(phase: &str) -> String {
    let mut c = phase.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// How an environment looks, apart from its lines.
struct Look {
    rank: Rank,
    tone: Tone,
    status: String,
    summary: String,
    action: Option<Action>,
}

/// An environment as Home shows it, at now.
pub fn card(place: &PlaceConfig, e: &Environment, now: DateTime<Utc>) -> Card {
    let mut lines = Vec::new();
    let lead = lead(&e.agents);
    let who = lead.map(|a| agent_name(&a.agent)).unwrap_or("The agent");
    let look = match (e.phase.as_str(), lead) {
        ("failed", _) => {
            let reason = e
                .reason
                .clone()
                .unwrap_or_else(|| "It could not start.".into());
            lines.push(line(reason.clone(), LineKind::Plain));
            Look {
                rank: Rank::Failed,
                tone: Tone::Bad,
                status: "Failed to start".into(),
                summary: format!("Failed to start: {}", lower_first(&reason)),
                action: Some(Action::Details),
            }
        }
        // An agent that asks is asking until it is answered; once the user
        // has seen it, it is not among what needs them.
        ("running", Some(a)) if a.state == "asking" => {
            let question = match &a.step {
                Some(step) => format!("{who} asks: {}?", lower_first(step)),
                None => a
                    .message
                    .clone()
                    .unwrap_or_else(|| format!("{who} wants a decision")),
            };
            lines.push(line(question, LineKind::Question));
            if let (Some(_), Some(m)) = (&a.step, &a.message) {
                lines.push(line(m.clone(), LineKind::Muted));
            }
            let about = a
                .step
                .as_deref()
                .map(lower_first)
                .or(a.message.clone())
                .unwrap_or_default();
            Look {
                rank: if a.needs_you {
                    Rank::Asking
                } else {
                    Rank::Working
                },
                tone: Tone::Wait,
                status: format!("Asking · {}", age(a.since, now)),
                summary: format!("{who} is asking: {about}? · {}", age(a.since, now)),
                action: Some(Action::Answer),
            }
        }
        ("running", Some(a)) if a.state == "finished" && a.needs_you => {
            lines.push(line(format!("{who} finished its turn"), LineKind::Plain));
            Look {
                rank: Rank::Finished,
                tone: Tone::Good,
                status: format!("Finished · {}", age(a.since, now)),
                summary: format!("{who} finished · {}", age(a.since, now)),
                action: Some(Action::Review),
            }
        }
        ("running", Some(a))
            if a.state != "idle" && a.state != "finished" && a.state != "asking" =>
        {
            let step = a.step.clone().unwrap_or_else(|| "Thinking".into());
            lines.push(line(step.clone(), LineKind::Plain));
            Look {
                rank: Rank::Working,
                tone: Tone::Busy,
                status: format!("{who} working · {}", age(a.since, now)),
                summary: format!("{who} · {}", lower_first(&step)),
                action: None,
            }
        }
        ("running", lead) => {
            let (text, summary) = match lead {
                Some(_) => (
                    format!("{who} is waiting for a prompt"),
                    format!("{who} waiting"),
                ),
                None => ("No agent running".into(), "Running".into()),
            };
            lines.push(line(text, LineKind::Plain));
            Look {
                rank: Rank::Running,
                tone: Tone::Good,
                status: format!("Running · {}", age(e.updated_at, now)),
                summary: format!("{summary} · {}", age(e.updated_at, now)),
                action: Some(Action::Suspend),
            }
        }
        ("pending" | "starting", _) => {
            let step = e
                .progress
                .as_ref()
                .map(|p| start_step(&p.step))
                .unwrap_or("Waiting for a worker");
            lines.push(line(step, LineKind::Plain));
            Look {
                rank: Rank::Starting,
                tone: Tone::Busy,
                status: "Starting".into(),
                summary: format!("Starting: {}", lower_first(step)),
                action: None,
            }
        }
        ("suspended", _) => {
            lines.push(line(
                format!("Suspended {}", ago(e.updated_at, now)),
                LineKind::Plain,
            ));
            lines.push(line("Its memory is kept on disk", LineKind::Muted));
            Look {
                rank: Rank::Suspended,
                tone: Tone::Ring,
                status: "Suspended".into(),
                summary: format!("Suspended {}", ago(e.updated_at, now)),
                action: Some(Action::Resume),
            }
        }
        ("stopped", _) => {
            lines.push(line(
                format!("Stopped {}", ago(e.updated_at, now)),
                LineKind::Plain,
            ));
            Look {
                rank: Rank::Stopped,
                tone: Tone::Idle,
                status: "Stopped".into(),
                summary: format!("Stopped {}", ago(e.updated_at, now)),
                action: Some(Action::Start),
            }
        }
        (phase, _) => {
            let label = capitalised(phase);
            lines.push(line(format!("{label}…"), LineKind::Plain));
            Look {
                rank: Rank::Busy,
                tone: Tone::Busy,
                summary: label.clone(),
                status: label,
                action: None,
            }
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
        rank: look.rank,
        tone: look.tone,
        mark: Mark::of(look.rank, look.tone),
        status: look.status,
        summary: look.summary,
        action: look.action,
        session: lead.map(|a| a.id.clone()),
        stale: false,
    }
}

/// A place's environments as cards, marked stale when the place cannot be
/// reached. One still connecting shows what it last had as it is.
fn cards_of(p: &PlaceConfig, s: &PlaceState, now: DateTime<Utc>) -> Vec<Card> {
    let stale = !matches!(s.connection, Connection::Online | Connection::Connecting);
    s.environments
        .iter()
        .map(|e| {
            let mut c = card(p, e, now);
            if stale {
                c.stale = true;
                c.action = None;
                let when = s
                    .synced_at
                    .map(|t| format!(" · {}", ago(t, now)))
                    .unwrap_or_default();
                c.summary = format!("Last known: {}{when}", e.phase);
            }
            c
        })
        .collect()
}

fn by_attention(cards: &mut [Card]) {
    cards.sort_by(|a, b| a.rank.cmp(&b.rank).then_with(|| a.name.cmp(&b.name)));
}

/// Home's cards: every place's environments, what needs the user first.
pub fn home(places: &[(PlaceConfig, PlaceState)], now: DateTime<Utc>) -> Vec<Card> {
    let mut cards: Vec<Card> = places
        .iter()
        .flat_map(|(p, s)| cards_of(p, s, now))
        .collect();
    by_attention(&mut cards);
    cards
}

/// What a place is, as the sidebar says beside its name.
pub fn kind(_: &PlaceConfig) -> &'static str {
    "server"
}

/// A place's connection as the sidebar shows it: its dot, a word when it
/// is not online, and whether that word is a warning.
pub fn connection(c: &Connection) -> (Tone, Option<&'static str>, bool) {
    match c {
        Connection::Online => (Tone::Good, None, false),
        Connection::Connecting => (Tone::Busy, Some("Connecting…"), false),
        Connection::Offline { .. } => (Tone::Bad, Some("Offline"), true),
        Connection::SignIn => (Tone::Busy, Some("Sign in"), true),
        Connection::Unsupported { .. } => (Tone::Bad, Some("Needs an update"), true),
    }
}

/// One place in the sidebar, or on Home grouped by place.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub place: String,
    pub name: String,
    pub kind: &'static str,
    pub connection: Connection,
    pub rows: Vec<Card>,
}

/// The sidebar: each place, its environments needing the user first, the
/// rest by name.
pub fn sidebar(places: &[(PlaceConfig, PlaceState)], now: DateTime<Utc>) -> Vec<Group> {
    places
        .iter()
        .map(|(p, s)| {
            let mut rows = cards_of(p, s, now);
            rows.sort_by(|a, b| {
                let (an, bn) = (a.rank.needs_you(), b.rank.needs_you());
                bn.cmp(&an)
                    .then_with(|| {
                        if an {
                            a.rank.cmp(&b.rank)
                        } else {
                            std::cmp::Ordering::Equal
                        }
                    })
                    .then_with(|| a.name.cmp(&b.name))
            });
            Group {
                place: p.id.clone(),
                name: p.name.clone(),
                kind: kind(p),
                connection: s.connection.clone(),
                rows,
            }
        })
        .collect()
}

/// Home grouped by place, each place's cards by attention.
pub fn home_by_place(places: &[(PlaceConfig, PlaceState)], now: DateTime<Utc>) -> Vec<Group> {
    let mut groups = sidebar(places, now);
    for g in &mut groups {
        by_attention(&mut g.rows);
    }
    groups
}

/// The environments running with nothing to do: no agent working or
/// waiting on the user. "Suspend idle" suspends them.
pub fn idle(cards: &[Card]) -> Vec<(String, String)> {
    cards
        .iter()
        .filter(|c| c.rank == Rank::Running && !c.stale)
        .map(|c| (c.place.clone(), c.env.clone()))
        .collect()
}
