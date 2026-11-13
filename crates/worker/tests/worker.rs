//! The worker against a server of the test's own, which speaks the worker
//! API as hangar-server does: registration, the desired set, status
//! reports, shared files, and the tunnel the server opens streams over.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, ready};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use hangar_api::{
    DesiredSet, DesiredState, EnvironmentFiles, EnvironmentSpec, ObservedEnvironment, Phase,
    WorkerStatus,
};
use hangar_images::HangarRegistry;
use hangar_vm::log::Log;
use hangar_vm::runtime::FilesView;
use hangar_worker::config::Config;
use hangar_worker::logs::{Reply, Rings};
use hangar_worker::{Runtime, Worker, client};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_util::compat::FuturesAsyncReadCompatExt;

const TOKEN: &str = "boot-token";
const CREDENTIAL: &str = "worker-credential-1";

#[derive(Default)]
struct State {
    registered: Vec<hangar_api::RegisterWorker>,
    statuses: Vec<WorkerStatus>,
    desired: DesiredSet,
    files: EnvironmentFiles,
    reject: bool,
    opener: Option<mpsc::Sender<oneshot::Sender<yamux::Stream>>>,
    tunnels: usize,
}

#[derive(Clone, Default)]
struct Server {
    st: Arc<Mutex<State>>,
    changed: Arc<Notify>,
    /// Drops the tunnel held, as a server restarting would.
    cut: Arc<Notify>,
}

