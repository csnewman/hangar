//! Coding agents' hooks, the tracker that follows them, the socket they
//! reach it through, and the MCP server that sets the task.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use hangar_agent::agents::{
    self, Hook, Processes, SET_TASK_TOOL, Session, State, Task, Tracker, Uplink,
};
use serde_json::Value;

/// A directory of its own, short enough for a unix socket, removed after.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = PathBuf::from(format!("/tmp/ha-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn hook(event: &str) -> Hook {
    Hook {
        agent: agents::CLAUDE.into(),
        event: event.into(),
        session: "s".into(),
        ..Hook::default()
    }
}

#[test]
fn parse_hook_keeps_what_travels() {
    let tool = |tool: &str, step: &str| Hook {
        tool: tool.into(),
        step: step.into(),
        ..hook("PreToolUse")
    };
    let cases = [
        (
            r#"{"session_id":"s","hook_event_name":"PreToolUse","cwd":"/home/dev/app","tool_name":"Edit",
               "tool_input":{"file_path":"/home/dev/app/src/main.go","old_string":"secret","new_string":"x"}}"#,
            tool("Edit", "Edit src/main.go"),
        ),
        (
            r#"{"session_id":"s","hook_event_name":"PreToolUse","cwd":"/home/dev/app","tool_name":"Bash",
               "tool_input":{"command":"psql -c 'select secret'","description":"Run the  migration"}}"#,
            tool("Bash", "Run the migration"),
        ),
        (
            r#"{"session_id":"s","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"ls"}}"#,
            tool("Bash", "Run a command"),
        ),
        (
            r#"{"session_id":"s","hook_event_name":"PreToolUse","cwd":"/a","tool_name":"Read","tool_input":{"file_path":"/etc/hosts"}}"#,
            tool("Read", "Read /etc/hosts"),
        ),
        (
            r#"{"session_id":"s","hook_event_name":"PreToolUse","tool_name":"mcp__github__create_issue","tool_input":null}"#,
            tool("mcp__github__create_issue", "Use create_issue"),
        ),
        (
            r#"{"session_id":"s","hook_event_name":"UserPromptSubmit","prompt":"my secret plan"}"#,
            hook("UserPromptSubmit"),
        ),
        (
            r#"{"session_id":"s","hook_event_name":"Notification","message":"Claude needs your permission to use Bash",
               "notification_type":"permission_prompt"}"#,
            Hook {
                message: "Claude needs your permission to use Bash".into(),
                notification: "permission_prompt".into(),
                ..hook("Notification")
            },
        ),
        (
            r#"{"session_id":"s","hook_event_name":"Stop","last_assistant_message":"all done, the key is hunter2"}"#,
            hook("Stop"),
        ),
    ];
    for (input, want) in cases {
        let got = agents::parse_hook("claude", input.as_bytes()).unwrap();
        assert_eq!(got, want, "{input}");
    }
    assert!(agents::parse_hook("claude", b"[1]").is_err());
}

#[test]
fn tracker_follows_a_turn() {
    let t = Tracker::new();
    let mut now = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
    let mut apply = |h: Hook| -> Session {
        now += Duration::from_secs(1);
        t.apply(&h, 42, now);
        let list = t.sessions();
        assert_eq!(list.len(), 1, "after {}", h.event);
        list[0].clone()
    };
    let s = apply(Hook {
        terminal: "t1".into(),
        ..hook("SessionStart")
    });
    assert!(
        s.state == State::Idle && s.terminal == "t1",
        "started: {s:?}"
    );
    apply(hook("UserPromptSubmit"));
    let s = apply(Hook {
        tool: "Edit".into(),
        step: "Edit a.go".into(),
        ..hook("PreToolUse")
    });
    assert!(
        s.state == State::Working && s.step == "Edit a.go",
        "editing: {s:?}"
    );
    let working = s.since;
    let s = apply(Hook {
        tool: "Edit".into(),
        ..hook("PostToolUse")
    });
    assert!(
        s.since == working && s.step == "Edit a.go",
        "still working: {s:?}"
    );
    // Setting the task is not a step.
    let s = apply(Hook {
        tool: SET_TASK_TOOL.into(),
        ..hook("PreToolUse")
    });
    assert_eq!(s.step, "Edit a.go", "setting the task");
    let s = apply(Hook {
        notification: "permission_prompt".into(),
        message: "Claude needs your permission to use Bash".into(),
        ..hook("Notification")
    });
    assert!(
        s.state == State::Asking && !s.message.is_empty(),
        "asking: {s:?}"
    );
    let s = apply(Hook {
        tool: "Bash".into(),
        ..hook("PostToolUse")
    });
    assert!(
        s.state == State::Working && s.message.is_empty(),
        "answered: {s:?}"
    );
    let s = apply(hook("Stop"));
    assert!(
        s.state == State::Finished && s.step.is_empty(),
        "finished: {s:?}"
    );
    // The reminder that it waits for a prompt is not a question.
    let s = apply(Hook {
        notification: "idle_prompt".into(),
        message: "Claude is waiting for your input".into(),
        ..hook("Notification")
    });
    assert_eq!(s.state, State::Finished, "idle reminder");
    let s = apply(Hook {
        source: "compact".into(),
        ..hook("SessionStart")
    });
    assert_eq!(s.state, State::Finished, "compacted");

    // Its process goes without a SessionEnd: it ends, and is forgotten
    // later.
    assert!(!t.sweep(now, |_| true), "swept while its process lives");
    assert!(t.sweep(now, |pid| pid != 42));
    assert_eq!(
        t.sessions()[0].state,
        State::Ended,
        "after its process went"
    );
    assert!(t.sweep(now + Duration::from_secs(3600), |_| false));
    assert!(t.sessions().is_empty(), "an hour later");
    let ended = Hook {
        session: "x".into(),
        ..hook("SessionEnd")
    };
    assert!(
        !t.apply(&ended, 0, now),
        "a session that ended before it was seen was kept"
    );
}

