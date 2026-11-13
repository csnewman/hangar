//! The host end of the control channel to an environment's agent, in the
//! protocol crates/agent's control module speaks.
//!
//! What the channel proves, and what it does not: the context ID a session
//! reports is the host's own allocation, the socket a connection arrived on
//! being the guest's alone (vsock::HybridListener), so one environment
//! cannot impersonate another, as long as the host resolves identity
//! through its own records and treats everything in Hello as advisory. It
//! does not prove which process is talking: any code in the environment can
//! open the port and speak the protocol. The machine is the boundary, so
//! this channel must never carry authority that the environment's own
//! workload should not already have.
//!
//! The guest dials out, so the host needs no address for a guest, and an
//! environment that is wedged, still booting or cut off from the network
//! is still reachable. The protocol is JSON lines both ways; every request
//! carries an ID its reply repeats, so replies may come out of order.

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::oneshot;

use crate::err;
use crate::vsock::HybridListener;

/// The vsock port the host listens on and the agent dials.
pub const PORT: u32 = 8100;

/// Sent in Hello and checked here: the agent and the host are built at
/// different times and must be able to say so.
pub const PROTOCOL_VERSION: i64 = 1;

/// The first line the agent sends, unasked. Every field is ADVISORY: it is
/// whatever the guest chose to say about itself, and nothing may key an
/// environment off it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Hello {
    pub kind: String,
    pub version: i64,
    #[serde(default)]
    pub hostname: String,
    #[serde(default)]
    pub kernel: String,
    /// How long the guest took to reach the agent, by the kernel's own
    /// clock, so without the monitor's and the host's start.
    #[serde(default)]
    pub boot_micros: i64,
    /// The guest kernel's page size, the unit of /proc/vmstat's counts.
    #[serde(default)]
    pub page_size: i64,
}

#[derive(Debug, Clone, Default, Serialize)]
struct Request<'a> {
    id: u64,
    kind: &'a str,
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    cmd: &'a [String],
    /// The wall-clock time for set_clock.
    #[serde(skip_serializing_if = "is_zero")]
    unix_nanos: i64,
}

fn is_zero(n: &i64) -> bool {
    *n == 0
}

/// The agent's reply. err is set when the request could not be carried out
/// at all; a command that ran and failed gives its status in code.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Response {
    pub id: u64,
    #[serde(default)]
    pub err: String,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    #[serde(default)]
    pub code: i32,
    #[serde(default)]
    pub info: String,
}

/// Listens for one environment's agent dialling out.
pub struct Server {
    l: HybridListener,
}

impl Server {
    /// Accepts agent connections from a monitor that carries vsock on unix
    /// sockets. base is the monitor's vsock socket path and cid the context
    /// ID the host allocated for the guest.
    pub fn listen_hybrid(base: &Path, cid: u32) -> io::Result<Server> {
        Ok(Server {
            l: HybridListener::listen(base, PORT, cid)?,
        })
    }

    /// Waits for an agent to connect and reads its Hello.
    ///
    /// A connection that ends before its Hello is one the agent gave up on,
    /// as it does when a resumed guest's vsock is reset under it, and the
    /// agent dials again; this waits for that, within the same timeout.
    pub async fn accept(&self, timeout: Option<Duration>) -> io::Result<Session> {
        let deadline = timeout.map(|t| tokio::time::Instant::now() + t);
        loop {
            let left = deadline.map(|d| d.saturating_duration_since(tokio::time::Instant::now()));
            let (conn, cid) = self.l.accept(left).await?;
            match Session::start(conn, cid, left).await {
                Ok(s) => return Ok(s),
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => continue,
                Err(e) => return Err(e),
            }
        }
    }
}

#[derive(Default)]
struct Pending {
    next: u64,
    waiting: HashMap<u64, oneshot::Sender<Response>>,
    /// Why the connection stopped carrying replies, once it has.
    broken: Option<String>,
}

/// A live connection to one environment's agent.
///
/// Requests are independent: several may be outstanding at once, and each
/// reply is matched to its request by ID. A long-running command holds up
/// nothing but its own caller, and a request that times out leaves nothing
/// behind to be mistaken for the reply to the next.
pub struct Session {
    /// The guest's context ID, the host's own allocation: the only
    /// trustworthy identifier on the connection. CIDs are recycled as
    /// environments go, so a caller pairs it with the allocation it made.
    pub cid: u32,
    /// What the agent said about itself. Advisory.
    pub hello: Hello,
    writer: tokio::sync::Mutex<OwnedWriteHalf>,
    pending: Arc<Mutex<Pending>>,
    reader: tokio::task::JoinHandle<()>,
}

