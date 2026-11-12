//! The profile session: what of a user follows them into their
//! environments, carried over a session the server holds with the agent,
//! in the protocol `internal/profile` defines.
//!
//! # Shared files
//!
//! The server sends the copies of packs the environment uses and their
//! paths, which the agent routes there (hangarfs, to the worker's NFS
//! server). Before it changes the routes it sorts each path whose sharing
//! changes (`moves.rs`): a file only the environment has is copied up into
//! the copy; one only the copy has simply appears; a file the environment
//! had of its own, differing from the copy's, is kept aside and reported as
//! a conflict, which the server resolves; and a path that stops being
//! shared is copied down, so the environment keeps it as its own.
//!
//! # SSH keys and Hangar's registry
//!
//! The agent serves an SSH agent whose signatures the server makes
//! (`sshagent.rs`), and a Docker credential helper whose credentials the
//! server gives (`registry.rs`).
//!
//! # Coding agents
//!
//! What the coding agents in the environment are doing goes to the server,
//! and the environment's task both ways (`crate::agents`).
//!
//! # Protocol
//!
//! A session is JSON lines both ways on one stream, which the server opens
//! to vsock port 8105. Each line is a `Message`.

mod files;
mod moves;
mod registry;
mod routes;
mod sshagent;

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Notify, mpsc, oneshot, watch};

use crate::agents::{self, Task};
use crate::user::User;

pub use files::{Files, Router, as_user};
pub use registry::{HelperCredential, credential_helper};
pub use routes::{Route, SetPaths, copies_in, copy_of, routes_for, served};
pub use sshagent::parse_authorized_key;

pub const PORT: u32 = 8105;

/// Where the credential helper reaches the agent.
pub const REGISTRY_SOCKET: &str = registry::SOCKET;

/// Written once the owner's profile has first arrived whole, which the
/// worker waits for before setting up the workspace.
pub const SYNCED: &str = "/run/hangar/profile-synced";

// Server to agent.
/// The copies the environment uses and their paths; again when they change.
pub const TYPE_PATHS: &str = "paths";
/// The public keys the SSH agent offers.
pub const TYPE_KEYS: &str = "keys";
/// The answer to a sign.
pub const TYPE_SIGNED: &str = "signed";
/// How a conflict is resolved: copy, path and resolution.
pub const TYPE_RESOLVE: &str = "resolve";
/// Hangar's registry's host, when the environment may use it.
pub const TYPE_REGISTRY: &str = "registry";
/// The answer to a get_credential.
pub const TYPE_CREDENTIAL: &str = "credential";
/// The environment's task, sent first and again when it changes; none when
/// it has none.
pub const TYPE_TASK: &str = "task";
/// The answer to a set_task: the task as it is after, or an error.
pub const TYPE_TASK_SET: &str = "task_set";

// Agent to server.
/// Sign with one of the keys.
pub const TYPE_SIGN: &str = "sign";
/// The copies the agent routes to, once it has applied a paths.
pub const TYPE_ROUTED: &str = "routed";
/// A conflict found (copy and path), and one resolved.
pub const TYPE_CONFLICT: &str = "conflict";
pub const TYPE_RESOLVED: &str = "resolved";
/// A credential for Hangar's registry, as the environment's owner.
pub const TYPE_GET_CREDENTIAL: &str = "get_credential";
/// The coding agents' sessions, all of them, sent when they change and at
/// the start of each session.
pub const TYPE_AGENTS: &str = "agents";
/// Set the environment's task to text, as its coding agent; an empty text
/// marks it done.
pub const TYPE_SET_TASK: &str = "set_task";

/// The environment's file replaces the copy's, resolving a conflict; the
/// other resolution drops it.
pub const RESOLVE_ENVIRONMENT: &str = "environment";

