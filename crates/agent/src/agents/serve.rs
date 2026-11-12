//! The agents' socket: hooks' events in, the task both ways.

use std::future::Future;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::sync::Notify;

use super::{Reply, Request, Session, Task, Tracker};

/// The agent's session with the server (the profile session).
pub trait Uplink: Send + Sync + 'static {
    /// Tells the server the coding agents' sessions, all of them.
    fn report_agents(&self, sessions: Vec<Session>);
    /// Sets the environment's task as its coding agent, or marks it done
    /// for an empty text, and gives it as it is after.
    fn set_task(&self, text: String) -> impl Future<Output = Result<Option<Task>, String>> + Send;
    /// The environment's task as the server last said.
    fn task(&self) -> Option<Task>;
}

/// Finds and checks the agents' processes.
#[derive(Clone, Copy)]
pub struct Processes {
    /// The agent process a hook's parent belongs to: the parent itself, or
    /// what started the shell it is.
    pub agent: fn(i32) -> i32,
    /// Whether a process is still running.
    pub alive: fn(i32) -> bool,
}

impl Processes {
    /// The guest's own, from /proc.
    pub fn proc() -> Self {
        Self {
            agent: agent_of,
            alive,
        }
    }
}

/// How long a change waits for those close behind it before the sessions
/// are reported: a tool's PreToolUse and PostToolUse often come together.
const REPORT_GATHER: Duration = Duration::from_millis(300);

/// How often sessions whose process has gone are ended.
const SWEEP_EVERY: Duration = Duration::from_secs(10);

/// How long a request may take: setting the task waits for the server.
const TASK_WAIT: Duration = Duration::from_secs(30);

/// Serves `socket`, tracking the agents' sessions and reporting them
/// through `up`, until the socket fails.
pub async fn serve<U: Uplink>(socket: &Path, up: Arc<U>, procs: Processes) -> io::Result<()> {
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let _ = std::fs::remove_file(socket);
    let ln = UnixListener::bind(socket)?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o666))?;
    let tracker = Arc::new(Tracker::new());
    let changed = Arc::new(Notify::new());
    let reporter = {
        let (tracker, changed, up) = (tracker.clone(), changed.clone(), up.clone());
        tokio::spawn(async move {
            let mut sweep = tokio::time::interval(SWEEP_EVERY);
            sweep.tick().await;
            loop {
                tokio::select! {
                    _ = changed.notified() => tokio::time::sleep(REPORT_GATHER).await,
                    _ = sweep.tick() => {
                        if !tracker.sweep(SystemTime::now(), procs.alive) {
                            continue;
                        }
                    }
                }
                up.report_agents(tracker.sessions());
            }
        })
    };
    let _reporter = crate::serve::AbortOnDrop(reporter);
    loop {
        let (conn, _) = ln.accept().await?;
        let (tracker, changed, up) = (tracker.clone(), changed.clone(), up.clone());
        tokio::spawn(async move {
            let _ = tokio::time::timeout(TASK_WAIT, async move {
                let (rd, mut wr) = conn.into_split();
                let mut line = vec![];
                if tokio::io::BufReader::new(rd)
                    .read_until(b'\n', &mut line)
                    .await
                    .is_err()
                {
                    return;
                }
                let Ok(req) = serde_json::from_slice::<Request>(&line) else {
                    return;
                };
                let mut reply = Reply::default();
                if let Some(hook) = req.hook {
                    let pid = if hook.pid > 0 {
                        (procs.agent)(hook.pid)
                    } else {
                        0
                    };
                    if tracker.apply(&hook, pid, SystemTime::now()) {
                        changed.notify_one();
                    }
                    reply.task = up.task();
                } else if let Some(text) = req.set_task {
                    match up.set_task(text).await {
                        Ok(task) => reply.task = task,
                        Err(e) => reply.error = e,
                    }
                }
                let mut b = serde_json::to_vec(&reply).unwrap_or_default();
                b.push(b'\n');
                let _ = wr.write_all(&b).await;
            })
            .await;
        });
    }
}

/// Sends one request to the agent on `socket` and gives its reply, giving
/// up after `wait`.
pub(super) fn ask(socket: &Path, req: &Request, wait: Duration) -> Result<Reply, String> {
    let mut conn = std::os::unix::net::UnixStream::connect(socket).map_err(|e| e.to_string())?;
    conn.set_read_timeout(Some(wait))
        .map_err(|e| e.to_string())?;
    conn.set_write_timeout(Some(wait))
        .map_err(|e| e.to_string())?;
    let mut b = serde_json::to_vec(req).map_err(|e| e.to_string())?;
    b.push(b'\n');
    conn.write_all(&b).map_err(|e| e.to_string())?;
    let mut line = vec![];
    BufReader::new(conn)
        .read_until(b'\n', &mut line)
        .map_err(|e| e.to_string())?;
    let r: Reply = serde_json::from_slice(&line).map_err(|e| e.to_string())?;
    if !r.error.is_empty() {
        return Err(r.error);
    }
    Ok(r)
}

/// What a coding agent may run its hooks through.
const HOOK_SHELLS: &[&str] = &["sh", "bash", "dash", "zsh", "fish", "env"];

/// The process a hook's parent belongs to: the parent, or, when it is a
/// shell the agent ran the hook in, what started that.
fn agent_of(parent: i32) -> i32 {
    let mut pid = parent;
    for _ in 0..4 {
        match stat(pid) {
            Some((comm, _, ppid)) if HOOK_SHELLS.contains(&comm.as_str()) && ppid > 1 => pid = ppid,
            _ => return pid,
        }
    }
    pid
}

/// Whether a process is running: there, and not a zombie.
fn alive(pid: i32) -> bool {
    stat(pid).is_some_and(|(_, state, _)| state != "Z" && state != "X")
}

/// A process's command name, state and parent, from /proc.
fn stat(pid: i32) -> Option<(String, String, i32)> {
    let b = std::fs::read(format!("/proc/{pid}/stat")).ok()?;
    // pid (comm) state ppid ...; comm may hold spaces and parentheses.
    let open = b.iter().position(|&c| c == b'(')?;
    let close = b.iter().rposition(|&c| c == b')')?;
    if close < open {
        return None;
    }
    let rest = String::from_utf8_lossy(&b[close + 1..]).into_owned();
    let mut f = rest.split_whitespace();
    let state = f.next()?.to_string();
    let ppid = f.next()?.parse().ok()?;
    Some((
        String::from_utf8_lossy(&b[open + 1..close]).into_owned(),
        state,
        ppid,
    ))
}
