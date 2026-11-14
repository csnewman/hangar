//! The control channel, in the protocol `hangar_vm::agent` speaks: the agent
//! dials the host on vsock port 8100, says hello, and answers what the host
//! asks -- ping, info, a command to run, the time to set -- for as long as
//! the guest runs.
//!
//! The agent dials out, so the host needs no address for the guest and
//! nothing waits for the guest's network: an environment still booting, or
//! cut off from the network, is still reachable. The host end goes away
//! when the environment is suspended and comes back, as another process,
//! when it is resumed; from in here that is a connection that ends and a
//! host that answers again later, so the agent dials again.
//!
//! A session is JSON lines both ways. Each request is answered when it is
//! done, with its ID, so a command that runs for minutes does not hold up a
//! ping sent after it.

use std::io;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::user;

pub const PORT: u32 = 8100;

/// The protocol's version, which the host checks: the agent and the host
/// are built at different times.
const PROTOCOL_VERSION: i32 = 1;

/// How long the agent waits between tries at the host.
#[cfg(target_os = "linux")]
const RETRY: Duration = Duration::from_millis(500);

/// How long the agent fails to reach the host before it says so on the
/// console, the one log the host has of a guest it cannot reach.
#[cfg(target_os = "linux")]
const UNREACHED_AFTER: Duration = Duration::from_secs(10);

/// How long a command's output is still collected after it exits, for
/// processes it left behind to finish writing what they were writing. The
/// answer is due when the command exits, not when everything that inherited
/// its output -- a desktop, a server it started -- has closed it.
const OUTPUT_GRACE: Duration = Duration::from_millis(500);

/// The first line the agent sends. Everything in it is advisory: the host
/// knows the guest by the connection, not by what it says.
#[derive(Serialize)]
struct Hello {
    kind: &'static str,
    version: i32,
    hostname: String,
    kernel: String,
    /// How long the guest took to reach the agent, from the kernel's clock.
    boot_micros: i64,
    /// The kernel's page size, the unit of /proc/vmstat's counts.
    page_size: i64,
}

#[derive(Deserialize)]
pub struct Request {
    pub id: u64,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub cmd: Vec<String>,
    /// The wall-clock time for set_clock.
    #[serde(default)]
    pub unix_nanos: i64,
}

/// The answer to a request. `err` is set when it could not be carried out
/// at all; a command that ran and failed gives its status in `code`.
#[derive(Debug, Default, Serialize)]
pub struct Response {
    pub id: u64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub err: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stdout: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stderr: String,
    pub code: i32,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub info: String,
}

/// Keeps the agent connected to the host for as long as the guest runs.
#[cfg(target_os = "linux")]
pub async fn run() {
    use std::time::Instant;

    let mut failing: Option<Instant> = None;
    let mut said: Option<Instant> = None;
    loop {
        let dialed =
            tokio::task::spawn_blocking(|| crate::vsock::connect(crate::vsock::HOST, PORT))
                .await
                .unwrap_or_else(|e| Err(io::Error::other(e)));
        match dialed {
            // Expected while the host is not listening: in early boot, and
            // while the environment is being restored.
            Err(e) => {
                let now = Instant::now();
                let since = *failing.get_or_insert(now);
                if now - since > UNREACHED_AFTER
                    && said.is_none_or(|s| now - s > Duration::from_secs(60))
                {
                    said = Some(now);
                    console(&format!(
                        "hangar-agent: cannot reach the host for {}s: {e}",
                        (now - since).as_secs()
                    ));
                }
            }
            Ok(conn) => {
                failing = None;
                serve(conn).await;
            }
        }
        tokio::time::sleep(RETRY).await;
    }
}

/// Says hello on `conn` and answers its requests until the host hangs up.
pub async fn serve<S: AsyncRead + AsyncWrite + Send + 'static>(conn: S) {
    let (rd, mut wr) = tokio::io::split(conn);
    let hello = Hello {
        kind: "hello",
        version: PROTOCOL_VERSION,
        hostname: hostname(),
        kernel: kernel(),
        boot_micros: uptime_micros(),
        // SAFETY: sysconf has no preconditions.
        page_size: unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as i64,
    };
    let mut line = serde_json::to_vec(&hello).unwrap_or_default();
    line.push(b'\n');
    if wr.write_all(&line).await.is_err() {
        return;
    }
    let (replies, mut out) = mpsc::unbounded_channel::<Response>();
    let writer = crate::serve::AbortOnDrop(tokio::spawn(async move {
        while let Some(r) = out.recv().await {
            let mut b = serde_json::to_vec(&r).unwrap_or_default();
            b.push(b'\n');
            if wr.write_all(&b).await.is_err() {
                return;
            }
        }
    }));
    let mut lines = BufReader::new(rd).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(req) = serde_json::from_str::<Request>(&line) else {
            continue;
        };
        let replies = replies.clone();
        tokio::spawn(async move {
            let _ = replies.send(handle(req).await);
        });
    }
    drop(writer);
}

