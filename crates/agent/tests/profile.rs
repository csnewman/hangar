//! The profile session, driven as the server drives it: the shared files'
//! routes and the files moving as sharing changes, the SSH agent, Hangar's
//! registry, and the coding agents' status and task.
//!
//! The shared files are a directory standing in for the worker's NFS
//! server, and routes are recorded rather than applied, so a path the
//! routes share is still the guest's own on disk.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hangar_agent::agents::{self, Uplink};
use hangar_agent::profile::{Files, Guest, Route, SetPaths, routes_for};
use hangar_agent::user::User;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream};
use tokio::io::{ReadHalf, WriteHalf};
use tokio::net::UnixStream;

const WAIT: Duration = Duration::from_secs(10);

/// A directory of its own, short enough for a unix socket, removed after.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = PathBuf::from(format!("/tmp/hp-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        Self(std::fs::canonicalize(p).unwrap())
    }

    fn dir(&self, name: &str) -> PathBuf {
        let p = self.0.join(name);
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The shared files, a directory; routes recorded.
struct Fake {
    shared: PathBuf,
    state: PathBuf,
    routes: Mutex<Vec<Vec<Route>>>,
}

impl Files for Fake {
    fn mount(&self) -> io::Result<()> {
        Ok(())
    }

    fn route(&self, routes: &[Route]) -> io::Result<()> {
        self.routes.lock().unwrap().push(routes.to_vec());
        Ok(())
    }

    fn shared(&self) -> &Path {
        &self.shared
    }

    fn state(&self) -> &Path {
        &self.state
    }
}

struct Env {
    scratch: Scratch,
    home: PathBuf,
    shared: PathBuf,
    state: PathBuf,
    guest: Arc<Guest<Fake>>,
}

fn env() -> Env {
    let scratch = Scratch::new();
    let (home, shared, state) = (
        scratch.dir("home"),
        scratch.dir("shared"),
        scratch.dir("state"),
    );
    let mut user = User::current();
    user.home = home.clone();
    let files = Fake {
        shared: shared.clone(),
        state: state.clone(),
        routes: Mutex::new(vec![]),
    };
    let (uid, gid) = (user.uid, user.gid);
    let guest = Arc::new(Guest::new(user, files).with_docker_homes(vec![(home.clone(), uid, gid)]));
    Env {
        scratch,
        home,
        shared,
        state,
        guest,
    }
}

impl Env {
    /// A session, as the server opens one.
    fn session(&self) -> Server {
        let (a, b) = tokio::io::duplex(1 << 20);
        let g = self.guest.clone();
        tokio::spawn(async move { g.serve(b).await });
        let (rd, wr) = tokio::io::split(a);
        Server {
            rd: BufReader::new(rd),
            wr,
        }
    }

    fn last_routes(&self) -> Vec<Route> {
        self.guest
            .files()
            .routes
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap_or_default()
    }

    fn read(&self, p: impl AsRef<Path>) -> String {
        std::fs::read_to_string(p).unwrap_or_default()
    }
}

/// The server's end of a session.
struct Server {
    rd: BufReader<ReadHalf<DuplexStream>>,
    wr: WriteHalf<DuplexStream>,
}

impl Server {
    async fn send(&mut self, m: Value) {
        let mut b = serde_json::to_vec(&m).unwrap();
        b.push(b'\n');
        self.wr.write_all(&b).await.unwrap();
    }

    /// The next message of type `kind`, passing over any other.
    async fn next(&mut self, kind: &str) -> Value {
        tokio::time::timeout(WAIT, async {
            loop {
                let mut line = String::new();
                assert!(
                    self.rd.read_line(&mut line).await.unwrap() > 0,
                    "the session ended"
                );
                let m: Value = serde_json::from_str(&line).unwrap();
                if m["type"] == kind {
                    return m;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("no {kind}"))
    }

    async fn paths(&mut self, sets: Value) {
        self.send(json!({"type": "paths", "sets": sets})).await;
    }
}

async fn eventually(what: &str, ok: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while !ok() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The routes copies give an environment: in the home directory under its
/// home, an exclusion as the guest's own, an absolute path where it is; a
/// path two copies name, the first's; a relative one, as a server before
/// packs sent, under home.
#[test]
fn routes() {
    let set = |id: &str, paths: &[&str]| SetPaths {
        id: id.into(),
        paths: paths.iter().map(|p| p.to_string()).collect(),
    };
    let got = routes_for(
        "/home/dev",
        &[
            set(
                "me",
                &[
                    "~/.gitconfig",
                    "~/.claude/",
                    "!~/.claude/projects/",
                    ".bashrc",
                ],
            ),
            set(
                "pack1",
                &[
                    "/workspace/app/.env",
                    "/home/dev/.gitconfig",
                    "/srv/conf/",
                    "~/.claude/x.json",
                ],
            ),
            set(
                "pack2",
                &[
                    "/workspace/app/.env",
                    "../bad",
                    "/x/../y",
                    "~/../x",
                    "/",
                    "~/a//b",
                ],
            ),
        ],
    );
    let route = |path: &str, target: &str, dir: bool| Route {
        path: path.into(),
        target: target.into(),
        dir,
        exclude: false,
    };
    let want = vec![
        route("/home/dev/.gitconfig", "me/~/.gitconfig", false),
        route("/home/dev/.claude", "me/~/.claude", true),
        Route {
            path: "/home/dev/.claude/projects".into(),
            exclude: true,
            ..Route::default()
        },
        route("/home/dev/.bashrc", "me/.bashrc", false),
        route("/workspace/app/.env", "pack1/workspace/app/.env", false),
        route("/srv/conf", "pack1/srv/conf", true),
    ];
    assert_eq!(got, want);
}

/// As paths become shared, a file only the environment has is copied up
/// into the copy, and one differing from the copy's is kept aside as a
/// conflict, which resolving with the environment's writes over the copy's;
/// a path that stops being shared is copied down, the environment keeping
/// it.
#[tokio::test(flavor = "multi_thread")]
async fn files_move_as_sharing_changes() {
    let e = env();
    let copy = e.shared.join("c1/~");
    std::fs::create_dir_all(copy.join(".tool")).unwrap();
    std::fs::write(copy.join(".tool/a"), "shared").unwrap();
    std::fs::write(e.home.join(".toolrc"), "local").unwrap();
    std::fs::create_dir_all(e.home.join(".tool")).unwrap();
    std::fs::write(e.home.join(".tool/a"), "mine").unwrap();
    // A directory at a path given without a slash.
    std::fs::create_dir_all(e.home.join(".kube")).unwrap();
    std::fs::write(e.home.join(".kube/config"), "clusters").unwrap();

    let mut s = e.session();
    s.send(json!({"type": "keys"})).await;
    s.paths(json!([{"id": "c1", "paths": ["~/.toolrc", "~/.tool/", "~/.kube"]}]))
        .await;
    let c = s.next("conflict").await;
    assert_eq!(
        (c["copy"].as_str(), c["path"].as_str()),
        (Some("c1"), Some("~/.tool/a"))
    );
    assert_eq!(s.next("routed").await["routed"], json!(["c1"]));
    tokio::time::timeout(WAIT, e.guest.synced())
        .await
        .expect("synced");

    assert_eq!(e.read(copy.join(".toolrc")), "local", "copied up");
    assert_eq!(
        e.read(copy.join(".kube/config")),
        "clusters",
        "a directory copied up"
    );
    assert_eq!(
        e.read(copy.join(".tool/a")),
        "shared",
        "the copy's file, in a conflict"
    );
    assert_eq!(
        e.read(e.state.join("kept/c1/~/.tool/a")),
        "mine",
        "kept aside"
    );
    let home = e.home.to_str().unwrap();
    assert!(e.last_routes().contains(&Route {
        path: format!("{home}/.tool"),
        target: "c1/~/.tool".into(),
        dir: true,
        exclude: false,
    }));
    assert!(
        e.read(e.home.join(".claude.json"))
            .contains("hasCompletedOnboarding")
    );

    s.send(
        json!({"type": "resolve", "copy": "c1", "path": "~/.tool/a", "resolution": "environment"}),
    )
    .await;
    s.next("resolved").await;
    assert_eq!(
        e.read(copy.join(".tool/a")),
        "mine",
        "the environment's, resolved"
    );
    assert!(
        !e.state.join("kept/c1").exists(),
        "what was kept aside is gone"
    );

    // Edited in the copy, then unshared: the environment keeps the
    // copy's.
    std::fs::write(copy.join(".toolrc"), "edited").unwrap();
    s.paths(json!([{"id": "c1", "paths": ["~/.tool/"]}])).await;
    s.next("routed").await;
    assert_eq!(e.read(e.home.join(".toolrc")), "edited", "copied down");
    assert_eq!(e.read(e.home.join(".kube/config")), "clusters");

    // The routes outlast the guest: one started again sorts from them.
    let routes: Value =
        serde_json::from_slice(&std::fs::read(e.state.join("routes.json")).unwrap()).unwrap();
    assert_eq!(routes[0]["Target"], "c1/~/.tool");
}

/// A shared file in a directory the environment lacks has the directory
/// made, so it is there to be found.
#[tokio::test(flavor = "multi_thread")]
async fn a_shared_files_directory_is_made() {
    let e = env();
    let mut s = e.session();
    s.paths(json!([{"id": "c1", "paths": ["~/.aws/config", "~/.cache/tool/state/"]}]))
        .await;
    s.next("routed").await;
    tokio::time::timeout(WAIT, e.guest.synced())
        .await
        .expect("synced");
    assert!(e.home.join(".aws").is_dir());
    assert!(e.home.join(".cache/tool").is_dir());
    assert!(
        !e.home.join(".cache/tool/state").exists(),
        "a shared directory was made in the environment rather than routed"
    );
}

/// A key in wire form, of a type and some bytes.
fn wire(kind: &str, body: &[u8]) -> Vec<u8> {
    let mut b = vec![];
    for s in [kind.as_bytes(), body] {
        b.extend_from_slice(&(s.len() as u32).to_be_bytes());
        b.extend_from_slice(s);
    }
    b
}

/// One request to the SSH agent, and its answer.
async fn agent(conn: &mut UnixStream, req: &[u8]) -> Vec<u8> {
    conn.write_all(&(req.len() as u32).to_be_bytes())
        .await
        .unwrap();
    conn.write_all(req).await.unwrap();
    let mut len = [0; 4];
    conn.read_exact(&mut len).await.unwrap();
    let mut out = vec![0; u32::from_be_bytes(len) as usize];
    conn.read_exact(&mut out).await.unwrap();
    out
}

/// The SSH agent offers the owner's keys, waiting for the session to send
/// them, and has the server sign; it takes no keys.
#[tokio::test(flavor = "multi_thread")]
async fn ssh_agent_signs_with_the_owners_key() {
    let e = env();
    let sock = e.scratch.0.join("agent.sock");
    tokio::spawn({
        let (g, sock) = (e.guest.clone(), sock.clone());
        async move { g.serve_ssh_agent(&sock).await }
    });
    eventually("the agent's socket", || sock.exists()).await;
    let mut conn = UnixStream::connect(&sock).await.unwrap();

    // Asked before there is a session, it waits for one.
    let listing = tokio::spawn(async move {
        let out = agent(&mut conn, &[11]).await;
        (conn, out)
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let key = wire("ssh-ed25519", &[7; 32]);
    let mut s = e.session();
    s.send(json!({"type": "keys", "keys": [
        format!("ssh-ed25519 {} laptop", STANDARD.encode(&key)),
        "not a key",
    ]}))
    .await;
    let (mut conn, out) = listing.await.unwrap();
    let mut want = vec![12, 0, 0, 0, 1];
    want.extend_from_slice(&(key.len() as u32).to_be_bytes());
    want.extend_from_slice(&key);
    want.extend_from_slice(&[0, 0, 0, 6]);
    want.extend_from_slice(b"hangar");
    assert_eq!(out, want, "the keys offered");

    let mut sign = vec![13];
    sign.extend_from_slice(&(key.len() as u32).to_be_bytes());
    sign.extend_from_slice(&key);
    sign.extend_from_slice(&[0, 0, 0, 15]);
    sign.extend_from_slice(b"session to sign");
    sign.extend_from_slice(&[0, 0, 0, 0]);
    let signing = tokio::spawn(async move {
        let out = agent(&mut conn, &sign).await;
        (conn, out)
    });
    let m = s.next("sign").await;
    assert_eq!(m["key"], STANDARD.encode(&key));
    assert_eq!(m["data"], STANDARD.encode(b"session to sign"));
    let sig = wire("ssh-ed25519", &[9; 64]);
    s.send(json!({"type": "signed", "id": m["id"], "signature": STANDARD.encode(&sig)}))
        .await;
    let (mut conn, out) = signing.await.unwrap();
    let mut want = vec![14];
    want.extend_from_slice(&(sig.len() as u32).to_be_bytes());
    want.extend_from_slice(&sig);
    assert_eq!(out, want, "the server's signature");

    assert_eq!(
        agent(&mut conn, &[17, 0, 0, 0, 0]).await,
        [5],
        "a key added"
    );
}

/// What the guest says of its coding agents reaches the server, sent again
/// with each session, and the environment's task goes both ways.
#[tokio::test(flavor = "multi_thread")]
async fn agents_and_tasks() {
    let e = env();
    let session = agents::Session {
        id: "s1".into(),
        agent: agents::CLAUDE.into(),
        terminal: String::new(),
        state: agents::State::Working,
        step: "Edit main.go".into(),
        message: String::new(),
        since: SystemTime::now(),
    };
    // Reported before there is a session, it is sent when one opens.
    e.guest.report_agents(vec![session]);
    let mut s = e.session();
    let m = s.next("agents").await;
    assert_eq!(m["agents"][0]["step"], "Edit main.go");
    assert_eq!(m["agents"][0]["state"], "working");
    e.guest.report_agents(vec![]);
    assert!(s.next("agents").await.get("agents").is_none(), "no agents");

    let setting = tokio::spawn({
        let g = e.guest.clone();
        async move { g.set_task("Fix rounding".into()).await }
    });
    let m = s.next("set_task").await;
    assert_eq!(m["text"], "Fix rounding");
    let task = json!({"id": "t1", "text": "Fix rounding", "set_by": "agent", "set_at": "2026-11-12T10:00:00Z"});
    s.send(json!({"type": "task_set", "id": m["id"], "task": task}))
        .await;
    let t = setting.await.unwrap().unwrap().unwrap();
    assert_eq!(
        (t.text.as_str(), t.set_by.as_str()),
        ("Fix rounding", "agent")
    );
    assert_eq!(e.guest.task().unwrap().id, "t1");

    let setting = tokio::spawn({
        let g = e.guest.clone();
        async move { g.set_task(" ".into()).await }
    });
    let m = s.next("set_task").await;
    s.send(json!({"type": "task_set", "id": m["id"], "error": "a task needs some text"}))
        .await;
    assert_eq!(setting.await.unwrap(), Err("a task needs some text".into()));

    s.send(json!({"type": "task", "task": {"id": "t2", "text": "Review the audit log"}}))
        .await;
    eventually("the owner's task", || {
        e.guest
            .task()
            .is_some_and(|t| t.text == "Review the audit log")
    })
    .await;
    s.send(json!({"type": "task"})).await;
    eventually("no task", || e.guest.task().is_none()).await;
}

/// Docker is pointed at the credential helper for Hangar's registry, and
/// the helper's credential is the server's, for that registry alone.
#[tokio::test(flavor = "multi_thread")]
async fn registry_credentials() {
    let e = env();
    let config = e.home.join(".docker/config.json");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(
        &config,
        r#"{"auths": {"ghcr.io": {}}, "credHelpers": {"gcr.io": "gcloud"}}"#,
    )
    .unwrap();
    let mut s = e.session();
    s.send(json!({"type": "registry", "host": "registry.hangar.localhost:8080"}))
        .await;
    eventually("docker's configuration", || {
        e.read(&config).contains("registry.hangar.localhost")
    })
    .await;
    let cfg: Value = serde_json::from_str(&e.read(&config)).unwrap();
    assert_eq!(
        cfg["credHelpers"]["registry.hangar.localhost:8080"],
        "hangar"
    );
    assert_eq!(
        cfg["credHelpers"]["gcr.io"], "gcloud",
        "another helper kept"
    );
    assert!(cfg["auths"]["ghcr.io"].is_object(), "the rest kept");

    let sock = e.scratch.0.join("registry.sock");
    tokio::spawn({
        let (g, sock) = (e.guest.clone(), sock.clone());
        async move { g.serve_credentials(&sock).await }
    });
    eventually("the credentials' socket", || sock.exists()).await;
    let ask = |host: &'static str| {
        let sock = sock.clone();
        tokio::spawn(async move {
            let mut c = UnixStream::connect(&sock).await.unwrap();
            c.write_all(format!("{host}\n").as_bytes()).await.unwrap();
            let mut out = String::new();
            BufReader::new(c).read_line(&mut out).await.unwrap();
            serde_json::from_str::<Value>(&out).unwrap()
        })
    };
    let asking = ask("registry.hangar.localhost:8080");
    let m = s.next("get_credential").await;
    s.send(
        json!({"type": "credential", "id": m["id"], "host": "registry.hangar.localhost:8080",
        "username": "alice", "secret": "hge_x"}),
    )
    .await;
    assert_eq!(
        asking.await.unwrap(),
        json!({"ServerURL": "registry.hangar.localhost:8080", "Username": "alice", "Secret": "hge_x"})
    );
    let asking = ask("ghcr.io");
    let m = s.next("get_credential").await;
    s.send(
        json!({"type": "credential", "id": m["id"], "host": "registry.hangar.localhost:8080",
        "username": "alice", "secret": "hge_x"}),
    )
    .await;
    let c = asking.await.unwrap();
    assert!(
        c.get("Secret").is_none(),
        "another registry was given the credential: {c}"
    );
    assert!(c["error"].as_str().unwrap().contains("not ghcr.io"));
}

/// A new session replaces the one before it, which ends; what was asked
/// through it fails.
#[tokio::test(flavor = "multi_thread")]
async fn a_new_session_replaces_the_old() {
    let e = env();
    let mut old = e.session();
    let setting = tokio::spawn({
        let g = e.guest.clone();
        async move { g.set_task("Fix it".into()).await }
    });
    old.next("set_task").await;
    let mut new = e.session();
    let mut line = String::new();
    let n = tokio::time::timeout(WAIT, async {
        loop {
            line.clear();
            let n = old.rd.read_line(&mut line).await.unwrap();
            if n == 0 {
                return n;
            }
        }
    })
    .await
    .expect("the old session ended");
    assert_eq!(n, 0);
    assert_eq!(
        setting.await.unwrap(),
        Err("the profile session ended".into())
    );
    new.send(json!({"type": "keys"})).await;
    new.paths(json!([])).await;
    new.next("routed").await;
}
