//! A server's client API, /api/v1 (api/client.yaml): what
//! the app reads and asks of a server place. Its types and calls are
//! generated from the spec (hangar-openapi); this is the app's blocking face
//! on them, each call run to its end on a runtime of this module's, so each
//! place's connection runs on a thread of its own as the rest of the app
//! expects.

use std::future::Future;
use std::io::{self, BufRead, BufReader, Read};
use std::sync::OnceLock;
use std::time::Duration;

use hangar_openapi::clientapi::{self as generated, types};
use serde::Deserialize;

pub use types::{
    Access, AccessToken, ActivityEvent, AgentSession, Environment, EnvironmentPack, ImageRollback,
    ImageUpdate, LogPart, LoginKey, Me, PackCopy, Process, ProcessList, Repo, Resources,
    SignInGrant, SshGateway, SshKey, StartProgress, Task, Template, TerminalSession, Version,
};

/// An environment's machine, as its settings change it.
pub type MachineSettings = types::EnvironmentSettings;
/// What making an environment asks for.
pub type NewEnvironment = types::CreateEnvironment;
/// What an environment uses, while it runs.
pub type Stats = types::EnvironmentStats;
/// Who did something to an environment.
pub type Actor = types::ActivityEventActor;
/// What something was done to.
pub type Target = types::ActivityEventTarget;

/// The API version the app speaks.
pub const VERSION: &str = "v1";

/// How long a read on the event stream may wait. The server sends a
/// comment every 30 seconds, so a stream silent for longer is dead.
const STREAM_READ: Duration = Duration::from_secs(75);

/// How long any other request may take.
const CALL: Duration = Duration::from_secs(30);

/// Where a sign-in through the browser stands.
#[derive(Debug, Clone, PartialEq)]
pub enum SignedIn {
    /// Approved: the access token, made for the app.
    Token(String),
    /// Not yet approved.
    Waiting,
    /// The person refused it.
    Refused,
    /// It expired or was collected.
    Gone,
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

/// The runtime every call runs on: reqwest's connections need tokio's,
/// whichever thread waits on the call.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("hangar-api")
            .enable_all()
            .build()
            .expect("a tokio runtime")
    })
}

/// A server's client API, signed in with an access token.
#[derive(Clone)]
pub struct Client {
    base: String,
    token: String,
    /// The generated client for calls, held to CALL, and for the event
    /// stream, whose reads are held to STREAM_READ; or why neither could be
    /// made.
    api: Result<(generated::Client, generated::Client), String>,
}

impl Client {
    /// A client for the server at url (its address, such as
    /// https://hangar.example.com), signing in with token.
    pub fn new(url: &str, token: &str) -> Client {
        let base = format!("{}/api/{VERSION}", url.trim_end_matches('/'));
        let make = |calls: bool| -> Result<generated::Client, String> {
            let mut headers = reqwest::header::HeaderMap::new();
            if !token.is_empty() {
                let auth = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
                    .map_err(|e| e.to_string())?;
                headers.insert(reqwest::header::AUTHORIZATION, auth);
            }
            let builder = hangar_openapi::http_client()
                .map_err(|e| e.to_string())?
                .default_headers(headers)
                .user_agent(concat!("hangar-app/", env!("CARGO_PKG_VERSION")));
            let builder = if calls {
                builder.timeout(CALL)
            } else {
                builder.read_timeout(STREAM_READ)
            };
            let http = builder.build().map_err(|e| e.to_string())?;
            Ok(generated::Client::new_with_client(&base, http))
        };
        let api = make(true).and_then(|calls| Ok((calls, make(false)?)));
        Client {
            base,
            token: token.to_string(),
            api,
        }
    }

    pub fn version(&self) -> Result<Version, Error> {
        self.call(|c| c.get_version())
    }

    pub fn me(&self) -> Result<Me, Error> {
        self.call(|c| c.get_me())
    }

    /// Asks the server to let an app that calls itself name sign in
    /// through the browser. Needs no token.
    pub fn start_sign_in(&self, name: &str) -> Result<SignInGrant, Error> {
        let body = types::StartSignIn { name: name.into() };
        self.call(|c| c.start_sign_in(&body))
    }