/// One line of a session.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Message {
    #[serde(rename = "type", default)]
    pub kind: String,

    /// The copies the environment uses, in order, with the paths each
    /// shares; `routed` the IDs of those the agent routes to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sets: Vec<SetPaths>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routed: Vec<String>,

    /// A conflict: the copy, the path (as a pack names it) and how it is
    /// resolved.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub copy: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub resolution: String,

    /// Authorized-keys lines.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<String>,

    /// A request and its answer share an ID. For a signature, `key` is the
    /// public key in SSH wire form, `data` what to sign, and `signature`
    /// the signature in wire form.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub id: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty", with = "b64")]
    pub data: Vec<u8>,
    #[serde(default, skip_serializing_if = "Vec::is_empty", with = "b64")]
    pub key: Vec<u8>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub flags: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty", with = "b64")]
    pub signature: Vec<u8>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,

    /// Hangar's registry: its host, and a credential for it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub host: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub username: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub secret: String,

    /// The environment's task, and the text of one to set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<Task>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// The coding agents' sessions.
    #[serde(default, skip_serializing_if = "Vec::is_empty", skip_deserializing)]
    pub agents: Vec<agents::Session>,
}

fn is_zero<T: Default + PartialEq>(n: &T) -> bool {
    *n == T::default()
}

/// Bytes as Go's JSON has them: standard base64.
mod b64 {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(b: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(b))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        match Option::<String>::deserialize(d)? {
            None => Ok(vec![]),
            Some(s) => STANDARD.decode(s).map_err(D::Error::custom),
        }
    }
}

pub(crate) fn warn(msg: &str) {
    eprintln!("hangar-agent: profile: {msg}");
}

/// A line only `HANGAR_AGENT_DEBUG` asks for.
pub(crate) fn debug(msg: &str) {
    if std::env::var_os("HANGAR_AGENT_DEBUG").is_some() {
        eprintln!("hangar-agent: profile: {msg}");
    }
}

/// How often routes that could not be applied are tried again: the worker
/// may not be serving the shared files yet.
const ROUTE_RETRY: Duration = Duration::from_secs(2);

/// How long an answer from the server is waited for.
const ANSWER_WAIT: Duration = Duration::from_secs(30);

/// How long setting the task waits for a session to ask through.
const TASK_WAIT: Duration = Duration::from_secs(10);

/// The agent's half of the profile, for one user. A new session replaces
/// the one before it, which ended with its connection, whether or not this
/// side has noticed yet.
pub struct Guest<F> {
    user: User,
    files: F,
    /// The homes whose Docker configuration names the credential helper,
    /// each with its owner.
    docker_homes: Vec<(PathBuf, u32, u32)>,
    state: Mutex<State>,
    /// Bumped each time the session changes.
    changed: watch::Sender<u64>,
    /// Set once a session's routes have first been applied.
    synced: watch::Sender<bool>,
}

#[derive(Default)]
struct State {
    current: Option<Arc<Session>>,
    /// The keys the SSH agent offers, in wire form.
    keys: Vec<Vec<u8>>,
    /// As last applied.
    routes: Option<Vec<Route>>,
    /// The coding agents' sessions as last reported, sent at the start of
    /// each session; none until there have been any reported.
    agents: Option<Vec<agents::Session>>,
    /// The environment's task, as the server last said.
    task: Option<Task>,
}

/// One session with the server.
pub(crate) struct Session {
    out: mpsc::UnboundedSender<Vec<u8>>,
    /// Whether the keys have been sent.
    have_keys: AtomicBool,
    pending: Mutex<HashMap<i64, oneshot::Sender<Message>>>,
    next_id: AtomicI64,
    /// Ends the session: another has replaced it.
    replaced: Notify,
}

impl Session {
    fn keys_known(&self) -> bool {
        self.have_keys.load(Ordering::Acquire)
    }

    /// Sends a message, unless the session has ended.
    fn send(&self, m: &Message) -> bool {
        let mut b = serde_json::to_vec(m).unwrap_or_default();
        b.push(b'\n');
        self.out.send(b).is_ok()
    }

