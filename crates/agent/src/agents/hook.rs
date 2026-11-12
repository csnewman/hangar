//! `hangar-agent hook <agent>`: a coding agent's hook.

use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use serde_json::{Map, Value, json};

use super::{CLAUDE, Hook, Request, SET_TASK_TOOL, Task, event, serve::ask};
use crate::terminal::TERMINAL_ENV;

/// How long a hook waits for the agent: a hook must never hold its coding
/// agent up.
const HOOK_WAIT: Duration = Duration::from_secs(2);

/// A hook's input as `agent` gives it on standard input. Only an object
/// is one: a field missing, null or of another type is empty.
pub fn parse_hook(agent: &str, input: &[u8]) -> Result<Hook, String> {
    let v: Value = serde_json::from_slice(input).map_err(|e| e.to_string())?;
    let Some(i) = v.as_object() else {
        return Err("a hook's input is an object".into());
    };
    let str = |k: &str| {
        i.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let mut h = Hook {
        agent: agent.to_string(),
        event: str("hook_event_name"),
        session: str("session_id"),
        notification: str("notification_type"),
        source: str("source"),
        ..Hook::default()
    };
    if h.event == event::NOTIFICATION {
        h.message = str("message");
    }
    let tool = str("tool_name");
    if !tool.is_empty() {
        let empty = Map::new();
        let input = i
            .get("tool_input")
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        h.step = step(&tool, input, &str("cwd"));
        h.tool = tool;
    }
    Ok(h)
}

/// What a tool does, in a few words: the file it acts on, or the short
/// description the agent gave a command. A command itself is not kept.
fn step(tool: &str, input: &Map<String, Value>, cwd: &str) -> String {
    let str = |k: &str| {
        input
            .get(k)
            .and_then(Value::as_str)
            .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
            .unwrap_or_default()
    };
    let mut path = ["file_path", "notebook_path", "path"]
        .into_iter()
        .map(str)
        .find(|p| !p.is_empty())
        .unwrap_or_default();
    if !path.is_empty()
        && Path::new(cwd).is_absolute()
        && Path::new(&path).is_absolute()
        && let Ok(rel) = Path::new(&path).strip_prefix(cwd)
    {
        path = match rel.to_string_lossy() {
            r if r.is_empty() => ".".into(),
            r => r.into_owned(),
        };
    }
    let described = |prefix: &str, otherwise: &str| match str("description") {
        d if d.is_empty() => otherwise.to_string(),
        d => format!("{prefix}{d}"),
    };
    match tool {
        "Edit" | "MultiEdit" | "NotebookEdit" => format!("Edit {path}"),
        "Write" => format!("Write {path}"),
        "Read" => format!("Read {path}"),
        "Bash" => described("", "Run a command"),
        "Grep" | "Glob" => "Search the code".into(),
        "WebFetch" => "Read a web page".into(),
        "WebSearch" => "Search the web".into(),
        "Task" | "Agent" => described("Subagent: ", "Run a subagent"),
        "TodoWrite" => "Update the to-do list".into(),
        _ => {
            let parts: Vec<&str> = tool.split("__").collect();
            if parts.len() == 3 && parts[0] == "mcp" {
                format!("Use {}", parts[2])
            } else if !path.is_empty() {
                format!("{tool} {path}")
            } else {
                tool.to_string()
            }
        }
    }
}

/// A hook's command: reads the hook's input from `input`, passes what it
/// keeps to the agent on `socket`, and writes to `out` what the coding
/// agent should be told. `getenv` and `parent` are the command's
/// environment and parent process. Nothing it meets is an error: a hook
/// that failed would show in the coding agent, and it must work whether or
/// not Hangar does.
pub fn run_hook(
    agent: &str,
    input: impl Read,
    mut out: impl Write,
    socket: &Path,
    getenv: impl Fn(&str) -> Option<String>,
    parent: i32,
) {
    let mut buf = vec![];
    if input.take(4 << 20).read_to_end(&mut buf).is_err() {
        return;
    }
    let Ok(mut h) = parse_hook(agent, &buf) else {
        return;
    };
    h.terminal = getenv(TERMINAL_ENV).unwrap_or_default();
    h.pid = parent;
    let (event, tool) = (h.event.clone(), h.tool.clone());
    let Ok(reply) = ask(
        socket,
        &Request {
            hook: Some(h),
            ..Request::default()
        },
        HOOK_WAIT,
    ) else {
        return;
    };
    let output = match (agent, event.as_str()) {
        (CLAUDE, event::SESSION_START) => json!({
            "hookEventName": event::SESSION_START,
            "additionalContext": session_context(reply.task.as_ref()),
        }),
        (CLAUDE, event::PRE_TOOL_USE) if tool == SET_TASK_TOOL => json!({
            "hookEventName": event::PRE_TOOL_USE,
            "permissionDecision": "allow",
            "permissionDecisionReason": "Hangar's task for this environment",
        }),
        _ => return,
    };
    let _ = writeln!(out, "{}", json!({ "hookSpecificOutput": output }));
}

/// What a Claude Code session is told as it starts.
fn session_context(task: Option<&Task>) -> String {
    let mut s = String::from(
        "This machine is a Hangar environment. Hangar shows the person a one-line task for each of \
         their environments, so they can tell them apart and see what each is for. When you start on a piece \
         of work, and whenever the work changes, set it with the set_task tool of the hangar MCP server: a few \
         words, like a commit's title (\"Fix rounding in checkout totals\").",
    );
    match task {
        Some(t) => s.push_str(&format!(
            " The environment's task is \"{}\"; leave it if the work is the same.",
            t.text
        )),
        None => s.push_str(" The environment has no task yet."),
    }
    s
}
