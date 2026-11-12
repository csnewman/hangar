//! Each coding agent session's state, from its hooks.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use super::{Hook, SET_TASK_TOOL, Session, State, event};

/// How long an ended session is kept, so a session that finished as it
/// exited still shows it did.
const ENDED_KEPT: Duration = Duration::from_secs(10 * 60);

/// The most sessions kept: the oldest ended go first, then the oldest.
const MAX_SESSIONS: usize = 32;

/// Keeps each coding agent session's state from its hooks.
#[derive(Default)]
pub struct Tracker {
    sessions: Mutex<HashMap<String, Tracked>>,
}

struct Tracked {
    session: Session,
    /// The session's agent process, to end it when the process goes
    /// without saying so.
    pid: i32,
}

impl Tracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes a hook's event, from process `pid` (the agent's, or 0 when it
    /// is not known), at `now`. It says whether any session changed.
    pub fn apply(&self, h: &Hook, pid: i32, now: SystemTime) -> bool {
        if h.session.is_empty() || h.agent.is_empty() {
            return false;
        }
        let mut sessions = self.sessions.lock().unwrap();
        if !sessions.contains_key(&h.session) {
            if h.event == event::SESSION_END {
                return false;
            }
            sessions.insert(
                h.session.clone(),
                Tracked {
                    session: Session {
                        id: h.session.clone(),
                        agent: h.agent.clone(),
                        terminal: String::new(),
                        state: State::Idle,
                        step: String::new(),
                        message: String::new(),
                        since: now,
                    },
                    pid: 0,
                },
            );
            trim(&mut sessions);
        }
        let Some(t) = sessions.get_mut(&h.session) else {
            // Trimmed at once: there were more than enough newer.
            return false;
        };
        let before = t.session.clone();
        if pid != 0 {
            t.pid = pid;
        }
        let s = &mut t.session;
        if !h.terminal.is_empty() {
            s.terminal = h.terminal.clone();
        }
        let none = String::new;
        let (mut state, mut step, mut message) = (s.state, s.step.clone(), s.message.clone());
        match h.event.as_str() {
            event::SESSION_START => {
                // A session compacted or resumed carries on as it was.
                if before.state == State::Ended {
                    (state, step, message) = (State::Idle, none(), none());
                }
            }
            event::USER_PROMPT_SUBMIT => (state, step, message) = (State::Working, none(), none()),
            event::PRE_TOOL_USE => {
                if h.tool != SET_TASK_TOOL {
                    (state, step, message) = (State::Working, h.step.clone(), none());
                }
            }
            event::POST_TOOL_USE => (state, message) = (State::Working, none()),
            event::PERMISSION_REQUEST => {
                (state, step, message) = (
                    State::Asking,
                    h.step.clone(),
                    format!("Permission to use {}", h.tool),
                )
            }
            event::NOTIFICATION => {
                if asks(h) {
                    (state, message) = (State::Asking, h.message.clone());
                }
            }
            event::STOP => (state, step, message) = (State::Finished, none(), none()),
            event::SESSION_END => (state, step, message) = (State::Ended, none(), none()),
            _ => {}
        }
        if state != s.state {
            s.since = now;
        }
        (s.state, s.step, s.message) = (state, step, message);
        *s != before
    }

    /// Ends the sessions whose agent process has gone, as `alive` says, and
    /// forgets those ended long enough ago. It says whether any changed.
    pub fn sweep(&self, now: SystemTime, alive: impl Fn(i32) -> bool) -> bool {
        let mut sessions = self.sessions.lock().unwrap();
        let mut changed = false;
        sessions.retain(|_, t| {
            let s = &mut t.session;
            if s.state == State::Ended {
                let old = now.duration_since(s.since).unwrap_or_default() > ENDED_KEPT;
                changed |= old;
                return !old;
            }
            if t.pid != 0 && !alive(t.pid) {
                (s.state, s.since) = (State::Ended, now);
                s.step.clear();
                s.message.clear();
                changed = true;
            }
            true
        });
        changed
    }

    /// The sessions, oldest first.
    pub fn sessions(&self) -> Vec<Session> {
        let sessions = self.sessions.lock().unwrap();
        let mut out: Vec<Session> = sessions.values().map(|t| t.session.clone()).collect();
        out.sort_by(|a, b| a.since.cmp(&b.since).then_with(|| a.id.cmp(&b.id)));
        out
    }
}

/// Whether a notification wants a decision: a permission prompt or a
/// question, not the reminder that the agent is waiting for its next
/// prompt.
fn asks(h: &Hook) -> bool {
    match h.notification.as_str() {
        "permission_prompt"
        | "elicitation_dialog"
        | "elicitation_url_dialog"
        | "agent_needs_input" => true,
        "" => !h.message.is_empty() && !h.message.contains("waiting for your input"),
        _ => false,
    }
}

/// Keeps at most `MAX_SESSIONS`, the oldest ended going first.
fn trim(sessions: &mut HashMap<String, Tracked>) {
    while sessions.len() > MAX_SESSIONS {
        let rank = |t: &Tracked| (t.session.state != State::Ended, t.session.since);
        let Some(oldest) = sessions
            .iter()
            .min_by_key(|(_, t)| rank(t))
            .map(|(id, _)| id.clone())
        else {
            return;
        };
        sessions.remove(&oldest);
    }
}
