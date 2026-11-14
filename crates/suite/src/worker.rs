//! A worker for the server under test: the real one (`hangar_worker`), so
//! its registration, reports, placement and tunnel are the real protocol,
//! on a runtime of the suite's whose machines are what they are asked to be
//! at once, and whose guests answer every stream with a greeting naming its
//! port, then an echo of what they are sent.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use hangar_api::{DesiredState, EnvironmentSpec, ObservedEnvironment, Phase};
use hangar_vm::log::Log;
use hangar_worker::config::Config;
use hangar_worker::{Runtime, Worker as RealWorker};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::Notify;

/// The vsock port the agent reaches the guest's own TCP ports on.
const GUEST_PORTS: u32 = 108;

/// The vsock port the agent serves terminals on.
const TERMINALS: u32 = 101;

/// The vsock port the agent serves SSH on.
const SSH: u32 = 107;

/// The guest ports the suite's guests serve a web page on.
pub const WEB_PORTS: [u16; 2] = [80, 8080];

/// The label a template's placement names to land on the suite's workers.
pub const LABEL: (&str, &str) = ("runtime", "fake");

/// A worker, connected to its server until it goes.
pub struct Worker {
    task: Option<tokio::task::JoinHandle<Result<(), String>>>,
    machines: Machines,
    dir: PathBuf,
}

impl Worker {
    /// Starts a worker named name for the server at url (its own address:
    /// the worker reaches it by number), presenting the bootstrap token.
    pub fn start(url: &str, name: &str) -> Worker {
        Worker::start_with_token(url, name, crate::BOOTSTRAP_TOKEN)
    }

    /// As start, presenting token as the bootstrap token.
    pub fn start_with_token(url: &str, name: &str, token: &str) -> Worker {
        let dir = std::env::temp_dir().join(crate::unique("hangar-suite-worker"));
        std::fs::create_dir_all(&dir).expect("the worker's directory");
        std::fs::write(dir.join("token"), format!("{token}\n")).expect("the bootstrap token");
        let cfg = Config::parse(&format!(
            "server:\n  url: {url}\n  token_file: {}\n  credential_file: {}\n\
             node:\n  name: {name}\n  labels:\n    {}: {}\n\
             storage:\n  environments: {}\n  files: {}\n\
             runtime: simulated\n",
            dir.join("token").display(),
            dir.join("credential").display(),
            LABEL.0,
            LABEL.1,
            dir.join("env").display(),
            dir.join("files").display(),
        ))
        .expect("the worker's configuration");
        let machines = Machines::default();
        let worker = RealWorker::new(cfg, machines.clone(), Log::default(), None)
            .expect("making the worker");
        let task = tokio::spawn(async move { worker.run().await.map_err(|e| e.to_string()) });
        Worker {
            task: Some(task),
            machines,
            dir,
        }
    }

    /// How the worker's run ended, if it ends within 20 seconds: an error
    /// says why, such as the server rejecting its credential.
    pub async fn ended(&mut self) -> Option<Result<(), String>> {
        let task = self.task.as_mut()?;
        let out = tokio::time::timeout(std::time::Duration::from_secs(20), task)
            .await
            .ok()?;
        self.task = None;
        Some(out.unwrap_or_else(|e| Err(e.to_string())))
    }

    /// What the worker has been asked of an environment, last.
    pub fn desired(&self, id: &str) -> Option<DesiredState> {
        self.machines.desired(id)
    }

