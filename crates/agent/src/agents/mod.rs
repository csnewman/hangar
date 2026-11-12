//! The coding agents running in the environment -- Claude Code, Codex --
//! followed through their hooks, and the environment's task, which they
//! set.
//!
//! Each agent's hooks run this binary (`hook`), which reads the hook's
//! input, keeps the little that travels -- the event, the tool and what it
//! acts on -- and passes it to the agent's tracker over a socket (`serve`).
//! The tracker turns events into each session's state, which the profile
//! session reports to the server. The task is set through an MCP server
//! offered to Claude Code (`mcp`), whose one tool its hooks let run without
//! asking.

mod hook;
mod mcp;
mod plugin;
mod serve;
mod tracker;

use std::time::SystemTime;

use serde::{Deserialize, Serialize, Serializer};

pub use hook::{parse_hook, run_hook};
pub use mcp::serve_mcp;
pub use plugin::{PLUGIN_DIR, write_plugin};
pub use serve::{Processes, Uplink, serve};
pub use tracker::Tracker;

/// Where hooks and the MCP server reach the agent. Anyone in the guest may:
/// a coding agent runs as whoever starts it, and the machine, not its
/// users, is the boundary.
pub const SOCKET: &str = "/run/hangar/agents.sock";

/// The coding agents followed, as the server names them.
pub const CLAUDE: &str = "claude";

/// The MCP tool that sets the task, as Claude Code names it:
/// mcp__plugin_<plugin>_<server>__<tool>.
pub const SET_TASK_TOOL: &str = "mcp__plugin_hangar_hangar__set_task";

/// The hook events followed. Codex's are named as Claude Code's.
pub mod event {
    pub const SESSION_START: &str = "SessionStart";
    pub const SESSION_END: &str = "SessionEnd";
    pub const USER_PROMPT_SUBMIT: &str = "UserPromptSubmit";
    pub const PRE_TOOL_USE: &str = "PreToolUse";
    pub const POST_TOOL_USE: &str = "PostToolUse";
    pub const PERMISSION_REQUEST: &str = "PermissionRequest";
    pub const NOTIFICATION: &str = "Notification";
    pub const STOP: &str = "Stop";
}

/// What one hook said, as much of it as is kept: never the prompt, the
/// command or what a file holds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Hook {
    /// Which coding agent: `CLAUDE`, or codex.
    pub agent: String,
    /// The hook's event: PreToolUse, Stop, ...
    pub event: String,
    pub session: String,
    /// The terminal session the agent runs in, from the terminal's
    /// `HANGAR_TERMINAL_SESSION`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub terminal: String,
    /// The hook command's parent: the agent, or a shell it ran the hook in.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub pid: i32,
    /// The tool a tool event is for, and what it does ("Edit src/main.go").
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tool: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub step: String,
    /// A notification's text, and its kind.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notification: String,
    /// Why a session started: startup, resume, clear, compact.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
}

fn is_zero(n: &i32) -> bool {
    *n == 0
}

/// One line written to the socket.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Request {
    /// A hook's event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook: Option<Hook>,
    /// Sets the environment's task; empty marks it done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_task: Option<String>,
}

/// The answer to a `Request`, on one line: the environment's task as it is
/// after, or why the request failed.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Reply {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<Task>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
}

/// The environment's task, as the server sends it (`api.Task`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Task {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub text: String,
    /// agent or user.
    #[serde(default)]
    pub set_by: String,
    #[serde(default)]
    pub set_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_at: Option<String>,
}

/// What a coding agent's session is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Wants a decision: a permission prompt, a question.
    Asking,
    /// Ended its turn.
    Finished,
    /// Has a prompt or a tool running.
    Working,
    /// Waiting for a prompt.
    Idle,
    /// Its process has gone.
    Ended,
}

/// One coding agent's session (`api.AgentSession`): only what it is doing
/// travels, the tool and the file, never prompts, file contents or
/// commands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Session {
    /// The agent's own ID for the session.
    pub id: String,
    pub agent: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub terminal: String,
    pub state: State,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub step: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub message: String,
    /// When it took its state.
    #[serde(serialize_with = "rfc3339")]
    pub since: SystemTime,
}

fn rfc3339<S: Serializer>(t: &SystemTime, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&crate::clock::rfc3339(*t))
}