    /// Sends a request and waits for its answer.
    async fn ask(&self, mut m: Message) -> Result<Message, String> {
        let (tx, rx) = oneshot::channel();
        m.id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        self.pending.lock().unwrap().insert(m.id, tx);
        let forget = || self.pending.lock().unwrap().remove(&m.id);
        if !self.send(&m) {
            forget();
            return Err("the profile session ended".into());
        }
        match tokio::time::timeout(ANSWER_WAIT, rx).await {
            Ok(Ok(r)) if !r.error.is_empty() => Err(r.error),
            Ok(Ok(r)) => Ok(r),
            Ok(Err(_)) => Err("the profile session ended".into()),
            Err(_) => {
                forget();
                Err("the server did not answer".into())
            }
        }
    }

    /// Hands an answer to the request waiting for it.
    fn answer(&self, m: Message) {
        if let Some(tx) = self.pending.lock().unwrap().remove(&m.id) {
            let _ = tx.send(m);
        }
    }

    fn fail_pending(&self) {
        for (id, tx) in self.pending.lock().unwrap().drain() {
            let _ = tx.send(Message {
                id,
                error: "the profile session ended".into(),
                ..Message::default()
            });
        }
    }
}

impl<F: Files> Guest<F> {
    /// Serves `user`'s shared files through `files`, and their SSH agent.
    pub fn new(user: User, files: F) -> Self {
        let docker_homes = vec![
            (user.home.clone(), user.uid, user.gid),
            (PathBuf::from("/root"), 0, 0),
        ];
        Self {
            user,
            files,
            docker_homes,
            state: Mutex::new(State::default()),
            changed: watch::channel(0).0,
            synced: watch::channel(false).0,
        }
    }

    /// Has Docker configured for Hangar's registry in these homes alone,
    /// each with its owner.
    pub fn with_docker_homes(mut self, homes: Vec<(PathBuf, u32, u32)>) -> Self {
        self.docker_homes = homes;
        self
    }

    pub fn user(&self) -> &User {
        &self.user
    }

    pub fn files(&self) -> &F {
        &self.files
    }

    /// Returns once a session's routes have first been applied.
    pub async fn synced(&self) {
        let mut rx = self.synced.subscribe();
        let _ = rx.wait_for(|s| *s).await;
    }

    /// Holds one session with the server, on `conn`, until it ends.
    pub async fn serve<S>(self: &Arc<Self>, conn: S)
    where
        S: AsyncRead + AsyncWrite + Send + 'static,
    {
        let (rd, mut wr) = tokio::io::split(conn);
        let (out, mut lines) = mpsc::unbounded_channel::<Vec<u8>>();
        let writer = crate::serve::AbortOnDrop(tokio::spawn(async move {
            while let Some(b) = lines.recv().await {
                if wr.write_all(&b).await.is_err() {
                    return;
                }
            }
        }));
        let s = Arc::new(Session {
            out,
            have_keys: AtomicBool::new(false),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicI64::new(0),
            replaced: Notify::new(),
        });
        self.set_current(Some(s.clone()), None);
        let agents = self.state.lock().unwrap().agents.clone();
        if let Some(agents) = agents {
            s.send(&Message {
                kind: TYPE_AGENTS.into(),
                agents,
                ..Message::default()
            });
        }
        match self.run(&s, rd).await {
            Err(e) if e.kind() != io::ErrorKind::UnexpectedEof => {
                warn(&format!("the session ended: {e}"))
            }
            _ => {}
        }
        self.set_current(None, Some(&s));
        s.fail_pending();
        drop(writer);
    }