    /// The last spec the worker was given for an environment, deleted or
    /// not.
    pub fn spec(&self, id: &str) -> Option<EnvironmentSpec> {
        self.machines.specs.lock().unwrap().get(id).cloned()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Machines that are as they are asked to be.
#[derive(Clone, Default)]
struct Machines {
    held: Arc<Mutex<BTreeMap<String, DesiredState>>>,
    specs: Arc<Mutex<BTreeMap<String, EnvironmentSpec>>>,
    terminals: Terminals,
    changed: Arc<Notify>,
}

impl Machines {
    fn desired(&self, id: &str) -> Option<DesiredState> {
        self.held.lock().unwrap().get(id).copied()
    }
}

impl Runtime for Machines {
    type Conn = DuplexStream;

    fn apply(&self, spec: EnvironmentSpec) {
        self.specs
            .lock()
            .unwrap()
            .insert(spec.id.clone(), spec.clone());
        let mut held = self.held.lock().unwrap();
        if spec.desired == DesiredState::Deleted {
            held.remove(&spec.id);
        } else {
            held.insert(spec.id, spec.desired);
        }
        drop(held);
        self.changed.notify_one();
    }

    fn observe(&self) -> Vec<ObservedEnvironment> {
        self.held
            .lock()
            .unwrap()
            .iter()
            .map(|(id, desired)| ObservedEnvironment {
                id: id.clone(),
                phase: match desired {
                    DesiredState::Running => Phase::Running,
                    DesiredState::Suspended => Phase::Suspended,
                    _ => Phase::Stopped,
                },
                reason: String::new(),
                progress: None,
                stats: None,
                image_digest: String::new(),
                image_pinned: false,
                image_update: None,
                image_rollback: None,
            })
            .collect()
    }

    async fn changed(&self) {
        self.changed.notified().await
    }

    async fn dial(&self, id: &str, port: u32) -> io::Result<DuplexStream> {
        if self.desired(id) != Some(DesiredState::Running) {
            return Err(io::Error::other(format!("environment {id} is not running")));
        }
        let (ours, mut guest) = tokio::io::duplex(1 << 16);
        if port == GUEST_PORTS {
            tokio::spawn(guest_ports(guest));
            return Ok(ours);
        }
        if port == SSH {
            tokio::spawn(ssh(guest));
            return Ok(ours);
        }
        if port == TERMINALS {
            tokio::spawn(self.terminals.clone().serve(id.to_string(), guest));
            return Ok(ours);
        }
        tokio::spawn(async move {
            let _ = guest.write_all(format!("port {port}\n").as_bytes()).await;
            let mut buf = [0u8; 4096];
            loop {
                match guest.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => {
                        if guest.write_all(&buf[..n]).await.is_err() {
                            return;
                        }
                    }
                }
            }
        });
        Ok(ours)
    }

    /// It says it is libkrun, a monitor the spec knows, and that it
    /// suspends, so the server lets environments be suspended on it.
    fn monitor(&self) -> (String, bool) {
        ("krun".into(), true)
    }
}

/// Answers one connection to the guest's TCP ports as the agent does
/// (crates/agent/src/guestport.rs): the port, two bytes big-endian, then a
/// byte, 0 connected or 1 refused. A WEB_PORTS port answers each HTTP
/// request with a page naming the port and the request's host and path.
async fn guest_ports(mut conn: DuplexStream) {
    let mut port = [0u8; 2];
    if conn.read_exact(&mut port).await.is_err() {
        return;
    }
    let port = u16::from_be_bytes(port);
    if !WEB_PORTS.contains(&port) {
        let _ = conn.write_all(&[1]).await;
        return;
    }
    if conn.write_all(&[0]).await.is_err() {
        return;
    }
    let mut request = Vec::new();
    let mut buf = [0u8; 4096];
    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
        match conn.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => request.extend_from_slice(&buf[..n]),
        }
    }
    let text = String::from_utf8_lossy(&request);
    let path = text.split_whitespace().nth(1).unwrap_or("/");
    let host = text
        .lines()
        .find_map(|l| {
            l.strip_prefix("Host: ")
                .or_else(|| l.strip_prefix("host: "))
        })
        .unwrap_or("");
    let page = format!("guest port {port}, {host}, {path}");
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{page}",
        page.len()
    );
    let _ = conn.write_all(response.as_bytes()).await;
}

/// Terminal sessions as the agent keeps them (crates/agent/src/terminal.rs,
/// internal/terminal): each environment's, outliving the connections
/// attached to them. Their shells write back what is typed, and exit when
/// typed "exit".
#[derive(Clone, Default)]
struct Terminals {
    sessions: Arc<Mutex<BTreeMap<String, Vec<serde_json::Value>>>>,
}

impl Terminals {
    /// Answers one connection: a JSON request line, a JSON reply line, then
    /// for an attach, frames.
    async fn serve(self, env: String, conn: DuplexStream) {
        use tokio::io::AsyncBufReadExt;
        let mut conn = tokio::io::BufReader::new(conn);
        let mut line = String::new();
        if conn.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        let req: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
        let reply = |v: serde_json::Value| format!("{v}\n");
        match req["op"].as_str().unwrap_or("") {
            "list" => {
                let sessions = self.sessions.lock().unwrap().get(&env).cloned();
                let _ = conn
                    .write_all(
                        reply(serde_json::json!({ "sessions": sessions.unwrap_or_default() }))
                            .as_bytes(),
                    )
                    .await;
            }
            "close" => {
                let id = req["session"].as_str().unwrap_or("").to_string();
                if let Some(list) = self.sessions.lock().unwrap().get_mut(&env) {
                    list.retain(|s| s["id"] != id.as_str());
                }
                let _ = conn
                    .write_all(reply(serde_json::json!({})).as_bytes())
                    .await;
            }
            "attach" => self.attach(env, req, conn).await,
            op => {
                let _ = conn
                    .write_all(
                        reply(serde_json::json!({ "err": format!("no op {op:?}") })).as_bytes(),
                    )
                    .await;
            }
        }
    }