impl Server {
    async fn start() -> (Server, String) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let srv = Server::default();
        let s = srv.clone();
        tokio::spawn(async move {
            while let Ok((c, _)) = l.accept().await {
                tokio::spawn(s.clone().serve(c));
            }
        });
        (srv, url)
    }

    fn set_desired(&self, f: impl FnOnce(&mut DesiredSet)) {
        let mut st = self.st.lock().unwrap();
        f(&mut st.desired);
        st.desired.version += 1;
        drop(st);
        self.changed.notify_waiters();
    }

    async fn serve(self, c: TcpStream) {
        let mut head = [0u8; 64];
        loop {
            let Ok(n) = c.peek(&mut head).await else {
                return;
            };
            if n == 0 {
                return;
            }
            if n >= 40 || head[..n].contains(&b'\n') {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        if head.starts_with(b"GET /api/worker/v1/tunnel") {
            return self.tunnel(c).await;
        }
        let mut r = BufReader::new(c);
        loop {
            let mut line = String::new();
            if r.read_line(&mut line).await.unwrap_or(0) == 0 {
                return;
            }
            let mut parts = line.split_whitespace();
            let (method, path) = (
                parts.next().unwrap_or("").to_string(),
                parts.next().unwrap_or("").to_string(),
            );
            let (mut length, mut auth) = (0usize, String::new());
            loop {
                let mut h = String::new();
                r.read_line(&mut h).await.unwrap();
                let h = h.trim_end();
                if h.is_empty() {
                    break;
                }
                let (k, v) = h.split_once(':').unwrap();
                match k.to_ascii_lowercase().as_str() {
                    "content-length" => length = v.trim().parse().unwrap(),
                    "authorization" => auth = v.trim().to_string(),
                    _ => {}
                }
            }
            let mut body = vec![0; length];
            r.read_exact(&mut body).await.unwrap();
            let (status, reply) = self.answer(&method, &path, &auth, &body).await;
            let out = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{reply}",
                reply.len()
            );
            if r.get_mut().write_all(out.as_bytes()).await.is_err() {
                return;
            }
        }
    }

    async fn answer(&self, method: &str, path: &str, auth: &str, body: &[u8]) -> (u16, String) {
        let rejected = (401, r#"{"error":"no"}"#.to_string());
        if self.st.lock().unwrap().reject {
            return rejected;
        }
        if (method, path) == ("POST", "/api/worker/v1/register") {
            if auth != format!("Bearer {TOKEN}") {
                return rejected;
            }
            let req = serde_json::from_slice(body).unwrap();
            self.st.lock().unwrap().registered.push(req);
            return (200, format!(r#"{{"id":"w1","credential":"{CREDENTIAL}"}}"#));
        }
        if auth != format!("Bearer {CREDENTIAL}") {
            return rejected;
        }
        if let Some(after) = path.strip_prefix("/api/worker/v1/desired?after=") {
            let after: i64 = after.parse().unwrap();
            let wait = self.changed.notified();
            if self.st.lock().unwrap().desired.version <= after {
                let _ = tokio::time::timeout(Duration::from_millis(500), wait).await;
            }
            let set = self.st.lock().unwrap().desired.clone();
            return (200, serde_json::to_string(&set).unwrap());
        }
        if (method, path) == ("PUT", "/api/worker/v1/status") {
            let st: WorkerStatus = serde_json::from_slice(body).unwrap();
            self.st.lock().unwrap().statuses.push(st);
            return (204, String::new());
        }
        if path.starts_with("/api/worker/v1/environments/") && path.ends_with("/files") {
            let f = self.st.lock().unwrap().files.clone();
            return (200, serde_json::to_string(&f).unwrap());
        }
        (404, r#"{"error":"no such route"}"#.into())
    }

    #[allow(clippy::result_large_err)]
    async fn tunnel(self, c: TcpStream) {
        let check = |req: &Request, resp: Response| -> Result<Response, ErrorResponse> {
            let ok = req
                .headers()
                .get("authorization")
                .is_some_and(|v| v == &format!("Bearer {CREDENTIAL}")[..]);
            if ok {
                Ok(resp)
            } else {
                let mut e = ErrorResponse::new(None);
                *e.status_mut() = 401.try_into().unwrap();
                Err(e)
            }
        };
        let Ok(ws) = tokio_tungstenite::accept_hdr_async(c, check).await else {
            return;
        };
        let mut conn =
            yamux::Connection::new(Ws::new(ws), yamux::Config::default(), yamux::Mode::Client);
        let (tx, mut opens) = mpsc::channel::<oneshot::Sender<yamux::Stream>>(8);
        {
            let mut st = self.st.lock().unwrap();
            st.opener = Some(tx);
            st.tunnels += 1;
        }
        let mut pending: Option<oneshot::Sender<yamux::Stream>> = None;
        let cut = self.cut.notified();
        let drive = std::future::poll_fn(|cx| {
            loop {
                if pending.is_none()
                    && let Poll::Ready(Some(tx)) = opens.poll_recv(cx)
                {
                    pending = Some(tx);
                }
                if pending.is_some()
                    && let Poll::Ready(r) = conn.poll_new_outbound(cx)
                {
                    let _ = pending.take().unwrap().send(r.unwrap());
                    continue;
                }
                match conn.poll_next_inbound(cx) {
                    Poll::Ready(Some(Ok(_))) => continue,
                    Poll::Ready(_) => return Poll::Ready(()),
                    Poll::Pending => return Poll::Pending,
                }
            }
        });
        tokio::select! {
            _ = drive => {}
            _ = cut => {}
        }
        self.st.lock().unwrap().opener = None;
    }

    /// Opens a stream to the worker, its header written.
    async fn open(&self, kind: &str, env: &str) -> tokio_util::compat::Compat<yamux::Stream> {
        let opener = wait(|| self.st.lock().unwrap().opener.clone()).await;
        let (tx, rx) = oneshot::channel();
        opener.send(tx).await.unwrap();
        let mut s = rx.await.unwrap().compat();
        s.write_all(format!("{{\"kind\":\"{kind}\",\"environment\":\"{env}\"}}\n").as_bytes())
            .await
            .unwrap();
        s
    }
}

/// A WebSocket as the server's end of the tunnel: one binary message per
/// write.
struct Ws {
    ws: WebSocketStream<TcpStream>,
    rbuf: Vec<u8>,
}

impl Ws {
    fn new(ws: WebSocketStream<TcpStream>) -> Ws {
        Ws {
            ws,
            rbuf: Vec::new(),
        }
    }
}

impl futures_util::io::AsyncRead for Ws {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        loop {
            if !this.rbuf.is_empty() {
                let n = out.len().min(this.rbuf.len());
                out[..n].copy_from_slice(&this.rbuf[..n]);
                this.rbuf.drain(..n);
                return Poll::Ready(Ok(n));
            }
            match ready!(this.ws.poll_next_unpin(cx)) {
                Some(Ok(Message::Binary(b))) => this.rbuf = b.to_vec(),
                Some(Ok(Message::Close(_))) | None => return Poll::Ready(Ok(0)),
                Some(Ok(_)) => {}
                Some(Err(e)) => return Poll::Ready(Err(io::Error::other(e))),
            }
        }
    }
}

impl futures_util::io::AsyncWrite for Ws {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        ready!(this.ws.poll_ready_unpin(cx)).map_err(io::Error::other)?;
        this.ws
            .start_send_unpin(Message::Binary(data.to_vec().into()))
            .map_err(io::Error::other)?;
        Poll::Ready(Ok(data.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut()
            .ws
            .poll_flush_unpin(cx)
            .map_err(io::Error::other)
    }
    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut()
            .ws
            .poll_close_unpin(cx)
            .map_err(io::Error::other)
    }
}

/// A runtime that keeps what it is asked for and answers each port with an
/// echo of what it is sent, after a greeting naming the port.
#[derive(Clone, Default)]
struct Fake {
    applied: Arc<Mutex<Vec<EnvironmentSpec>>>,
    registry: Arc<Mutex<Option<HangarRegistry>>>,
    files: Arc<Mutex<Option<(PathBuf, FilesView)>>>,
    changed: Arc<Notify>,
}

impl Runtime for Fake {
    type Conn = DuplexStream;

    fn apply(&self, spec: EnvironmentSpec) {
        self.applied.lock().unwrap().push(spec);
        self.changed.notify_one();
    }

    fn observe(&self) -> Vec<ObservedEnvironment> {
        let mut last = BTreeMap::new();
        for s in self.applied.lock().unwrap().iter() {
            last.insert(s.id.clone(), s.desired);
        }
        last.into_iter()
            .filter(|(_, d)| *d != DesiredState::Deleted)
            .map(|(id, d)| ObservedEnvironment {
                id,
                phase: if d == DesiredState::Running {
                    Phase::Running
                } else {
                    Phase::Stopped
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
        if id != "e1" {
            return Err(io::Error::other(format!(
                "environment {id} is not on this worker"
            )));
        }
        let (a, mut b) = tokio::io::duplex(1 << 16);
        tokio::spawn(async move {
            let _ = b.write_all(format!("port {port}\n").as_bytes()).await;
            let mut buf = [0u8; 4096];
            loop {
                match b.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => {
                        if b.write_all(&buf[..n]).await.is_err() {
                            return;
                        }
                    }
                }
            }
        });
        Ok(a)
    }

    fn use_registry(&self, hangar: Option<HangarRegistry>) {
        *self.registry.lock().unwrap() = hangar;
    }

    fn use_shared_files(&self, root: PathBuf, view: FilesView) {
        *self.files.lock().unwrap() = Some((root, view));
    }
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hangar-worker-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn config(url: &str, dir: &std::path::Path) -> Config {
    std::fs::write(dir.join("token"), format!("{TOKEN}\n")).unwrap();
    Config::parse(&format!(
        "server:\n  url: {url}\n  token_file: {}\n  credential_file: {}\n\
         node:\n  name: test-worker\n  labels:\n    runtime: fake\n\
         storage:\n  environments: {}\n  files: {}\n\
         runtime: simulated\n",
        dir.join("token").display(),
        dir.join("state/credential").display(),
        dir.join("env").display(),
        dir.join("files").display(),
    ))
    .unwrap()
}

struct TestLog;
impl log::Log for TestLog {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, r: &log::Record) {
        eprintln!(
            "{:?} {} {} {}",
            std::thread::current().id(),
            r.level(),
            r.target(),
            r.args()
        );
    }
    fn flush(&self) {}
}

fn trace() {
    if std::env::var_os("HANGAR_TEST_LOG").is_some() {
        let _ = log::set_logger(&TestLog);
        log::set_max_level(log::LevelFilter::Trace);
    }
}

/// Waits for f, for up to ten seconds, saying what for.
async fn within<T>(what: &str, f: impl std::future::Future<Output = T>) -> T {
    match tokio::time::timeout(Duration::from_secs(10), f).await {
        Ok(v) => v,
        Err(_) => panic!("timed out: {what}"),
    }
}

/// Polls until f has something to give, for up to ten seconds.
async fn wait<T>(mut f: impl FnMut() -> Option<T>) -> T {
    for _ in 0..1000 {
        if let Some(v) = f() {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("gave up waiting");
}

fn spec(id: &str, desired: &str) -> EnvironmentSpec {
    serde_json::from_value(serde_json::json!({"id": id, "desired": desired})).unwrap()
}

/// A worker registers with the bootstrap token, keeps the credential it is
/// given, applies every desired set whole, reports what it holds, and looks
/// up each environment's shared files.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn holds_the_desired_set() {
    let (srv, url) = Server::start().await;
    let dir = scratch("desired");
    let rt = Fake::default();
    let w = Worker::new(config(&url, &dir), rt.clone(), Log::default(), None).unwrap();
    let run = tokio::spawn(w.clone().run());

    let reg = wait(|| srv.st.lock().unwrap().registered.first().cloned()).await;
    assert_eq!(reg.name, "test-worker");
    assert_eq!(reg.labels.get("runtime").map(String::as_str), Some("fake"));
    let cred = dir.join("state/credential");
    let saved = wait(|| std::fs::read_to_string(&cred).ok()).await;
    assert_eq!(saved, format!("{CREDENTIAL}\n"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&cred).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    srv.set_desired(|d| {
        d.environments = vec![spec("e1", "running"), spec("e2", "stopped")];
        d.remove_environments = vec!["gone".into()];
        d.registry = "registry.hangar.test".into();
    });
    let applied = wait(|| {
        let a = rt.applied.lock().unwrap();
        (a.len() >= 3).then(|| a.clone())
    })
    .await;
    assert_eq!(applied[0].id, "e1");
    assert_eq!(applied[1].id, "e2");
    assert_eq!(
        (applied[2].id.as_str(), applied[2].desired),
        ("gone", DesiredState::Deleted)
    );
    let reg = rt.registry.lock().unwrap().clone().expect("the registry");
    assert_eq!(reg.host, "registry.hangar.test");
    assert_eq!(reg.url, format!("{url}/api/worker/v1/registry"));
    assert_eq!(reg.credential, CREDENTIAL);

    // Each change is reported, soon.
    let st = wait(|| {
        srv.st
            .lock()
            .unwrap()
            .statuses
            .iter()
            .find(|s| s.environments.len() == 2)
            .cloned()
    })
    .await;
    assert_eq!(st.capacity, w.capacity());
    assert_eq!(st.labels.get("runtime").map(String::as_str), Some("fake"));
    assert!(
        st.stats.as_ref().is_some_and(|s| s.memory_total_mib > 0),
        "{st:?}"
    );

    // An environment's shared files are what the server says, kept current.
    let (root, view) = rt.files.lock().unwrap().clone().expect("shared files");
    assert_eq!(root, dir.join("files"));
    assert!(root.is_dir());
    srv.st.lock().unwrap().files = serde_json::from_value(serde_json::json!({
        "sets": ["s1"], "hidden": {"s1": [".ssh/"]}, "squash": {"s1": "root_squash"}
    }))
    .unwrap();
    let v = view("e1");
    wait(|| (!v.sets().is_empty()).then_some(())).await;
    assert_eq!(v.sets(), ["s1"]);
    assert!(v.hidden("s1", b".ssh"));
    assert!(v.hidden("s1", b".ssh/id"));
    assert!(!v.hidden("s1", b".sshrc"));
    assert_eq!(v.squash("s1"), hangar_nfs::Squash::Root);
    assert_eq!(v.squash("s2"), hangar_nfs::Squash::All);

    // A worker that starts again keeps its credential.
    run.abort();
    let w2 = Worker::new(config(&url, &dir), Fake::default(), Log::default(), None).unwrap();
    let run2 = tokio::spawn(w2.run());
    let before = srv.st.lock().unwrap().statuses.len();
    wait(|| (srv.st.lock().unwrap().statuses.len() > before).then_some(())).await;
    assert_eq!(srv.st.lock().unwrap().registered.len(), 1);
    run2.abort();
}

/// The server's streams reach the environment's services through the
/// runtime, and a request for a log is answered by the worker itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relays_streams() {
    trace();
    let (srv, url) = Server::start().await;
    let dir = scratch("streams");
    let rings = Arc::new(Rings::new());
    let w = Worker::new(
        config(&url, &dir),
        Fake::default(),
        Log::default(),
        Some(rings.clone()),
    )
    .unwrap();
    let run = tokio::spawn(w.run());

    let mut s = BufReader::new(within("opening", srv.open("terminal", "e1")).await);
    let mut line = String::new();
    within("the greeting", s.read_line(&mut line))
        .await
        .unwrap();
    assert_eq!(line, "port 101\n");
    // Bytes go both ways as they are, a lot of them included. Both
    // directions are worked from this one task, as a yamux stream needs.
    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    let (mut r, mut wr) = tokio::io::split(s);
    let mut got = vec![0; big.len() + 4];
    let (sent, read) = within("the echo", async {
        tokio::join!(
            async {
                wr.write_all(&big).await?;
                wr.write_all(b"end\n").await
            },
            r.read_exact(&mut got)
        )
    })
    .await;
    sent.unwrap();
    read.unwrap();
    assert!(got[..big.len()] == big[..] && &got[big.len()..] == b"end\n");
    // Closing its end closes the environment's, and so the stream.
    wr.shutdown().await.unwrap();
    let mut rest = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), r.read_to_end(&mut rest))
        .await
        .expect("the stream closed")
        .unwrap();

    let mut s = BufReader::new(within("opening ssh", srv.open("ssh", "e1")).await);
    let mut line = String::new();
    within("ssh's greeting", s.read_line(&mut line))
        .await
        .unwrap();
    assert_eq!(line, "port 107\n");

    // An environment the runtime cannot reach, and a kind there is no
    // such thing as, close the stream.
    for (kind, env) in [("code", "nowhere"), ("teleport", "e1")] {
        let mut s = within("opening", srv.open(kind, env)).await;
        let mut rest = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut rest))
            .await
            .expect("the stream closed")
            .ok();
        assert!(rest.is_empty(), "{kind}: {rest:?}");
    }

    rings.write("", b"time=x level=INFO msg=\"worker ready\"\n");
    rings.write("e1", b"time=x level=INFO msg=booting environment=e1\n");
    let ask = |req: &'static str| {
        let srv = srv.clone();
        async move {
            let mut s = BufReader::new(within("opening logs", srv.open("logs", "")).await);
            s.write_all(req.as_bytes()).await.unwrap();
            s.write_all(b"\n").await.unwrap();
            let mut line = String::new();
            within(req, s.read_line(&mut line)).await.unwrap();
            serde_json::from_str::<Reply>(&line).unwrap()
        }
    };
    let whole = ask(r#"{"log":"worker","offset":0}"#).await;
    let text = String::from_utf8(whole.data).unwrap();
    assert!(
        text.contains("worker ready") && text.contains("booting"),
        "{text}"
    );
    let e1 = ask(r#"{"environment":"e1","log":"worker","offset":0}"#).await;
    assert_eq!(
        String::from_utf8(e1.data).unwrap(),
        "time=x level=INFO msg=booting environment=e1\n"
    );
    assert!(ask(r#"{"environment":"e1","log":"console"}"#).await.missing);
    assert!(ask(r#"{"log":"console"}"#).await.missing);
    assert_eq!(ask("not json").await.error, "a malformed request");
    run.abort();
}

/// A tunnel that drops is dialled again, and streams reach the worker
/// over the new one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn redials_the_tunnel() {
    let (srv, url) = Server::start().await;
    let dir = scratch("redial");
    let w = Worker::new(config(&url, &dir), Fake::default(), Log::default(), None).unwrap();
    let run = tokio::spawn(w.run());
    for round in 1..=2 {
        let mut s = BufReader::new(within("opening", srv.open("processes", "e1")).await);
        let mut line = String::new();
        within("the greeting", s.read_line(&mut line))
            .await
            .unwrap();
        assert_eq!(line, "port 106\n", "round {round}");
        assert_eq!(srv.st.lock().unwrap().tunnels, round);
        srv.cut.notify_waiters();
        wait(|| srv.st.lock().unwrap().opener.is_none().then_some(())).await;
    }
    run.abort();
}

/// A credential the server rejects ends the worker: no amount of retrying
/// fixes a revoked worker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rejected_credential_ends_it() {
    let (srv, url) = Server::start().await;
    let dir = scratch("rejected");
    let w = Worker::new(config(&url, &dir), Fake::default(), Log::default(), None).unwrap();
    let run = tokio::spawn(w.run());
    wait(|| (!srv.st.lock().unwrap().statuses.is_empty()).then_some(())).await;
    srv.st.lock().unwrap().reject = true;
    let out = tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .expect("the worker stopped")
        .unwrap();
    assert_eq!(out, Err(client::Error::Rejected));

    // And so does a bootstrap token the server does not take.
    let dir = scratch("rejected-token");
    let cfg = config(&url, &dir);
    std::fs::write(dir.join("token"), "wrong\n").unwrap();
    srv.st.lock().unwrap().reject = false;
    let w = Worker::new(cfg, Fake::default(), Log::default(), None).unwrap();
    let out = tokio::time::timeout(Duration::from_secs(10), w.run())
        .await
        .expect("it stopped");
    assert_eq!(out, Err(client::Error::Rejected));
}