    /// Makes `s` the session. Given `was`, it only replaces that one.
    fn set_current(&self, s: Option<Arc<Session>>, was: Option<&Arc<Session>>) {
        let mut st = self.state.lock().unwrap();
        if let Some(was) = was
            && !st.current.as_ref().is_some_and(|c| Arc::ptr_eq(c, was))
        {
            return;
        }
        if s.is_some()
            && let Some(old) = &st.current
        {
            old.replaced.notify_one();
        }
        st.current = s;
        self.changed.send_modify(|n| *n += 1);
    }

    /// The session as soon as there is one, waiting up to `wait`.
    async fn connected(&self, wait: Duration) -> Option<Arc<Session>> {
        self.await_session(wait, |_| true).await
    }

    /// The session once there is one and `ok` says it will do, waiting up
    /// to `wait`.
    async fn await_session(
        &self,
        wait: Duration,
        ok: impl Fn(&Session) -> bool,
    ) -> Option<Arc<Session>> {
        let deadline = tokio::time::Instant::now() + wait;
        let mut changed = self.changed.subscribe();
        loop {
            let current = self.state.lock().unwrap().current.clone();
            if let Some(s) = current
                && ok(&s)
            {
                return Some(s);
            }
            tokio::select! {
                _ = changed.changed() => {}
                _ = tokio::time::sleep_until(deadline) => return None,
                _ = tokio::time::sleep(Duration::from_millis(50)) => {}
            }
        }
    }

    async fn run<R>(self: &Arc<Self>, s: &Arc<Session>, rd: R) -> io::Result<()>
    where
        R: AsyncRead + Unpin + Send + 'static,
    {
        let (incoming, mut heard) = mpsc::channel::<Message>(64);
        let reader = {
            let s = s.clone();
            tokio::spawn(async move {
                let mut r = BufReader::with_capacity(64 << 10, rd);
                loop {
                    let mut line = vec![];
                    if r.read_until(b'\n', &mut line).await? == 0 {
                        return Err(io::ErrorKind::UnexpectedEof.into());
                    }
                    let m: Message = serde_json::from_slice(&line)?;
                    // Answers go straight to what asked.
                    if [TYPE_SIGNED, TYPE_CREDENTIAL, TYPE_TASK_SET].contains(&m.kind.as_str()) {
                        s.answer(m);
                        continue;
                    }
                    if incoming.send(m).await.is_err() {
                        return Ok(());
                    }
                }
            })
        };
        let mut reader = crate::serve::AbortOnDrop(reader);
        // Routes not yet applied, tried again until they are.
        let mut unrouted: Option<Vec<Route>> = None;
        let mut retry = tokio::time::interval(ROUTE_RETRY);
        retry.tick().await;
        loop {
            tokio::select! {
                biased;
                Some(m) = heard.recv() => self.handle(s, m, &mut unrouted).await,
                r = &mut reader.0 => return r.unwrap_or(Ok(())),
                _ = s.replaced.notified() => return Ok(()),
                _ = retry.tick() => {
                    if let Some(routes) = unrouted.clone() {
                        self.apply_routes(s, routes, &mut unrouted).await;
                    }
                }
            }
        }
    }

    async fn handle(
        self: &Arc<Self>,
        s: &Arc<Session>,
        m: Message,
        unrouted: &mut Option<Vec<Route>>,
    ) {
        match m.kind.as_str() {
            TYPE_PATHS => {
                let home = self.user.home.to_string_lossy().into_owned();
                self.apply_routes(s, routes_for(&home, &m.sets), unrouted)
                    .await;
            }
            TYPE_RESOLVE => {
                let (g, s) = (self.clone(), s.clone());
                let _ = tokio::task::spawn_blocking(move || match g.resolve(&m) {
                    Ok(()) => {
                        s.send(&Message {
                            kind: TYPE_RESOLVED.into(),
                            copy: m.copy,
                            path: m.path,
                            ..Message::default()
                        });
                    }
                    Err(e) => warn(&format!("resolving a conflict at {}: {e}", m.path)),
                })
                .await;
            }
            TYPE_REGISTRY => {
                let g = self.clone();
                let _ = tokio::task::spawn_blocking(move || g.set_registry(&m.host)).await;
            }
            TYPE_TASK => self.state.lock().unwrap().task = m.task,
            TYPE_KEYS => {
                let keys = m
                    .keys
                    .iter()
                    .filter_map(|k| parse_authorized_key(k))
                    .collect();
                self.state.lock().unwrap().keys = keys;
                s.have_keys.store(true, Ordering::Release);
            }
            _ => {}
        }
    }