    /// Asks whether a grant is approved, collecting its token once it is.
    /// Needs no token.
    pub fn finish_sign_in(&self, device_code: &str) -> Result<SignedIn, Error> {
        let body = types::FinishSignIn {
            device_code: device_code.into(),
        };
        match self.answer(|c| c.finish_sign_in_step(&body)) {
            Ok(hangar_openapi::SignInStep::Token(t)) => Ok(SignedIn::Token(t.token)),
            Ok(hangar_openapi::SignInStep::Waiting(_)) => Ok(SignedIn::Waiting),
            Err(Error::Status(403, _)) => Ok(SignedIn::Refused),
            Err(Error::Status(404, _)) => Ok(SignedIn::Gone),
            Err(e) => Err(e),
        }
    }

    pub fn login_keys(&self) -> Result<Vec<LoginKey>, Error> {
        self.call(|c| c.list_login_keys())
    }

    /// Adds a public key, in authorized_keys form, that signs the user in
    /// over SSH; its comment names it.
    pub fn add_login_key(&self, public_key: &str) -> Result<LoginKey, Error> {
        let body = types::AddLoginKey {
            public_key: public_key.trim().into(),
            ..Default::default()
        };
        self.call(|c| c.add_login_key(&body))
    }

    pub fn delete_login_key(&self, id: &str) -> Result<(), Error> {
        self.call(|c| c.delete_login_key(id))
    }

    pub fn tokens(&self) -> Result<Vec<AccessToken>, Error> {
        self.call(|c| c.list_tokens())
    }

    pub fn revoke_token(&self, id: &str) -> Result<(), Error> {
        self.call(|c| c.revoke_token(id))
    }

    pub fn ssh_keys(&self) -> Result<Vec<SshKey>, Error> {
        self.call(|c| c.list_ssh_keys())
    }

    pub fn generate_ssh_key(&self, name: &str) -> Result<SshKey, Error> {
        let body = types::GenerateSshKey {
            name: name.trim().into(),
        };
        self.call(|c| c.generate_ssh_key(&body))
    }

    pub fn delete_ssh_key(&self, id: &str) -> Result<(), Error> {
        self.call(|c| c.delete_ssh_key(id))
    }

    pub fn environments(&self) -> Result<Vec<Environment>, Error> {
        self.call(|c| c.list_environments())
    }

    pub fn start(&self, id: &str) -> Result<Environment, Error> {
        self.call(|c| c.start_environment(id))
    }

    pub fn stop(&self, id: &str) -> Result<Environment, Error> {
        self.call(|c| c.stop_environment(id))
    }

    pub fn suspend(&self, id: &str) -> Result<Environment, Error> {
        self.call(|c| c.suspend_environment(id))
    }

    /// Gives a stopped environment its template's settings, keeping its
    /// disks.
    pub fn reset(&self, id: &str) -> Result<Environment, Error> {
        self.call(|c| c.reset_environment(id))
    }

    /// Deletes an environment and its disks: at once if it is stopped, else
    /// once it has stopped.
    pub fn delete(&self, id: &str) -> Result<(), Error> {
        self.answer(|c| c.delete_environment_or_gone(id))
            .map(|_| ())
    }

    pub fn set_task(&self, id: &str, text: &str) -> Result<Task, Error> {
        let body = types::SetTask { text: text.into() };
        self.call(|c| c.set_task(id, &body))
    }

    pub fn end_task(&self, id: &str) -> Result<(), Error> {
        self.call(|c| c.end_task(id))
    }

    /// The environment's terminal sessions.
    pub fn terminals(&self, id: &str) -> Result<Vec<TerminalSession>, Error> {
        self.call(|c| c.list_terminals(id))
    }

    /// Part of one of the environment's logs, kept by its worker: what
    /// follows offset, or the end.
    pub fn environment_log(
        &self,
        id: &str,
        log: &str,
        offset: Option<i64>,
    ) -> Result<LogPart, Error> {
        let log = types::EnvironmentLogName(log.into());
        self.call(|c| c.get_environment_log(id, &log, offset))
    }

