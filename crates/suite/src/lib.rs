//! Black-box tests of a Hangar server binary: the one `HANGAR_SUITE_SERVER`
//! names, started afresh for each test with the environment the Go server
//! takes (`cmd/hangar-server`), and spoken to only as a client would, over
//! its APIs. So the same tests check the Go server and its Rust
//! replacement (docs/server-rust.md).
//!
//! Without `HANGAR_SUITE_SERVER`, or without `HANGAR_TEST_DATABASE_URL` (a
//! Postgres to make each server's database on, as the Go tests' dbtest
//! has), `Server::start` gives None and a test passes over its checks.
//! Blobs go to `HANGAR_SUITE_BLOB_URL`, the dev stack's RustFS unless set.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub use hangar_openapi::{clientapi, frontendapi};

pub mod worker;

/// The administrator every server starts with.
pub const ADMIN: &str = "admin";
/// Its password.
pub const ADMIN_PASSWORD: &str = "suite-admin-password";
/// The token a worker first presents.
pub const BOOTSTRAP_TOKEN: &str = "suite-bootstrap-token";

/// The name every server is reached by; its subdomains are its
/// environments' hosts.
const HOST: &str = "hangar.localhost";

const DEFAULT_BLOB_URL: &str = "http://hangar:hangar-dev-secret@127.0.0.1:59000/hangar-suite";

/// A server under test, stopped and its database dropped when it goes. Its
/// log is printed if the test is failing as it goes.
pub struct Server {
    child: Child,
    port: u16,
    ssh_port: u16,
    dir: PathBuf,
    log: PathBuf,
    _database: Database,
}

/// A database of the suite's, dropped when this goes, whatever becomes of
/// the server meant for it.
struct Database {
    admin: String,
    name: String,
}

impl Drop for Database {
    fn drop(&mut self) {
        drop_database(self.admin.clone(), self.name.clone());
    }
}