    /// Has `routes` applied, on a thread of its own, keeping them to try
    /// again in `unrouted` when they cannot be.
    async fn apply_routes(
        self: &Arc<Self>,
        s: &Arc<Session>,
        routes: Vec<Route>,
        unrouted: &mut Option<Vec<Route>>,
    ) {
        let (g, s) = (self.clone(), s.clone());
        let applied = {
            let routes = routes.clone();
            tokio::task::spawn_blocking(move || g.apply(&s, routes))
                .await
                .unwrap_or_else(|e| Err(io::Error::other(e)))
        };
        match applied {
            Ok(()) => *unrouted = None,
            Err(e) => {
                if unrouted.is_none() {
                    warn(&format!("routing the shared files, trying again: {e}"));
                }
                *unrouted = Some(routes);
            }
        }
    }

    /// Applies the routes, when they are not those already -- the files
    /// whose sharing changes sorted first, and those the new routes leave
    /// to the environment copied down after -- and tells the server what it
    /// routes to and the conflicts it found. Then it makes the directories
    /// the shared paths are in, if they are missing, and writes Claude's
    /// record of this machine if it has none.
    fn apply(&self, s: &Session, routes: Vec<Route>) -> io::Result<()> {
        let same = self.state.lock().unwrap().routes.as_ref() == Some(&routes);
        if !same {
            self.files.mount()?;
            let old = self.last_routes();
            let (downs, conflicts) = self.sort_paths(&old, &routes);
            self.files.route(&routes)?;
            self.copy_down(downs);
            self.state.lock().unwrap().routes = Some(routes.clone());
            self.keep_routes(&routes);
            for c in conflicts {
                s.send(&Message {
                    kind: TYPE_CONFLICT.into(),
                    copy: c.copy,
                    path: c.key,
                    ..Message::default()
                });
            }
            s.send(&Message {
                kind: TYPE_ROUTED.into(),
                routed: copies_in(&routes),
                ..Message::default()
            });
        }
        // Each time, routes changed or not, so a directory removed since is
        // back once the session says the routes again, as it does on
        // reconnecting.
        self.make_parents(&routes);
        if let Err(e) = self.seed_claude_state() {
            warn(&format!("writing Claude's state: {e}"));
        }
        self.synced.send_replace(true);
        Ok(())
    }
}

impl<F: Files> agents::Uplink for Guest<F> {
    /// At once if there is a session, and at the start of each one after.
    fn report_agents(&self, sessions: Vec<agents::Session>) {
        let mut st = self.state.lock().unwrap();
        st.agents = Some(sessions.clone());
        if let Some(s) = &st.current {
            s.send(&Message {
                kind: TYPE_AGENTS.into(),
                agents: sessions,
                ..Message::default()
            });
        }
    }

    async fn set_task(&self, text: String) -> Result<Option<Task>, String> {
        let Some(s) = self.connected(TASK_WAIT).await else {
            return Err("not connected to Hangar".into());
        };
        let r = s
            .ask(Message {
                kind: TYPE_SET_TASK.into(),
                text,
                ..Message::default()
            })
            .await?;
        self.state.lock().unwrap().task = r.task.clone();
        Ok(r.task)
    }

    fn task(&self) -> Option<Task> {
        self.state.lock().unwrap().task.clone()
    }
}