    /// What the environment uses, while it runs.
    pub fn stats(&self, id: &str) -> Result<Option<Stats>, Error> {
        self.environment(id).map(|e| e.stats)
    }

    /// The processes running in the environment, asked of it.
    pub fn processes(&self, id: &str) -> Result<ProcessList, Error> {
        self.call(|c| c.list_processes(id))
    }

    /// Stops a process: TERM asks it to, KILL forces it.
    pub fn signal_process(&self, id: &str, pid: i64, signal: &str) -> Result<(), Error> {
        let body = types::SignalProcessBody {
            signal: signal.into(),
        };
        self.call(|c| c.signal_process(id, pid, &body))
    }

    /// What has been done to the environment, newest first: a page of it,
    /// before an event's ID.
    pub fn activity(
        &self,
        id: &str,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Vec<ActivityEvent>, Error> {
        self.call(|c| c.list_activity(id, before, Some(limit as i64)))
    }

    /// The templates the user may make an environment from.
    pub fn templates(&self) -> Result<Vec<Template>, Error> {
        self.call(|c| c.list_templates())
    }

    /// Makes an environment; it starts once a worker takes it.
    pub fn create_environment(&self, new: &NewEnvironment) -> Result<Environment, Error> {
        self.call(|c| c.create_environment(new))
    }

    /// One environment, as it is.
    pub fn environment(&self, id: &str) -> Result<Environment, Error> {
        self.call(|c| c.get_environment(id))
    }

    /// Changes an environment's machine: a stopped one's at its next start,
    /// a running one's memory as it runs. Answered with the environment.
    pub fn update_settings(&self, id: &str, s: &MachineSettings) -> Result<Environment, Error> {
        self.call(|c| c.update_environment_settings(id, s))
    }

    /// Each online worker's capacity, largest first.
    pub fn worker_sizes(&self) -> Result<Vec<Resources>, Error> {
        self.call(|c| c.list_worker_sizes())
    }

    /// The packs an environment has, each with the copy it uses.
    pub fn environment_packs(&self, id: &str) -> Result<Vec<EnvironmentPack>, Error> {
        self.call(|c| c.list_environment_packs(id))
    }

    /// Chooses the copy of a pack an environment uses; None undoes the
    /// choice.
    pub fn choose_copy(&self, id: &str, pack: &str, copy: Option<&str>) -> Result<(), Error> {
        let body = types::ChooseEnvironmentCopyBody {
            copy_id: copy.map(str::to_string),
        };
        self.call(|c| c.choose_environment_copy(id, pack, &body))
    }

    /// Changes an environment's image: upgrade, rollback, keep or cancel.
    /// force upgrades though the newer copy has not been compared yet.
    pub fn change_image(&self, id: &str, change: &str, force: bool) -> Result<Environment, Error> {
        let body = types::ChangeEnvironmentImageBody {
            force: force.then_some(true),
        };
        self.call(|c| c.change_environment_image(id, change, &body))
    }

    /// Says whether anyone may reach the environment's own web servers
    /// without signing in to Hangar, answered with the environment.
    pub fn set_ports_public(&self, id: &str, public: bool) -> Result<Environment, Error> {
        let body = types::SetEnvironmentPortsBody { public };
        self.call(|c| c.set_environment_ports(id, &body))
    }

    /// Ends a terminal session, and what runs in it.
    pub fn close_terminal(&self, id: &str, session: &str) -> Result<(), Error> {
        self.call(|c| c.close_terminal(id, session))
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

    /// Where the environment's desktop's WebSocket is (RFB to its VNC
    /// server), and the header that signs it in.
    pub fn desktop_socket(&self, id: &str) -> (String, String) {
        let base = match self.base.split_once("://") {
            Some(("https", rest)) => format!("wss://{rest}"),
            Some((_, rest)) => format!("ws://{rest}"),
            None => self.base.clone(),
        };
        (format!("{base}/environments/{id}/desktop"), self.auth())
    }

    /// Sets the size of the environment's desktop, in pixels.
    pub fn resize_desktop(&self, id: &str, width: u32, height: u32) -> Result<(), Error> {
        let body = types::ResizeDesktopBody {
            width: width.into(),
            height: height.into(),
        };
        self.call(|c| c.resize_desktop(id, &body))
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
        self.call(|c| c.see_agent(id, session))
    }

    /// Opens the event stream: every environment, Synced, then changes.
    pub fn events(&self) -> Result<Events<impl BufRead + use<>>, Error> {
        let (_, stream) = self.api.as_ref().map_err(|e| Error::Transport(e.clone()))?;
        let bytes = runtime().block_on(async {
            match stream.stream_events().await {
                Ok(v) => Ok(v.into_inner()),
                Err(e) => Err(error(e).await),
            }
        })?;
        Ok(Events::new(BufReader::new(StreamReader {
            stream: bytes.into_inner(),
            chunk: bytes::Bytes::new(),
        })))
    }

    fn auth(&self) -> String {
        format!("Bearer {}", self.token)
    }

    /// Runs a generated call to its end, its answer's body or why it
    /// failed.
    fn call<'a, T, E: Message, F>(
        &'a self,
        f: impl FnOnce(&'a generated::Client) -> F,
    ) -> Result<T, Error>
    where
        F: Future<Output = Result<generated::ResponseValue<T>, generated::Error<E>>>,
    {
        self.answer(|c| {
            let call = f(c);
            async move { call.await.map(generated::ResponseValue::into_inner) }
        })
    }

    /// Runs a call that gives its answer as it is (hangar-openapi's calls
    /// for the operations that answer two ways) to its end.
    fn answer<'a, T, E: Message, F>(
        &'a self,
        f: impl FnOnce(&'a generated::Client) -> F,
    ) -> Result<T, Error>
    where
        F: Future<Output = Result<T, generated::Error<E>>>,
    {
        let (calls, _) = self.api.as_ref().map_err(|e| Error::Transport(e.clone()))?;
        let call = f(calls);
        runtime().block_on(async move {
            match call.await {
                Ok(v) => Ok(v),
                Err(e) => Err(error(e).await),
            }
        })
    }
}

/// What an error answer says: the spec's Error body, or nothing for a call
/// the spec gives no error body.
trait Message: std::fmt::Debug {
    fn message(self) -> String;
}

impl Message for types::Error {
    fn message(self) -> String {
        self.error
    }
}

impl Message for () {
    fn message(self) -> String {
        String::new()
    }
}

/// A failed call, as the app tells it.
async fn error<E: Message>(e: generated::Error<E>) -> Error {
    let status = |code: u16, msg: String| {
        if code == 401 {
            Error::Unauthorized
        } else {
            Error::Status(code, msg)
        }
    };
    match e {
        generated::Error::ErrorResponse(r) => {
            let code = r.status().as_u16();
            status(code, r.into_inner().message())
        }
        generated::Error::UnexpectedResponse(r) => {
            #[derive(Deserialize)]
            struct Body {
                error: String,
            }
            let code = r.status().as_u16();
            let msg = r.json::<Body>().await.map(|b| b.error).unwrap_or_default();
            status(code, msg)
        }
        generated::Error::InvalidResponsePayload(_, e) => Error::Decode(e.to_string()),
        e => Error::Transport(e.to_string()),
    }
}

/// The event stream's bytes, read as they arrive by whichever thread reads
/// the stream, each read held to STREAM_READ.
struct StreamReader {
    stream: std::pin::Pin<
        Box<dyn futures_core::Stream<Item = reqwest::Result<bytes::Bytes>> + Send + Sync>,
    >,
    chunk: bytes::Bytes,
}

impl Read for StreamReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.chunk.is_empty() {
            let next = runtime().block_on(async {
                tokio::time::timeout(
                    STREAM_READ,
                    std::future::poll_fn(|cx| self.stream.as_mut().poll_next(cx)),
                )
                .await
            });
            match next {
                Err(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "the event stream went quiet",
                    ));
                }
                Ok(None) => return Ok(0),
                Ok(Some(Err(e))) => return Err(io::Error::other(e)),
                Ok(Some(Ok(bytes))) => self.chunk = bytes,
            }
        }
        let n = buf.len().min(self.chunk.len());
        buf[..n].copy_from_slice(&self.chunk[..n]);
        self.chunk = self.chunk.slice(n..);
        Ok(n)
    }
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