/// Stands in for the profile session.
#[derive(Default)]
struct Up {
    reports: Mutex<Vec<Vec<Session>>>,
    task: Mutex<Option<Task>>,
}

impl Uplink for Up {
    fn report_agents(&self, sessions: Vec<Session>) {
        self.reports.lock().unwrap().push(sessions);
    }

    async fn set_task(&self, text: String) -> Result<Option<Task>, String> {
        let t = Task {
            id: "t".into(),
            text,
            set_by: "agent".into(),
            ..Task::default()
        };
        *self.task.lock().unwrap() = Some(t.clone());
        Ok(Some(t))
    }

    fn task(&self) -> Option<Task> {
        self.task.lock().unwrap().clone()
    }
}

fn run_hook(sock: &Path, input: &str) -> String {
    let mut out = vec![];
    agents::run_hook(
        agents::CLAUDE,
        input.as_bytes(),
        &mut out,
        sock,
        |k| (k == "HANGAR_TERMINAL_SESSION").then(|| "t1".to_string()),
        std::process::id() as i32,
    );
    String::from_utf8(out).unwrap()
}

/// A hook reaches the tracker and is reported; a session starting is told
/// to set the task, and set_task runs without asking and sets it.
#[tokio::test(flavor = "multi_thread")]
async fn hooks_and_the_task_tool() {
    let dir = Scratch::new();
    let sock = dir.0.join("agents.sock");
    let up = Arc::new(Up::default());
    let procs = Processes {
        agent: |p| p,
        alive: |_| true,
    };
    tokio::spawn({
        let (sock, up) = (sock.clone(), up.clone());
        async move { agents::serve(&sock, up, procs).await }
    });
    for _ in 0..200 {
        if sock.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let s = sock.clone();
    let blocking = move |input: &'static str| {
        let s = s.clone();
        tokio::task::spawn_blocking(move || run_hook(&s, input))
    };
    let out =
        blocking(r#"{"session_id":"s1","hook_event_name":"SessionStart","source":"startup"}"#)
            .await
            .unwrap();
    assert!(
        out.contains("\"additionalContext\"")
            && out.contains("set_task")
            && out.contains("no task yet"),
        "SessionStart's output: {out}"
    );
    let out = blocking(
        r#"{"session_id":"s1","hook_event_name":"PreToolUse","tool_name":"Edit","tool_input":{"file_path":"a.go"}}"#,
    )
    .await
    .unwrap();
    assert_eq!(out, "", "an edit's output");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let last = up.reports.lock().unwrap().last().cloned();
        if let Some(l) = last
            && l.len() == 1
            && l[0].step == "Edit a.go"
            && l[0].terminal == "t1"
        {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "never reported");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let out = blocking(
        r#"{"session_id":"s1","hook_event_name":"PreToolUse","tool_name":"mcp__plugin_hangar_hangar__set_task","tool_input":{"task":"Fix it"}}"#,
    )
    .await
    .unwrap();
    assert!(
        out.contains("\"permissionDecision\":\"allow\""),
        "set_task's output: {out}"
    );

    // The MCP server: initialise, list, call.
    let input = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{}}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"set_task","arguments":{"task":"Fix rounding"}}}"#,
        r#"{"jsonrpc":"2.0","id":4,"method":"resources/list"}"#,
    ]
    .join("\n");
    let s = sock.clone();
    let out = tokio::task::spawn_blocking(move || {
        let mut out = vec![];
        agents::serve_mcp(input.as_bytes(), &mut out, &s).unwrap();
        out
    })
    .await
    .unwrap();
    let replies: Vec<Value> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(replies.len(), 4, "{replies:?}");
    assert_eq!(replies[0]["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(replies[1]["result"]["tools"][0]["name"], "set_task");
    assert_eq!(replies[2]["result"]["isError"], false, "{:?}", replies[2]);
    assert_eq!(up.task().unwrap().text, "Fix rounding");
    assert!(
        replies[3]["error"].is_object(),
        "an unknown method: {:?}",
        replies[3]
    );

    // With the task set, a session starting is told it.
    let out =
        blocking(r#"{"session_id":"s2","hook_event_name":"SessionStart","source":"startup"}"#)
            .await
            .unwrap();
    assert!(
        out.contains("Fix rounding"),
        "SessionStart with a task: {out}"
    );
}

/// Without the agent, a hook says nothing and does not fail.
#[test]
fn hook_without_the_agent() {
    let dir = Scratch::new();
    let out = run_hook(
        &dir.0.join("none.sock"),
        r#"{"session_id":"s","hook_event_name":"SessionStart"}"#,
    );
    assert_eq!(out, "");
    assert_eq!(run_hook(Path::new("/nonexistent"), "not json"), "");
}

#[test]
fn plugin() {
    let dir = Scratch::new();
    agents::write_plugin(&dir.0, "/usr/local/bin/hangar-agent").unwrap();
    let read = |name: &str| -> Value {
        serde_json::from_slice(&std::fs::read(dir.0.join(name)).unwrap()).unwrap()
    };
    assert_eq!(read(".claude-plugin/plugin.json")["name"], "hangar");
    assert_eq!(
        read("hooks/hooks.json")["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
        "/usr/local/bin/hangar-agent hook claude"
    );
    assert_eq!(read(".mcp.json")["mcpServers"]["hangar"]["args"][0], "mcp");
}
