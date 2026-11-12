//! The environment's task manager, served on vsock port 8106 in the protocol
//! `internal/procs` defines: what runs in the guest, what each process
//! costs, and stopping one.
//!
//! A connection carries requests and replies as JSON lines, one reply per
//! request. CPU use is measured between one listing and the next, so the
//! first listing reports none; a listing refreshed every few seconds reports
//! the share each process had since the one before.

use std::collections::HashMap;
use std::fs;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

use crate::clock::rfc3339;
use crate::user::User;

pub const PORT: u32 = 8106;

/// USER_HZ, the unit /proc counts CPU time in: 100 on every architecture
/// Linux runs guests on.
const CLOCK_TICKS: f64 = 100.0;

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Request {
    /// "list", or "kill" with `pid` and `signal`.
    #[serde(default)]
    pub op: String,
    #[serde(default)]
    pub pid: i64,
    /// TERM, the default, or KILL.
    #[serde(default)]
    pub signal: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Process {
    pub pid: i64,
    pub ppid: i64,
    pub user: String,
    pub name: String,
    pub command: String,
    pub state: String,
    /// Its share of one CPU since the last listing: a process busy on two
    /// CPUs is at 200.
    pub cpu_percent: f64,
    pub rss_bytes: i64,
    pub threads: i64,
    pub started: String,
    /// A process that cannot be stopped from here: the guest's init, and
    /// the agent.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub protected: bool,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Reply {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub processes: Vec<Process>,
    /// How many CPUs the guest has, and its memory.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cpus: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub memory_bytes: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
}

fn is_zero<T: Default + PartialEq>(n: &T) -> bool {
    *n == T::default()
}

/// Answers requests about the processes of this machine.
pub struct Server {
    last: Mutex<Option<(Instant, HashMap<i64, Sample>)>>,
}

#[derive(Clone, Copy)]
struct Sample {
    ticks: u64,
    start: u64,
}

impl Server {
    pub fn new() -> Self {
        Self {
            last: Mutex::new(None),
        }
    }

    /// Answers requests on `conn` until it closes.
    pub async fn serve<S: AsyncRead + AsyncWrite + Send + 'static>(&self, conn: S) {
        let (rd, mut wr) = tokio::io::split(conn);
        let mut lines = BufReader::new(rd).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let reply = match serde_json::from_str::<Request>(&line) {
                Ok(req) => self.handle(&req),
                Err(_) => Reply {
                    error: "a malformed request".into(),
                    ..Reply::default()
                },
            };
            let mut b = serde_json::to_vec(&reply).unwrap_or_default();
            b.push(b'\n');
            if wr.write_all(&b).await.is_err() {
                return;
            }
        }
    }

    pub fn handle(&self, req: &Request) -> Reply {
        let failed = |error: String| Reply {
            error,
            ..Reply::default()
        };
        match req.op.as_str() {
            "list" => match self.list() {
                Ok(processes) => Reply {
                    processes,
                    cpus: std::thread::available_parallelism().map_or(1, |n| n.get()),
                    memory_bytes: mem_total(),
                    ..Reply::default()
                },
                Err(e) => failed(e),
            },
            "kill" => match kill(req.pid, &req.signal) {
                Ok(()) => Reply::default(),
                Err(e) => failed(e),
            },
            op => failed(format!("unknown request {op}")),
        }
    }

    fn list(&self) -> Result<Vec<Process>, String> {
        let entries = fs::read_dir("/proc").map_err(|e| e.to_string())?;
        let boot = boot_time();
        let page = page_size();
        let mut users: HashMap<u32, String> = HashMap::new();
        let now = Instant::now();

        let mut last = self.last.lock().unwrap();
        let elapsed = last
            .as_ref()
            .map(|(at, _)| now.duration_since(*at).as_secs_f64());
        let mut next = HashMap::new();
        let mut out = vec![];
        for e in entries.flatten() {
            let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<i64>().ok()) else {
                continue;
            };
            let Some((mut p, smp)) = read_process(pid, boot, page, &mut users) else {
                continue;
            };
            next.insert(pid, smp);
            p.protected = protected(pid);
            // The same PID must be the same process: one that exited and
            // whose number was reused starts afresh.
            if let (Some((_, prev)), Some(elapsed)) = (last.as_ref(), elapsed)
                && let Some(prev) = prev.get(&pid)
                && prev.start == smp.start
                && elapsed > 0.0
            {
                p.cpu_percent =
                    smp.ticks.saturating_sub(prev.ticks) as f64 / CLOCK_TICKS / elapsed * 100.0;
            }
            out.push(p);
        }
        *last = Some((now, next));
        Ok(out)
    }
}

