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
    #[serde(default)]
    pub task: Option<Task>,
    #[serde(default)]
    pub agents: Vec<AgentSession>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
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
        (url, self.auth())
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