impl Session {
    async fn start(
        conn: tokio::net::UnixStream,
        cid: u32,
        timeout: Option<Duration>,
    ) -> io::Result<Session> {
        let (rd, wr) = conn.into_split();
        let mut rd = BufReader::new(rd);
        let mut line = String::new();
        let read = rd.read_line(&mut line);
        let n = match timeout {
            Some(t) => tokio::time::timeout(t, read)
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "reading hello: timed out"))?,
            None => read.await,
        }
        .map_err(|e| io::Error::new(e.kind(), format!("reading hello: {e}")))?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "reading hello: EOF",
            ));
        }
        let hello: Hello =
            serde_json::from_str(&line).map_err(|e| err(format!("malformed hello: {e}")))?;
        if hello.kind != "hello" {
            return Err(err(format!(
                "first message was {:?}, want \"hello\"",
                hello.kind
            )));
        }
        if hello.version != PROTOCOL_VERSION {
            return Err(err(format!(
                "agent speaks protocol {}, host speaks {PROTOCOL_VERSION}",
                hello.version
            )));
        }
        let pending = Arc::new(Mutex::new(Pending {
            next: 1,
            ..Default::default()
        }));
        // Hands each reply to the request waiting for it, until the
        // connection ends. A reply nobody is waiting for belongs to a
        // request that timed out, and is dropped.
        let reader = {
            let pending = pending.clone();
            tokio::spawn(async move {
                let mut lines = rd.lines();
                let why = loop {
                    match lines.next_line().await {
                        Ok(Some(l)) => {
                            let Ok(r) = serde_json::from_str::<Response>(&l) else {
                                continue;
                            };
                            if let Some(tx) = pending.lock().unwrap().waiting.remove(&r.id) {
                                let _ = tx.send(r);
                            }
                        }
                        Ok(None) => break "EOF".to_string(),
                        Err(e) => break e.to_string(),
                    }
                };
                let mut p = pending.lock().unwrap();
                p.broken = Some(why);
                p.waiting.clear();
            })
        };
        Ok(Session {
            cid,
            hello,
            writer: tokio::sync::Mutex::new(wr),
            pending,
            reader,
        })
    }

    /// Sends a request and waits for its reply.
    async fn request(
        &self,
        kind: &str,
        cmd: &[String],
        unix_nanos: i64,
        timeout: Duration,
    ) -> io::Result<Response> {
        let (tx, rx) = oneshot::channel();
        let id = {
            let mut p = self.pending.lock().unwrap();
            if let Some(why) = &p.broken {
                return Err(err(format!(
                    "sending {kind}: the agent connection ended: {why}"
                )));
            }
            let id = p.next;
            p.next += 1;
            p.waiting.insert(id, tx);
            id
        };
        let forget = || {
            self.pending.lock().unwrap().waiting.remove(&id);
        };
        let mut line = serde_json::to_vec(&Request {
            id,
            kind,
            cmd,
            unix_nanos,
        })
        .map_err(io::Error::other)?;
        line.push(b'\n');
        let sent = tokio::time::timeout(timeout, async {
            let mut w = self.writer.lock().await;
            w.write_all(&line).await
        })
        .await;
        match sent {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                forget();
                return Err(err(format!("sending {kind}: {e}")));
            }
            Err(_) => {
                forget();
                return Err(err(format!("sending {kind}: no room in {timeout:?}")));
            }
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(r)) => Ok(r),
            Ok(Err(_)) => {
                let why = self
                    .pending
                    .lock()
                    .unwrap()
                    .broken
                    .clone()
                    .unwrap_or_default();
                Err(err(format!(
                    "awaiting reply to {kind}: the agent connection ended: {why}"
                )))
            }
            Err(_) => {
                forget();
                Err(err(format!(
                    "awaiting reply to {kind}: no answer in {timeout:?}"
                )))
            }
        }
    }

    /// Checks the agent is answering.
    pub async fn ping(&self, timeout: Duration) -> io::Result<()> {
        let r = self.request("ping", &[], 0, timeout).await?;
        if !r.err.is_empty() {
            return Err(err(format!("ping: {}", r.err)));
        }
        Ok(())
    }

    /// Sets the guest's wall clock to t.
    ///
    /// A guest has no clock of its own worth trusting: it boots at whatever
    /// time its image was built with, and a restored guest carries on from
    /// the moment it was suspended, however long ago that was. The host's
    /// clock is the reference, so the host sets the guest's every time the
    /// agent connects, which is both after boot and after every resume.
    pub async fn set_clock(&self, t: SystemTime, timeout: Duration) -> io::Result<()> {
        let ns = t.duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos() as i64;
        let r = self.request("set_clock", &[], ns, timeout).await?;
        if !r.err.is_empty() {
            return Err(err(format!("setting the clock: {}", r.err)));
        }
        Ok(())
    }

    /// Runs a command in the environment and gives its output. A command
    /// that runs and exits non-zero is not an error here: that is the
    /// response's code. An error means the command could not be run.
    pub async fn exec(&self, timeout: Duration, cmd: &[&str]) -> io::Result<Response> {
        let cmd: Vec<String> = cmd.iter().map(|s| s.to_string()).collect();
        let r = self.request("exec", &cmd, 0, timeout).await?;
        if !r.err.is_empty() {
            return Err(err(format!("exec {cmd:?}: {}", r.err)));
        }
        Ok(r)
    }

    /// The agent's answer to info.
    pub async fn info(&self, timeout: Duration) -> io::Result<Response> {
        self.request("info", &[], 0, timeout).await
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.reader.abort();
    }
}