impl Default for Server {
    fn default() -> Self {
        Self::new()
    }
}

/// One process, or none if it has gone.
fn read_process(
    pid: i64,
    boot: SystemTime,
    page: i64,
    users: &mut HashMap<u32, String>,
) -> Option<(Process, Sample)> {
    use std::os::unix::fs::MetadataExt;

    let dir = format!("/proc/{pid}");
    let stat = fs::read(format!("{dir}/stat")).ok()?;
    // The name is in parentheses and may itself hold them and spaces, so
    // the fields are counted from the last closing one.
    let open = stat.iter().position(|&b| b == b'(')?;
    let close = stat.iter().rposition(|&b| b == b')')?;
    if close < open {
        return None;
    }
    let rest = String::from_utf8_lossy(&stat[close + 1..]).into_owned();
    // Fields from state, the third of stat's, on.
    let f: Vec<&str> = rest.split_whitespace().collect();
    if f.len() < 22 {
        return None;
    }
    let num = |i: usize| f[i].parse::<i64>().unwrap_or(0);
    let unum = |i: usize| f[i].parse::<u64>().unwrap_or(0);
    let start = unum(19);
    let mut p = Process {
        pid,
        ppid: num(1),
        name: String::from_utf8_lossy(&stat[open + 1..close]).into_owned(),
        state: f[0].to_string(),
        threads: num(17),
        rss_bytes: num(21) * page,
        started: rfc3339(boot + Duration::from_millis(start * 1000 / CLOCK_TICKS as u64)),
        ..Process::default()
    };
    if let Ok(cmd) = fs::read(format!("{dir}/cmdline")) {
        p.command = String::from_utf8_lossy(&cmd)
            .replace('\0', " ")
            .trim()
            .to_string();
    }
    if p.command.is_empty() {
        // A kernel thread has no command line.
        p.command = format!("[{}]", p.name);
    }
    if let Ok(md) = fs::metadata(&dir) {
        let uid = md.uid();
        p.user = users
            .entry(uid)
            .or_insert_with(|| User::name_of(uid).unwrap_or_else(|| uid.to_string()))
            .clone();
    }
    Some((
        p,
        Sample {
            ticks: unum(11) + unum(12),
            start,
        },
    ))
}

fn boot_time() -> SystemTime {
    let stat = fs::read_to_string("/proc/stat").unwrap_or_default();
    let secs = stat
        .lines()
        .find_map(|l| l.strip_prefix("btime "))
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0);
    UNIX_EPOCH + Duration::from_secs(secs)
}

fn mem_total() -> i64 {
    let info = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    info.lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<i64>().ok())
        .map_or(0, |kb| kb << 10)
}

fn page_size() -> i64 {
    // SAFETY: sysconf has no preconditions.
    unsafe { libc::sysconf(libc::_SC_PAGESIZE) as i64 }
}

/// The guest's init, and the agent: this process and the agent that started
/// it. Without any of them the environment is gone.
fn protected(pid: i64) -> bool {
    // SAFETY: getpid and getppid cannot fail.
    let (me, parent) = unsafe { (libc::getpid() as i64, libc::getppid() as i64) };
    pid <= 1 || pid == me || pid == parent
}

fn kill(pid: i64, signal: &str) -> Result<(), String> {
    if protected(pid) {
        return Err(
            "that process keeps the environment running, and cannot be stopped from here".into(),
        );
    }
    let sig = match signal {
        "" | "TERM" => libc::SIGTERM,
        "KILL" => libc::SIGKILL,
        other => return Err(format!("unknown signal {other:?}")),
    };
    // SAFETY: kill(2) of one process.
    if unsafe { libc::kill(pid as libc::pid_t, sig) } < 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::ESRCH) {
            return Err("no such process: it has already exited".into());
        }
        return Err(e.to_string());
    }
    Ok(())
}