/// Points every login at the SSH agent, and has ssh accept a host it has
/// not met: an environment is new, so every host is one it has not met,
/// and a prompt in the middle of a clone is no protection.
fn ssh_setup() {
    let files = [
        (
            "/etc/profile.d/hangar-ssh.sh",
            format!("export SSH_AUTH_SOCK={}\n", crate::user::SSH_AUTH_SOCK),
        ),
        (
            "/etc/ssh/ssh_config.d/50-hangar.conf",
            "Host *\n    StrictHostKeyChecking accept-new\n".into(),
        ),
    ];
    for (path, content) in files {
        let path = Path::new(path);
        if let Some(dir) = path.parent()
            && std::fs::create_dir_all(dir).is_ok()
        {
            let _ = std::fs::write(path, content);
        }
    }
}

/// Docker runs a credential helper by name from PATH: this binary, under
/// that name, beside it.
fn install_credential_helper(exe: &Path) -> io::Result<()> {
    let (Some(dir), Some(name)) = (exe.parent(), exe.file_name()) else {
        return Ok(());
    };
    let helper = dir.join(format!("docker-credential-{}", registry::HELPER));
    if std::fs::read_link(&helper).is_ok_and(|t| t == Path::new(name)) {
        return Ok(());
    }
    let tmp = dir.join(format!(".docker-credential-{}.new", registry::HELPER));
    let _ = std::fs::remove_file(&tmp);
    std::os::unix::fs::symlink(name, &tmp)?;
    std::fs::rename(&tmp, &helper)
}

/// The profile session and what it carries, for as long as the guest runs:
/// the shared files routed to the worker's, the user's SSH agent, Docker's
/// credentials for Hangar's registry, and the coding agents' status.
pub async fn serve() {
    ssh_setup();
    let user = loop {
        match User::lookup(crate::user::DEFAULT_USER) {
            Some(u) => break u,
            // The image has no such user, or not yet: users can be made by
            // units that run after the agent starts.
            None => tokio::time::sleep(Duration::from_secs(10)).await,
        }
    };
    let guest = Arc::new(Guest::new(user.clone(), Router::new(user)));

    tokio::spawn({
        let guest = guest.clone();
        async move {
            guest.synced().await;
            let synced = Path::new(SYNCED);
            if let Some(dir) = synced.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Err(e) = std::fs::write(synced, b"") {
                warn(&format!("marking it synced: {e}"));
            }
        }
    });
    tokio::spawn({
        let guest = guest.clone();
        async move {
            loop {
                if let Err(e) = guest
                    .serve_ssh_agent(Path::new(crate::user::SSH_AUTH_SOCK))
                    .await
                {
                    warn(&format!("the ssh agent: {e}"));
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    });

    let exe = std::env::current_exe().unwrap_or_else(|_| "/usr/local/bin/hangar-agent".into());
    if let Err(e) = install_credential_helper(&exe) {
        warn(&format!("installing the docker credential helper: {e}"));
    }
    // The coding agents' hooks and their MCP server, which reach the server
    // through the session.
    if let Err(e) = agents::write_plugin(Path::new(agents::PLUGIN_DIR), &exe.to_string_lossy()) {
        warn(&format!("writing the Claude Code plugin: {e}"));
    }
    tokio::spawn({
        let guest = guest.clone();
        async move {
            loop {
                let socket = Path::new(agents::SOCKET);
                if let Err(e) =
                    agents::serve(socket, guest.clone(), agents::Processes::proc()).await
                {
                    warn(&format!("the coding agents' socket: {e}"));
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    });
    tokio::spawn({
        let guest = guest.clone();
        async move {
            loop {
                if let Err(e) = guest.serve_credentials(Path::new(REGISTRY_SOCKET)).await {
                    warn(&format!("registry credentials: {e}"));
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    });

    crate::serve::listen("profile", PORT, move |conn| {
        let guest = guest.clone();
        async move { guest.serve(conn).await }
    })
    .await
}