impl Server {
    /// Starts a server of the test's own, or None when the suite has no
    /// server or database to start one with.
    pub fn start() -> Option<Server> {
        let bin = std::env::var("HANGAR_SUITE_SERVER").ok()?;
        let admin_db = std::env::var("HANGAR_TEST_DATABASE_URL").ok()?;
        let blobs =
            std::env::var("HANGAR_SUITE_BLOB_URL").unwrap_or_else(|_| DEFAULT_BLOB_URL.into());
        let name = unique("hangar_suite");
        let db_url = create_database(&admin_db, &name);
        let database = Database {
            admin: admin_db,
            name,
        };
        let dir = std::env::temp_dir().join(unique("hangar-suite"));
        fs::create_dir_all(dir.join("files")).expect("the server's directory");
        let key = dir.join("secret-key");
        fs::write(
            &key,
            "4f8a1c2e9b7d6035a1c4e8f20b9d7a36c5e1f8a2b4d6c8e0f1a3b5c7d9e2f4a6",
        )
        .expect("the secret key");
        let (port, ssh_port) = (free_port(), free_port());
        let log = dir.join("server.log");
        let out = fs::File::create(&log).expect("the server's log");
        let child = Command::new(&bin)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &dir)
            .env("HANGAR_LISTEN", format!("127.0.0.1:{port}"))
            .env("HANGAR_DATABASE_URL", &db_url)
            .env("HANGAR_BOOTSTRAP_TOKEN", BOOTSTRAP_TOKEN)
            .env("HANGAR_INITIAL_ADMIN_USERNAME", ADMIN)
            .env("HANGAR_INITIAL_ADMIN_PASSWORD", ADMIN_PASSWORD)
            .env("HANGAR_SECRET_KEY_FILE", &key)
            .env("HANGAR_FILES_DIR", dir.join("files"))
            .env("HANGAR_BLOB_URL", blobs)
            .env("HANGAR_PUBLIC_URL", format!("http://{HOST}:{port}"))
            .env("HANGAR_SSH_LISTEN", format!("127.0.0.1:{ssh_port}"))
            .env("HANGAR_SSH_ADDRESS", format!("{HOST}:{ssh_port}"))
            .stdin(Stdio::null())
            .stdout(out.try_clone().expect("the log"))
            .stderr(out)
            .spawn()
            .unwrap_or_else(|e| panic!("starting {bin}: {e}"));
        let mut s = Server {
            child,
            port,
            ssh_port,
            dir,
            log,
            _database: database,
        };
        s.wait_ready();
        Some(s)
    }

    /// The server's address: http://hangar.localhost:<port>.
    pub fn url(&self) -> String {
        format!("http://{HOST}:{}", self.port)
    }

    /// Where its SSH gateway listens.
    pub fn ssh_addr(&self) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], self.ssh_port))
    }

    /// An HTTP client that reaches the server, and its environments' hosts,
    /// by its name, keeping the cookies it is given.
    pub fn http(&self) -> reqwest::Client {
        let addr = SocketAddr::from(([127, 0, 0, 1], self.port));
        hangar_openapi::http_client()
            .expect("TLS")
            .resolve(HOST, addr)
            .cookie_store(true)
            .timeout(Duration::from_secs(30))
            .build()
            .expect("an HTTP client")
    }

    /// As http, also reaching names, hosts of the server's environments,
    /// and following no redirect, so a test sees where it is sent.
    pub fn http_to(&self, names: &[&str]) -> reqwest::Client {
        let addr = SocketAddr::from(([127, 0, 0, 1], self.port));
        let mut b = hangar_openapi::http_client()
            .expect("TLS")
            .resolve(HOST, addr)
            .cookie_store(true)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30));
        for name in names {
            b = b.resolve(name, addr);
        }
        b.build().expect("an HTTP client")
    }

    /// An environment's host on the server, under name: `<name>-env<short>`.
    pub fn environment_host(&self, name: &str, short: &str) -> String {
        format!("{name}-env{short}.{HOST}")
    }

    /// The server's port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The web UI's API, signed in as no one.
    pub fn frontend(&self) -> frontendapi::Client {
        frontendapi::Client::new_with_client(&self.url(), self.http())
    }

    /// The web UI's API, signed in as username with password.
    pub async fn sign_in(&self, username: &str, password: &str) -> frontendapi::Client {
        let c = self.frontend();
        c.login(&frontendapi::types::Login {
            username: username.parse().expect("a username the spec allows"),
            password: password.parse().expect("a password the spec allows"),
        })
        .await
        .unwrap_or_else(|e| panic!("signing in as {username}: {e}"));
        c
    }

    /// The web UI's API, signed in as the administrator.
    pub async fn admin(&self) -> frontendapi::Client {
        self.sign_in(ADMIN, ADMIN_PASSWORD).await
    }

    /// The client API, with no token.
    pub fn anonymous(&self) -> clientapi::Client {
        clientapi::Client::new_with_client(&format!("{}/api/v1", self.url()), self.http())
    }

    /// The client API, signed in with token.
    pub fn client(&self, token: &str) -> clientapi::Client {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {token}").parse().expect("a header"),
        );
        let addr = SocketAddr::from(([127, 0, 0, 1], self.port));
        let http = hangar_openapi::http_client()
            .expect("TLS")
            .resolve(HOST, addr)
            .default_headers(headers)
            .timeout(Duration::from_secs(30))
            .build()
            .expect("an HTTP client");
        clientapi::Client::new_with_client(&format!("{}/api/v1", self.url()), http)
    }

    /// Makes an access token for whoever session is signed in as.
    pub async fn token(&self, session: &frontendapi::Client, name: &str) -> String {
        session
            .create_token(&frontendapi::types::CreateToken {
                name: name.into(),
                expires_in_days: None,
            })
            .await
            .unwrap_or_else(|e| panic!("making a token: {e}"))
            .into_inner()
            .token
    }

    /// Starts a worker named name for this server, which takes templates
    /// placed on `worker::LABEL`.
    pub fn worker(&self, name: &str) -> worker::Worker {
        worker::Worker::start(&format!("http://127.0.0.1:{}", self.port), name)
    }

    /// As worker, presenting token as the bootstrap token.
    pub fn worker_with_token(&self, name: &str, token: &str) -> worker::Worker {
        worker::Worker::start_with_token(&format!("http://127.0.0.1:{}", self.port), name, token)
    }

    /// Makes a user, who is no administrator, and gives a session signed in
    /// as them.
    pub async fn user(&self, admin: &frontendapi::Client, username: &str) -> frontendapi::Client {
        admin
            .create_user(&body(serde_json::json!({
                "username": username,
                "password": format!("{username}-password"),
            })))
            .await
            .unwrap_or_else(|e| panic!("making {username}: {}", why(&e)));
        self.sign_in(username, &format!("{username}-password"))
            .await
    }

    /// Waits for the server to answer, failing the test with its log if it
    /// exits or does not answer within 60 seconds.
    fn wait_ready(&mut self) {
        let addr = SocketAddr::from(([127, 0, 0, 1], self.port));
        let start = Instant::now();
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                panic!("the server exited ({status}):\n{}", self.log_text());
            }
            if std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
                return;
            }
            if start.elapsed() > Duration::from_secs(60) {
                panic!("the server did not listen in 60 s:\n{}", self.log_text());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// What the server has logged.
    pub fn log_text(&self) -> String {
        let mut text = String::new();
        if let Ok(mut f) = fs::File::open(&self.log) {
            let len = f.metadata().map(|m| m.len()).unwrap_or(0);
            let _ = f.seek(SeekFrom::Start(len.saturating_sub(64 << 10)));
            let _ = f.read_to_string(&mut text);
        }
        text
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if std::thread::panicking() {
            eprintln!("--- the server's log ---\n{}", self.log_text());
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// A name no other test's has: prefix, the process and a count.
fn unique(prefix: &str) -> String {
    static N: AtomicU32 = AtomicU32::new(0);
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() % 1_000_000)
        .unwrap_or(0);
    format!(
        "{prefix}_{}_{t}_{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

/// A port nothing listens on, a moment ago.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .expect("a free port")
}

/// Makes a database on the server admin names, and gives its URL. Postgres's
/// blocking client keeps a runtime of its own, so it runs on a thread
/// outside the test's.
fn create_database(admin: &str, name: &str) -> String {
    let (admin, name) = (admin.to_string(), name.to_string());
    std::thread::spawn(move || {
        let mut c = postgres::Client::connect(&admin, postgres::NoTls)
            .unwrap_or_else(|e| panic!("connecting to HANGAR_TEST_DATABASE_URL: {e}"));
        c.batch_execute(&format!("CREATE DATABASE \"{name}\""))
            .unwrap_or_else(|e| panic!("making the database: {e}"));
        database_url(&admin, &name)
    })
    .join()
    .expect("making the database")
}

fn drop_database(admin: String, name: String) {
    let _ = std::thread::spawn(move || {
        if let Ok(mut c) = postgres::Client::connect(&admin, postgres::NoTls) {
            let _ = c.batch_execute(&format!("DROP DATABASE \"{name}\" WITH (FORCE)"));
        }
    })
    .join();
}

/// admin's URL with its database name replaced by name.
fn database_url(admin: &str, name: &str) -> String {
    let (base, query) = admin.split_once('?').unwrap_or((admin, ""));
    let base = match base.rfind('/') {
        Some(i) if base[..i].contains("://") && i > base.find("://").unwrap_or(0) + 2 => &base[..i],
        _ => base,
    };
    if query.is_empty() {
        format!("{base}/{name}")
    } else {
        format!("{base}/{name}?{query}")
    }
}

/// A request body of the spec's, written as JSON: the spec's types check
/// it as they read it.
pub fn body<T: serde::de::DeserializeOwned>(json: serde_json::Value) -> T {
    serde_json::from_value(json.clone())
        .unwrap_or_else(|e| panic!("a body the spec does not take ({e}): {json}"))
}

/// Asks f until it gives something, for up to 20 seconds, failing the test
/// with what it was waiting for.
pub async fn eventually<T, F, Fut>(what: &str, mut f: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let start = Instant::now();
    loop {
        if let Some(v) = f().await {
            return v;
        }
        if start.elapsed() > Duration::from_secs(20) {
            panic!("timed out waiting for {what}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Why a call failed, saying where an answer broke the spec when it did:
/// the suite's first sign of a server out of step with its spec.
pub fn why<E: std::fmt::Debug>(e: &frontendapi::Error<E>) -> String {
    match e {
        frontendapi::Error::InvalidResponsePayload(_, broke) => {
            format!("an answer the spec does not allow: {broke}")
        }
        e => e.to_string(),
    }
}

/// A template body for a small headless machine on the suite's workers.
pub fn machine(name: &str, visibility: &str) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "visibility": visibility,
        "spec": {
            "image": "ghcr.io/csnewman/hangar/minimal:ubuntu-26.04",
            "cpus": 1,
            "memory_mib": 1024,
            "display": "none",
            "gpu": "none",
            "placement": { worker::LABEL.0: worker::LABEL.1 },
            "repos": [],
        },
    })
}

/// The status a call was refused with.
pub fn status<T, E>(r: Result<T, frontendapi::Error<E>>) -> Option<u16> {
    r.err().and_then(|e| e.status()).map(|s| s.as_u16())
}

/// The client API's event stream, read an event at a time.
pub struct Events {
    stream: std::pin::Pin<
        Box<dyn futures_core::Stream<Item = reqwest::Result<bytes::Bytes>> + Send + Sync>,
    >,
    buf: String,
}

/// One server-sent event: its type and its data.
#[derive(Debug, Clone)]
pub struct Event {
    pub kind: String,
    pub data: serde_json::Value,
}

impl Events {
    /// Opens client's stream.
    pub async fn open(client: &clientapi::Client) -> Events {
        let bytes = client
            .stream_events()
            .await
            .unwrap_or_else(|e| panic!("opening the event stream: {}", why(&e)))
            .into_inner();
        Events {
            stream: bytes.into_inner(),
            buf: String::new(),
        }
    }

    /// The next event, or None if none comes within 20 seconds or the stream
    /// ends. Comments (the server's keepalives) are passed over.
    pub async fn next(&mut self) -> Option<Event> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(end) = self.buf.find("\n\n") {
                let block: String = self.buf.drain(..end + 2).collect();
                let (mut kind, mut data) = (String::new(), String::new());
                for line in block.lines() {
                    if let Some(v) = line.strip_prefix("event:") {
                        kind = v.trim().to_string();
                    } else if let Some(v) = line.strip_prefix("data:") {
                        data.push_str(v.trim_start());
                    }
                }
                if kind.is_empty() && data.is_empty() {
                    continue;
                }
                return Some(Event {
                    kind,
                    data: serde_json::from_str(&data).unwrap_or(serde_json::Value::Null),
                });
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            let chunk = tokio::time::timeout(
                left,
                std::future::poll_fn(|cx| self.stream.as_mut().poll_next(cx)),
            )
            .await
            .ok()??
            .ok()?;
            self.buf
                .push_str(&String::from_utf8_lossy(&chunk).replace("\r\n", "\n"));
        }
    }

    /// Reads events until one of kind whose data's id is id comes, or none
    /// does within 20 seconds.
    pub async fn until(&mut self, kind: &str, id: &str) -> Option<Event> {
        while let Some(e) = self.next().await {
            if e.kind == kind && e.data["id"] == id {
                return Some(e);
            }
        }
        None
    }
}