    async fn attach(
        self,
        env: String,
        req: serde_json::Value,
        mut conn: tokio::io::BufReader<DuplexStream>,
    ) {
        let (cols, rows) = (
            req["cols"].as_u64().unwrap_or(80) as u16,
            req["rows"].as_u64().unwrap_or(24) as u16,
        );
        let session = {
            let mut all = self.sessions.lock().unwrap();
            let list = all.entry(env.clone()).or_default();
            match req["session"].as_str().filter(|s| !s.is_empty()) {
                Some(id) => list.iter().find(|s| s["id"] == id).cloned(),
                None => {
                    let s = serde_json::json!({
                        "id": format!("s{}", list.len() + 1),
                        "title": "shell",
                        "created": "2026-11-14T00:00:00Z",
                        "clients": 1,
                        "cols": cols,
                        "rows": rows,
                    });
                    list.push(s.clone());
                    Some(s)
                }
            }
        };
        let Some(session) = session else {
            let _ = conn.write_all(b"{\"err\":\"no such session\"}\n").await;
            return;
        };
        let id = session["id"].as_str().unwrap_or("").to_string();
        let reply = serde_json::json!({ "session": session, "history": 0, "owner": true });
        if conn
            .write_all(format!("{reply}\n").as_bytes())
            .await
            .is_err()
        {
            return;
        }
        let mut size = Vec::with_capacity(7);
        size.extend_from_slice(&cols.to_be_bytes());
        size.extend_from_slice(&rows.to_be_bytes());
        size.extend_from_slice(&1u16.to_be_bytes());
        size.push(1);
        if frame(&mut conn, b's', &size).await.is_err() {
            return;
        }
        loop {
            let mut head = [0u8; 5];
            if conn.read_exact(&mut head).await.is_err() {
                return;
            }
            let len = u32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize;
            let mut payload = vec![0u8; len];
            if conn.read_exact(&mut payload).await.is_err() {
                return;
            }
            if head[0] != b'i' {
                continue;
            }
            if frame(&mut conn, b'o', &payload).await.is_err() {
                return;
            }
            if payload.starts_with(b"exit") {
                let _ = frame(&mut conn, b'x', &0u32.to_be_bytes()).await;
                if let Some(list) = self.sessions.lock().unwrap().get_mut(&env) {
                    list.retain(|s| s["id"] != id.as_str());
                }
                return;
            }
        }
    }
}

/// Writes one terminal frame: its type, its length (four bytes big-endian)
/// and its payload.
async fn frame(
    conn: &mut tokio::io::BufReader<DuplexStream>,
    kind: u8,
    payload: &[u8],
) -> io::Result<()> {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(kind);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    conn.get_mut().write_all(&out).await
}

/// Holds one SSH connection as the agent's server does
/// (crates/agent/src/sshd.rs): anyone the gateway brings is let in, and
/// each command is answered "ran <command>" with exit status 0.
async fn ssh(conn: DuplexStream) {
    use russh::keys::PrivateKey;
    use russh::keys::ssh_key::private::Ed25519Keypair;
    let config = russh::server::Config {
        keys: vec![PrivateKey::from(Ed25519Keypair::from_seed(&[7; 32]))],
        inactivity_timeout: None,
        ..russh::server::Config::default()
    };
    let guest = GuestSsh {
        channels: BTreeMap::new(),
    };
    if let Ok(running) = russh::server::run_stream(Arc::new(config), conn, guest).await {
        let _ = running.await;
    }
}

struct GuestSsh {
    channels: BTreeMap<russh::ChannelId, russh::Channel<russh::server::Msg>>,
}

impl russh::server::Handler for GuestSsh {
    type Error = russh::Error;

    async fn auth_none(&mut self, _user: &str) -> Result<russh::server::Auth, Self::Error> {
        Ok(russh::server::Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: russh::Channel<russh::server::Msg>,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        id: russh::ChannelId,
        data: &[u8],
        session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(id)?;
        if let Some(ch) = self.channels.remove(&id) {
            let out = format!("ran {}\n", String::from_utf8_lossy(data));
            tokio::spawn(async move {
                let _ = ch.data(out.as_bytes()).await;
                let _ = ch.exit_status(0).await;
                let _ = ch.eof().await;
                let _ = ch.close().await;
            });
        }
        Ok(())
    }
}
