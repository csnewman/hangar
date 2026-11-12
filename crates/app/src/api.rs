//! A server's client API, /api/v1 (internal/clientapi/openapi.yaml): what
//! the app reads and asks of a server place. Blocking, so each place's
//! connection runs on a thread of its own.

use std::io::{BufRead, BufReader, Read};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The API version the app speaks.
pub const VERSION: &str = "v1";

/// How long a read on the event stream may wait. The server sends a
/// comment every 30 seconds, so a stream silent for longer is dead.
const STREAM_READ: Duration = Duration::from_secs(75);

/// How long any other request may take.
const CALL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Version {
    pub versions: Vec<String>,
    pub server: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Me {
    pub id: String,
    pub username: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub admin: bool,
    /// Where the server's SSH gateway is, if it has one.
    #[serde(default)]
    pub ssh: Option<SshGateway>,
    /// What follows env<short> in the hosts environments' own web servers
    /// are reached on: a name and a hyphen before it reach a server by that
    /// name, a number that port. None when the server has no public URL.
    #[serde(default)]
    pub environment_host_suffix: Option<String>,
}

/// Where environments are reached over SSH: ssh <environment>@<host> -p
/// <port>.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SshGateway {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Environment {
    pub id: String,
    pub short_id: String,
    pub name: String,
    pub owner: String,
    pub template: String,
    /// pending, starting, running, stopping, stopped, suspending,
    /// suspended, failed, deleting; more may come.
    pub phase: String,
    pub desired: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub progress: Option<StartProgress>,
    #[serde(default)]
    pub worker: Option<String>,
    #[serde(default)]
    pub worker_online: Option<bool>,
    #[serde(default)]
    pub cpus: Option<i64>,
    #[serde(default)]
    pub memory_mib: Option<i64>,
    /// The folder an editor opens in it: its first project.
    #[serde(default)]
    pub editor_path: Option<String>,
    /// Whether anyone may reach its own web servers without signing in to
    /// Hangar.
    #[serde(default)]
    pub ports_public: bool,
    /// The names of its own web servers its template lists.
    #[serde(default)]
    pub web_names: Vec<String>,
    #[serde(default)]
    pub task: Option<Task>,
    #[serde(default)]
    pub agents: Vec<AgentSession>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// What a running environment uses, as its worker last measured.
#[derive(Debug, Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct Stats {
    /// Of its own vCPUs: 100 is all of them busy.
    pub cpu_percent: f64,
    pub memory_used_mib: i64,
    pub memory_total_mib: i64,
    pub memory_cached_mib: i64,
    pub host_memory_mib: i64,
    pub swap_total_mib: i64,
    pub swap_used_mib: i64,
    pub disk_used_bytes: i64,
    pub disk_read_bps: f64,
    pub disk_write_bps: f64,
    pub net_rx_bps: f64,
    pub net_tx_bps: f64,
}

/// The processes in an environment, and what its machine has.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct ProcessList {
    pub cpus: i64,
    pub memory_bytes: i64,
    pub processes: Vec<Process>,
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct Process {
    pub pid: i64,
    pub ppid: i64,
    pub user: String,
    pub name: String,
    pub command: String,
    /// As /proc gives it: R running, S sleeping, D waiting on I/O, Z a
    /// zombie.
    pub state: String,
    /// Its share of one CPU: busy on two is 200.
    pub cpu_percent: f64,
    pub rss_bytes: i64,
    pub threads: i64,
    pub started: Option<DateTime<Utc>>,
    /// The guest's init or its agent, which cannot be stopped from here.
    pub protected: bool,
}