pub async fn handle(req: Request) -> Response {
    let mut resp = Response {
        id: req.id,
        ..Response::default()
    };
    match req.kind.as_str() {
        "ping" => {}
        "info" => {
            resp.info = format!("{} {} up {}us", hostname(), kernel(), uptime_micros());
        }
        "exec" if req.cmd.is_empty() => resp.err = "exec with no command".into(),
        "exec" => execute(&req.cmd, &mut resp).await,
        "set_clock" => {
            if let Err(e) = set_clock(req.unix_nanos) {
                resp.err = format!("setting the clock: {e}");
            }
        }
        kind => resp.err = format!("unknown request kind {kind:?}"),
    }
    resp
}

/// Runs a command as the agent, at the user's OOM score, and gives its
/// output and status.
async fn execute(argv: &[String], resp: &mut Response) {
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    user::prepare(&mut cmd, None, user::oom::USER);
    let mut child = match tokio::process::Command::from(cmd).spawn() {
        Ok(c) => c,
        Err(e) => {
            resp.err = format!("{}: {e}", argv[0]);
            return;
        }
    };
    let (out, err) = (Arc::new(Mutex::new(vec![])), Arc::new(Mutex::new(vec![])));
    let mut readers = (
        crate::serve::AbortOnDrop(tokio::spawn(drain(child.stdout.take(), out.clone()))),
        crate::serve::AbortOnDrop(tokio::spawn(drain(child.stderr.take(), err.clone()))),
    );
    let status = child.wait().await;
    let _ = tokio::time::timeout(OUTPUT_GRACE, async {
        let _ = (&mut readers.0.0).await;
        let _ = (&mut readers.1.0).await;
    })
    .await;
    drop(readers);
    resp.stdout = String::from_utf8_lossy(&out.lock().unwrap()).into_owned();
    resp.stderr = String::from_utf8_lossy(&err.lock().unwrap()).into_owned();
    match status {
        // A command that ran and failed is a result, not an error. One a
        // signal ended has no code, which -1 says.
        Ok(s) => resp.code = s.code().unwrap_or(-1),
        Err(e) => resp.err = e.to_string(),
    }
}

/// Reads all of `from` into `buf` as it arrives, so what came before the
/// grace ran out is kept.
async fn drain(from: Option<impl AsyncRead + Unpin>, buf: Arc<Mutex<Vec<u8>>>) {
    let Some(mut from) = from else {
        return;
    };
    let mut chunk = vec![0; 32 << 10];
    while let Ok(n @ 1..) = from.read(&mut chunk).await {
        buf.lock().unwrap().extend_from_slice(&chunk[..n]);
    }
}

/// Sets the wall clock. The host sends its own time whenever it connects.
fn set_clock(unix_nanos: i64) -> io::Result<()> {
    let ts = libc::timespec {
        tv_sec: (unix_nanos.div_euclid(1_000_000_000)) as _,
        tv_nsec: (unix_nanos.rem_euclid(1_000_000_000)) as _,
    };
    // SAFETY: a timespec for the realtime clock.
    if unsafe { libc::clock_settime(libc::CLOCK_REALTIME, &ts) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn hostname() -> String {
    read_trimmed("/proc/sys/kernel/hostname")
}

fn kernel() -> String {
    read_trimmed("/proc/sys/kernel/osrelease")
}

fn read_trimmed(path: &str) -> String {
    std::fs::read_to_string(path)
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unknown".into())
}

/// How long the guest has been running, from the kernel's own clock: how
/// long it took to become reachable, not how long the whole run took.
fn uptime_micros() -> i64 {
    std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
        .map_or(0, |secs| (secs * 1e6) as i64)
}

/// Writes a line to the kernel's log, which is the machine's console, else
/// to standard error.
pub fn console(line: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open("/dev/kmsg")
        && f.write_all(format!("{line}\n").as_bytes()).is_ok()
    {
        return;
    }
    eprintln!("{line}");
}