/// Something done to an environment, from the audit log.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ActivityEvent {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub actor: Actor,
    pub action: String,
    pub target: Target,
    #[serde(default)]
    pub details: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Actor {
    /// user, worker or system; more may come.
    pub kind: String,
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Target {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StartProgress {
    pub step: String,
    #[serde(default)]
    pub done: Option<i64>,
    #[serde(default)]
    pub total: Option<i64>,
    #[serde(default)]
    pub unit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub text: String,
    /// agent or user; more may come.
    pub set_by: String,
    pub set_at: DateTime<Utc>,
    #[serde(default)]
    pub done_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSession {
    pub id: String,
    /// claude or codex; more may come.
    pub agent: String,
    #[serde(default)]
    pub terminal: Option<String>,
    /// asking, finished, working, idle, ended; more may come, taken as
    /// working.
    pub state: String,
    #[serde(default)]
    pub step: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    pub since: DateTime<Utc>,
    #[serde(default)]
    pub needs_you: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerminalSession {
    pub id: String,
    /// What the shell or the program in it last set as the window title.
    #[serde(default)]
    pub title: String,
    pub created_at: DateTime<Utc>,
    /// How many windows are attached to it.
    #[serde(default)]
    pub clients: i64,
    #[serde(default)]
    pub cols: i64,
    #[serde(default)]
    pub rows: i64,
}

/// Part of one of an environment's logs, which only grows: text from
/// start, of a log size bytes long so far, read on from next.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LogPart {
    pub start: i64,
    pub next: i64,
    pub size: i64,
    #[serde(default)]
    pub text: String,
    /// There is no such log yet: the machine has not been started, or has
    /// no GPU.
    #[serde(default)]
    pub missing: bool,
}

/// Why a request failed.
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// The token is missing, wrong or revoked.
    Unauthorized,
    /// The server answered with an error.
    Status(u16, String),
    /// The server could not be reached, or the connection failed.
    Transport(String),
    /// The server's answer was not what this API says.
    Decode(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unauthorized => write!(f, "not signed in"),
            Error::Status(code, msg) if msg.is_empty() => write!(f, "the server answered {code}"),
            Error::Status(code, msg) => write!(f, "{msg} ({code})"),
            Error::Transport(msg) => write!(f, "{msg}"),
            Error::Decode(msg) => write!(f, "an answer the app does not understand: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

/// One change on the event stream.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// An environment, all of it.
    Environment(Box<Environment>),
    /// An environment deleted, or out of reach.
    Removed(String),
    /// Every environment has been sent; changes follow.
    Synced,
}

/// A server's client API, signed in with an access token.
#[derive(Clone)]
pub struct Client {
    base: String,
    token: String,
    agent: ureq::Agent,
    stream: ureq::Agent,
}

impl Client {
    /// A client for the server at url (its address, such as
    /// https://hangar.example.com), signing in with token.
    pub fn new(url: &str, token: &str) -> Client {
        let config = |read: Option<Duration>, global: Option<Duration>| {
            ureq::Agent::config_builder()
                .http_status_as_error(false)
                .timeout_global(global)
                .timeout_per_read(read)
                .tls_config(
                    ureq::tls::TlsConfig::builder()
                        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                        .build(),
                )
                .user_agent(concat!("hangar-app/", env!("CARGO_PKG_VERSION")))
                .build()
                .new_agent()
        };
        Client {
            base: format!("{}/api/{VERSION}", url.trim_end_matches('/')),
            token: token.to_string(),
            agent: config(None, Some(CALL)),
            stream: config(Some(STREAM_READ), None),
        }
    }

    pub fn version(&self) -> Result<Version, Error> {
        self.get("/version")
    }

    pub fn me(&self) -> Result<Me, Error> {
        self.get("/me")
    }

    pub fn environments(&self) -> Result<Vec<Environment>, Error> {
        self.get("/environments")
    }

    pub fn start(&self, id: &str) -> Result<Environment, Error> {
        self.post(&format!("/environments/{id}/start"), None)
    }

    pub fn stop(&self, id: &str) -> Result<Environment, Error> {
        self.post(&format!("/environments/{id}/stop"), None)
    }

    pub fn suspend(&self, id: &str) -> Result<Environment, Error> {
        self.post(&format!("/environments/{id}/suspend"), None)
    }

    /// Gives a stopped environment its template's settings, keeping its
    /// disks.
    pub fn reset(&self, id: &str) -> Result<Environment, Error> {
        self.post(&format!("/environments/{id}/reset"), None)
    }

    /// Deletes an environment and its disks: at once if it is stopped, else
    /// once it has stopped.
    pub fn delete(&self, id: &str) -> Result<(), Error> {
        let req = self
            .agent
            .delete(format!("{}/environments/{id}", self.base))
            .header("Authorization", self.auth());
        check(req.call().map_err(transport)?).map(|_| ())
    }

    pub fn set_task(&self, id: &str, text: &str) -> Result<Task, Error> {
        let body = serde_json::json!({ "text": text });
        let resp = self.send(
            self.agent
                .put(format!("{}/environments/{id}/task", self.base))
                .header("Authorization", self.auth())
                .header("Content-Type", "application/json"),
            Some(body.to_string()),
        )?;
        decode(resp)
    }

    pub fn end_task(&self, id: &str) -> Result<(), Error> {
        let req = self
            .agent
            .delete(format!("{}/environments/{id}/task", self.base))
            .header("Authorization", self.auth());
        check(req.call().map_err(transport)?).map(|_| ())
    }

    /// The environment's terminal sessions.
    pub fn terminals(&self, id: &str) -> Result<Vec<TerminalSession>, Error> {
        self.get(&format!("/environments/{id}/terminals"))
    }

    /// Part of one of the environment's logs, kept by its worker: what
    /// follows offset, or the end.
    pub fn environment_log(
        &self,
        id: &str,
        log: &str,
        offset: Option<i64>,
    ) -> Result<LogPart, Error> {
        match offset {
            Some(o) => self.get(&format!("/environments/{id}/logs/{log}?offset={o}")),
            None => self.get(&format!("/environments/{id}/logs/{log}")),
        }
    }

    /// What the environment uses, while it runs.
    pub fn stats(&self, id: &str) -> Result<Option<Stats>, Error> {
        let env: serde_json::Value = self.get(&format!("/environments/{id}"))?;
        Ok(serde_json::from_value(env["stats"].clone()).ok())
    }

    /// The processes running in the environment, asked of it.
    pub fn processes(&self, id: &str) -> Result<ProcessList, Error> {
        self.get(&format!("/environments/{id}/processes"))
    }

    /// Stops a process: TERM asks it to, KILL forces it.
    pub fn signal_process(&self, id: &str, pid: i64, signal: &str) -> Result<(), Error> {
        let req = self
            .agent
            .post(format!(
                "{}/environments/{id}/processes/{pid}/signal",
                self.base
            ))
            .header("Authorization", self.auth())
            .header("Content-Type", "application/json");
        check(self.send(
            req,
            Some(serde_json::json!({ "signal": signal }).to_string()),
        )?)
        .map(|_| ())
    }

    /// What has been done to the environment, newest first: a page of it,
    /// before an event's ID.
    pub fn activity(
        &self,
        id: &str,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Vec<ActivityEvent>, Error> {
        match before {
            Some(b) => self.get(&format!(
                "/environments/{id}/activity?before={b}&limit={limit}"
            )),
            None => self.get(&format!("/environments/{id}/activity?limit={limit}")),
        }
    }

    /// Says whether anyone may reach the environment's own web servers
    /// without signing in to Hangar, answered with the environment.
    pub fn set_ports_public(&self, id: &str, public: bool) -> Result<Environment, Error> {
        let req = self
            .agent
            .put(format!("{}/environments/{id}/ports", self.base))
            .header("Authorization", self.auth())
            .header("Content-Type", "application/json");
        decode(self.send(
            req,
            Some(serde_json::json!({ "public": public }).to_string()),
        )?)
    }

    /// Ends a terminal session, and what runs in it.
    pub fn close_terminal(&self, id: &str, session: &str) -> Result<(), Error> {
        let req = self
            .agent
            .delete(format!(
                "{}/environments/{id}/terminals/{session}",
                self.base
            ))
            .header("Authorization", self.auth());
        check(req.call().map_err(transport)?).map(|_| ())
    }

    /// Where a terminal session's WebSocket is, and the header that signs
    /// it in: session, or a new one at cols by rows.
    pub fn terminal_socket(
        &self,
        id: &str,
        session: Option<&str>,
        cols: u16,
        rows: u16,
    ) -> (String, String) {
        self.terminal_socket_in(id, session, cols, rows, None)
    }

    /// As terminal_socket, a new session starting in dir.
    pub fn terminal_socket_in(
        &self,
        id: &str,
        session: Option<&str>,
        cols: u16,
        rows: u16,
        dir: Option<&str>,
    ) -> (String, String) {
        let base = match self.base.split_once("://") {
            Some(("https", rest)) => format!("wss://{rest}"),
            Some((_, rest)) => format!("ws://{rest}"),
            None => self.base.clone(),
        };
        let mut url = format!("{base}/environments/{id}/terminal?cols={cols}&rows={rows}");
        if let Some(s) = session {
            url.push_str("&session=");
            url.push_str(s);
        }
        if let Some(d) = dir {
            url.push_str("&dir=");
            url.push_str(&encode(d));
        }
        (url, self.auth())
    }

    /// Where the environment's Code service's WebSocket is, starting in
    /// root, and the header that signs it in.
    pub fn code_socket(&self, id: &str, root: &str) -> (String, String) {
        let (terminal, auth) = self.terminal_socket(id, None, 0, 0);
        let base = terminal.split("/environments/").next().unwrap_or_default();
        let mut url = format!("{base}/environments/{id}/code");
        if !root.is_empty() {
            url.push_str("?root=");
            url.push_str(&encode(root));
        }
        (url, auth)
    }

    /// Says the user has looked at an agent's session.
    pub fn seen(&self, id: &str, session: &str) -> Result<(), Error> {
        self.post_empty(&format!("/environments/{id}/agents/{session}/seen"))
    }

    /// Opens the event stream: every environment, Synced, then changes.
    pub fn events(&self) -> Result<Events<impl BufRead + use<>>, Error> {
        let resp = self
            .stream
            .get(format!("{}/events", self.base))
            .header("Authorization", self.auth())
            .header("Accept", "text/event-stream")
            .call()
            .map_err(transport)?;
        let resp = check(resp)?;
        Ok(Events::new(BufReader::new(resp.into_body().into_reader())))
    }

    fn auth(&self) -> String {
        format!("Bearer {}", self.token)
    }

    fn get<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T, Error> {
        let resp = self
            .agent
            .get(format!("{}{path}", self.base))
            .header("Authorization", self.auth())
            .call()
            .map_err(transport)?;
        decode(resp)
    }

    fn post<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        body: Option<String>,
    ) -> Result<T, Error> {
        let req = self
            .agent
            .post(format!("{}{path}", self.base))
            .header("Authorization", self.auth())
            .header("Content-Type", "application/json");
        decode(self.send(req, body)?)
    }

    fn post_empty(&self, path: &str) -> Result<(), Error> {
        let req = self
            .agent
            .post(format!("{}{path}", self.base))
            .header("Authorization", self.auth());
        check(self.send(req, None)?).map(|_| ())
    }

    fn send(
        &self,
        req: ureq::RequestBuilder<ureq::typestate::WithBody>,
        body: Option<String>,
    ) -> Result<ureq::http::Response<ureq::Body>, Error> {
        match body {
            Some(b) => req.send(b),
            None => req.send_empty(),
        }
        .map_err(transport)
    }
}

fn transport(e: ureq::Error) -> Error {
    Error::Transport(e.to_string())
}

/// The response, or the error it carries.
fn check(
    mut resp: ureq::http::Response<ureq::Body>,
) -> Result<ureq::http::Response<ureq::Body>, Error> {
    let status = resp.status().as_u16();
    if status < 300 {
        return Ok(resp);
    }
    if status == 401 {
        return Err(Error::Unauthorized);
    }
    #[derive(Deserialize)]
    struct Body {
        error: String,
    }
    let msg = resp
        .body_mut()
        .read_json::<Body>()
        .map(|b| b.error)
        .unwrap_or_default();
    Err(Error::Status(status, msg))
}

fn decode<T: for<'de> Deserialize<'de>>(
    resp: ureq::http::Response<ureq::Body>,
) -> Result<T, Error> {
    let mut resp = check(resp)?;
    let mut text = String::new();
    resp.body_mut()
        .as_reader()
        .read_to_string(&mut text)
        .map_err(|e| Error::Transport(e.to_string()))?;
    serde_json::from_str(&text).map_err(|e| Error::Decode(e.to_string()))
}

/// The events of a server-sent event stream, as this API sends them.
/// Events of types the app does not know are passed over.
pub struct Events<R> {
    r: R,
    line: String,
}

impl<R: BufRead> Events<R> {
    pub fn new(r: R) -> Self {
        Events {
            r,
            line: String::new(),
        }
    }
}

impl<R: BufRead> Iterator for Events<R> {
    type Item = Result<Event, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut name = String::new();
        let mut data = String::new();
        loop {
            self.line.clear();
            match self.r.read_line(&mut self.line) {
                Ok(0) => return None,
                Ok(_) => {}
                Err(e) => return Some(Err(Error::Transport(e.to_string()))),
            }
            let line = self.line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if data.is_empty() && name.is_empty() {
                    continue;
                }
                let event = parse(&name, &data);
                name.clear();
                data.clear();
                match event {
                    Ok(Some(e)) => return Some(Ok(e)),
                    Ok(None) => continue,
                    Err(e) => return Some(Err(e)),
                }
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = line.split_once(':').unwrap_or((line, ""));
            let value = value.strip_prefix(' ').unwrap_or(value);
            match field {
                "event" => name = value.to_string(),
                "data" => {
                    if !data.is_empty() {
                        data.push('\n');
                    }
                    data.push_str(value);
                }
                _ => {}
            }
        }
    }
}

fn parse(name: &str, data: &str) -> Result<Option<Event>, Error> {
    let decode = |e: serde_json::Error| Error::Decode(format!("a {name} event: {e}"));
    Ok(Some(match name {
        "environment" => Event::Environment(Box::new(serde_json::from_str(data).map_err(decode)?)),
        "removed" => {
            #[derive(Deserialize)]
            struct Removed {
                id: String,
            }
            Event::Removed(serde_json::from_str::<Removed>(data).map_err(decode)?.id)
        }
        "synced" => Event::Synced,
        _ => return Ok(None),
    }))
}

/// A query value, percent-encoded.
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                (b as char).to_string()
            }
            b => format!("%{b:02X}"),
        })
        .collect()
}
