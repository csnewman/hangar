//! The environment's Docker: its containers and images, what can be done to
//! them, their logs, and word of their changes, through the Engine API on its
//! socket.

use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use super::child;
use super::error::{Error, Result};
use super::files::collate;

/// The Engine API version spoken; any engine since Docker 20.10 has it.
const API: &str = "/v1.41";
const TIMEOUT: Duration = Duration::from_secs(30);

/// The Engine's socket: DOCKER_HOST's, when it names a unix socket, else
/// the usual one.
fn socket_path() -> String {
    std::env::var("DOCKER_HOST")
        .ok()
        .and_then(|h| h.strip_prefix("unix://").map(str::to_owned))
        .unwrap_or_else(|| "/var/run/docker.sock".into())
}

/// A response from the Engine, its body read as it arrives.
struct Response {
    status: u16,
    chunked: bool,
    left: Option<usize>,
    done: bool,
    r: BufReader<UnixStream>,
}

impl Response {
    /// The next part of the body, None at its end.
    async fn next(&mut self) -> io::Result<Option<Vec<u8>>> {
        if self.done {
            return Ok(None);
        }
        if self.chunked {
            let mut line = String::new();
            loop {
                line.clear();
                if self.r.read_line(&mut line).await? == 0 {
                    self.done = true;
                    return Ok(None);
                }
                if !line.trim().is_empty() {
                    break;
                }
            }
            let size = usize::from_str_radix(line.trim().split(';').next().unwrap_or("0"), 16)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "a bad chunk"))?;
            if size == 0 {
                self.done = true;
                return Ok(None);
            }
            let mut buf = vec![0u8; size];
            self.r.read_exact(&mut buf).await?;
            return Ok(Some(buf));
        }
        let want = self.left.unwrap_or(64 << 10).min(64 << 10);
        if want == 0 {
            self.done = true;
            return Ok(None);
        }
        let mut buf = vec![0u8; want];
        let n = self.r.read(&mut buf).await?;
        if n == 0 {
            self.done = true;
            return Ok(None);
        }
        buf.truncate(n);
        if let Some(l) = &mut self.left {
            *l -= n;
        }
        Ok(Some(buf))
    }

    async fn body(mut self) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        while let Some(c) = self.next().await? {
            out.extend(c);
        }
        Ok(out)
    }
}

async fn request(method: &str, path: &str) -> io::Result<Response> {
    let stream = UnixStream::connect(socket_path()).await?;
    let mut r = BufReader::new(stream);
    let req = format!(
        "{method} {API}{path} HTTP/1.1\r\nHost: docker\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
    );
    r.get_mut().write_all(req.as_bytes()).await?;
    let mut line = String::new();
    r.read_line(&mut line).await?;
    let status: u16 = line
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Docker did not answer"))?;
    let (mut chunked, mut left) = (false, None);
    loop {
        line.clear();
        if r.read_line(&mut line).await? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
            if k == "transfer-encoding" && v.eq_ignore_ascii_case("chunked") {
                chunked = true;
            } else if k == "content-length" {
                left = v.parse().ok();
            }
        }
    }
    Ok(Response {
        status,
        chunked,
        left,
        done: false,
        r,
    })
}

/// A whole response, its status and body.
async fn call(method: &str, path: &str) -> io::Result<(u16, Vec<u8>)> {
    let fut = async {
        let res = request(method, path).await?;
        let status = res.status;
        Ok((status, res.body().await?))
    };
    tokio::time::timeout(TIMEOUT, fut)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Docker did not answer"))?
}

async fn json(method: &str, path: &str) -> std::result::Result<Value, Failure> {
    let (status, body) = call(method, path).await.map_err(Failure::Io)?;
    if status >= 300 {
        return Err(Failure::Engine(engine_error(status, &body)));
    }
    serde_json::from_slice(&body).map_err(|e| Failure::Engine(e.to_string()))
}

enum Failure {
    Io(io::Error),
    Engine(String),
}

impl Failure {
    /// What stops the service reaching Docker, in words for a person.
    fn why(&self) -> String {
        match self {
            Failure::Io(e) => why(e),
            Failure::Engine(m) => m.clone(),
        }
    }
}

fn why(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => {
            "Docker is not running in this environment".into()
        }
        io::ErrorKind::PermissionDenied => {
            "this user may not use Docker (not in the docker group)".into()
        }
        _ => e.to_string(),
    }
}

fn engine_error(status: u16, body: &[u8]) -> String {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_else(|| format!("Docker answered {status}"))
}

#[derive(Serialize)]
pub struct Container {
    id: String,
    name: String,
    image: String,
    /// running, exited, paused, created, restarting, removing or dead.
    state: String,
    /// Docker's own description: "Up 3 minutes", "Exited (0) 2 hours ago".
    status: String,
    ports: Vec<String>,
    created: f64,
    /// The command it runs.
    command: String,
    /// The Compose project and service it belongs to, if any, and where the
    /// project's files are.
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    service: Option<String>,
    #[serde(rename = "workingDir", skip_serializing_if = "Option::is_none")]
    working_dir: Option<String>,
    #[serde(rename = "configFiles", skip_serializing_if = "Option::is_none")]
    config_files: Option<Vec<String>>,
}

#[derive(Serialize)]
pub struct Image {
    id: String,
    tags: Vec<String>,
    size: f64,
    created: f64,
    containers: f64,
}

#[derive(Serialize)]
pub struct Overview {
    available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    why: Option<String>,
    containers: Vec<Container>,
    images: Vec<Image>,
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_owned()
}

fn n(v: &Value, k: &str) -> f64 {
    v.get(k).and_then(Value::as_f64).unwrap_or(0.0)
}

fn container(c: &Value) -> Container {
    let id = s(c, "Id");
    let name = c
        .get("Names")
        .and_then(|n| n.get(0))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| id.chars().take(12).collect());
    let label = |k: &str| {
        c.get("Labels")
            .and_then(|l| l.get(k))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let mut ports: Vec<String> = Vec::new();
    for p in c
        .get("Ports")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        // IPv4 and IPv6 bindings of one port are one port to a person.
        let private = n(p, "PrivatePort");
        let kind = s(p, "Type");
        let port = match p
            .get("PublicPort")
            .and_then(Value::as_f64)
            .filter(|&x| x != 0.0)
        {
            Some(public) => format!("{public}→{private}/{kind}"),
            None => format!("{private}/{kind}"),
        };
        if !ports.contains(&port) {
            ports.push(port);
        }
    }
    Container {
        name: name.strip_prefix('/').unwrap_or(&name).to_owned(),
        id,
        image: s(c, "Image"),
        state: s(c, "State"),
        status: s(c, "Status"),
        ports,
        created: n(c, "Created"),
        command: s(c, "Command"),
        project: label("com.docker.compose.project"),
        service: label("com.docker.compose.service"),
        working_dir: label("com.docker.compose.project.working_dir"),
        config_files: label("com.docker.compose.project.config_files").map(|f| {
            f.split(',')
                .map(str::trim)
                .filter(|f| !f.is_empty())
                .map(str::to_owned)
                .collect()
        }),
    }
}

fn is_container_id(id: &str) -> bool {
    (12..=64).contains(&id.len()) && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

pub enum Action {
    Start,
    Stop,
    Restart,
    Pause,
    Unpause,
    Remove,
}

impl Action {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "start" => Action::Start,
            "stop" => Action::Stop,
            "restart" => Action::Restart,
            "pause" => Action::Pause,
            "unpause" => Action::Unpause,
            "remove" => Action::Remove,
            _ => return None,
        })
    }

    fn word(&self) -> &'static str {
        match self {
            Action::Start => "start",
            Action::Stop => "stop",
            Action::Restart => "restart",
            Action::Pause => "pause",
            Action::Unpause => "unpause",
            Action::Remove => "remove",
        }
    }
}

pub struct Docker {
    changed: broadcast::Sender<()>,
    watch: Mutex<DockerWatch>,
    pulse: Arc<AtomicU64>,
}

#[derive(Default)]
struct DockerWatch {
    listeners: usize,
    task: Option<JoinHandle<()>>,
}

/// Held by each listener; Docker is watched while any is.
pub struct DockerListening {
    docker: Arc<Docker>,
}

impl Drop for DockerListening {
    fn drop(&mut self) {
        let mut w = self.docker.watch.lock().unwrap();
        w.listeners -= 1;
        if w.listeners == 0
            && let Some(t) = w.task.take()
        {
            t.abort();
        }
    }
}

impl Docker {
    pub fn new() -> Arc<Self> {
        Arc::new(Docker {
            changed: broadcast::channel(16).0,
            watch: Mutex::new(DockerWatch::default()),
            pulse: Arc::new(AtomicU64::new(0)),
        })
    }

    pub async fn overview(&self) -> Overview {
        let (cs, is) = tokio::join!(
            json("GET", "/containers/json?all=1"),
            json("GET", "/images/json")
        );
        let (cs, is) = match (cs, is) {
            (Ok(cs), Ok(is)) => (cs, is),
            (Err(e), _) | (_, Err(e)) => {
                return Overview {
                    available: false,
                    why: Some(e.why()),
                    containers: Vec::new(),
                    images: Vec::new(),
                };
            }
        };
        let mut containers: Vec<Container> =
            cs.as_array().into_iter().flatten().map(container).collect();
        containers.sort_by(|a, b| {
            collate(
                a.project.as_deref().unwrap_or(""),
                b.project.as_deref().unwrap_or(""),
            )
            .then_with(|| collate(&a.name, &b.name))
        });
        let mut images: Vec<Image> = is
            .as_array()
            .into_iter()
            .flatten()
            .map(|i| Image {
                id: s(i, "Id"),
                tags: i
                    .get("RepoTags")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .filter(|t| *t != "<none>:<none>")
                    .map(str::to_owned)
                    .collect(),
                size: n(i, "Size"),
                created: n(i, "Created"),
                containers: n(i, "Containers"),
            })
            .collect();
        images.sort_by(|a, b| b.created.total_cmp(&a.created));
        Overview {
            available: true,
            why: None,
            containers,
            images,
        }
    }

    pub async fn act(&self, id: &str, action: &Action) -> Result<()> {
        if !is_container_id(id) {
            return Err(Error::msg("not a container"));
        }
        let (status, body) = match action {
            Action::Remove => call("DELETE", &format!("/containers/{id}?force=1")).await,
            a => call("POST", &format!("/containers/{id}/{}", a.word())).await,
        }
        .map_err(|e| Error::msg(why(&e)))?;
        // 304: already as asked.
        if status >= 300 && status != 304 {
            return Err(Error::msg(engine_error(status, &body)));
        }
        Ok(())
    }

    /// Does to a Compose project what its containers can be done to, each in
    /// turn, or takes it down as Compose does: its containers and networks
    /// removed.
    pub async fn project(&self, name: &str, action: &str) -> Result<()> {
        let ok = name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
        if !ok {
            return Err(Error::msg("not a Compose project"));
        }
        if action == "down" {
            let out = child::run(
                child::command("docker", &["compose", "-p", name, "down"]),
                None,
                Some(Duration::from_secs(120)),
            )
            .await
            .map_err(|e| Error::msg(e.to_string()))?;
            if out.status != Some(0) {
                let err = String::from_utf8_lossy(&out.stderr);
                let last = err.trim().lines().last().unwrap_or("").to_owned();
                return Err(Error::msg(if last.is_empty() {
                    "docker compose down failed".to_owned()
                } else {
                    last
                }));
            }
            return Ok(());
        }
        let Some(a) = Action::parse(action) else {
            return Err(Error::msg(format!("no such action: {action}")));
        };
        let filters = encode(&format!(
            r#"{{"label":["com.docker.compose.project={name}"]}}"#
        ));
        let cs = json("GET", &format!("/containers/json?all=1&filters={filters}"))
            .await
            .map_err(|e| Error::msg(e.why()))?;
        let mut errors = Vec::new();
        for c in cs.as_array().into_iter().flatten() {
            if let Err(e) = self.act(&s(c, "Id"), &a).await {
                let name = c
                    .get("Names")
                    .and_then(|n| n.get(0))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| s(c, "Id"));
                errors.push(format!("{}: {e}", name.strip_prefix('/').unwrap_or(&name)));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(Error::msg(errors.join("; ")))
        }
    }

    pub async fn remove_image(&self, id: &str) -> Result<()> {
        let ok = id.strip_prefix("sha256:").is_some_and(|h| {
            h.len() == 64 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        });
        if !ok {
            return Err(Error::msg("not an image"));
        }
        let (status, body) = call("DELETE", &format!("/images/{id}"))
            .await
            .map_err(|e| Error::msg(why(&e)))?;
        if status >= 300 {
            return Err(Error::msg(engine_error(status, &body)));
        }
        Ok(())
    }

    /// Follows a container's output: the last lines of it, then what it
    /// writes, each piece given to `text`, and why it ended to `end`, until
    /// the task is aborted.
    pub fn logs(
        &self,
        id: String,
        tail: f64,
        text: impl Fn(String) + Send + 'static,
        end: impl FnOnce(String) + Send + 'static,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            if !is_container_id(&id) {
                return end("not a container".into());
            }
            let tty = match json("GET", &format!("/containers/{id}/json")).await {
                Ok(v) => v["Config"]["Tty"].as_bool().unwrap_or(false),
                Err(e) => return end(e.why()),
            };
            let tail = tail.clamp(0.0, 10000.0) as u64;
            let path = format!(
                "/containers/{id}/logs?stdout=1&stderr=1&follow=1&timestamps=0&tail={tail}"
            );
            let mut res = match request("GET", &path).await {
                Ok(r) => r,
                Err(e) => return end(why(&e)),
            };
            if res.status >= 300 {
                return end(format!("Docker answered {}", res.status));
            }
            // Without a TTY, the stream is framed: a byte saying which stream,
            // three empty, the length, then that much output.
            let mut buf: Vec<u8> = Vec::new();
            loop {
                match res.next().await {
                    Ok(Some(c)) => {
                        if tty {
                            text(String::from_utf8_lossy(&c).into_owned());
                            continue;
                        }
                        buf.extend(c);
                        let mut out = Vec::new();
                        while buf.len() >= 8 {
                            let len = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
                            if buf.len() < 8 + len {
                                break;
                            }
                            out.extend_from_slice(&buf[8..8 + len]);
                            buf.drain(..8 + len);
                        }
                        if !out.is_empty() {
                            text(String::from_utf8_lossy(&out).into_owned());
                        }
                    }
                    Ok(None) => return end("the container stopped".into()),
                    Err(e) => return end(why(&e)),
                }
            }
        })
    }

    /// Called a moment after Docker reports anything happening to a
    /// container or image. Docker is watched while anyone listens.
    pub fn listen(self: &Arc<Self>) -> (broadcast::Receiver<()>, DockerListening) {
        let rx = self.changed.subscribe();
        let mut w = self.watch.lock().unwrap();
        w.listeners += 1;
        if w.task.is_none() {
            let me = self.clone();
            w.task = Some(tokio::spawn(async move { me.watch().await }));
        }
        drop(w);
        (
            rx,
            DockerListening {
                docker: self.clone(),
            },
        )
    }

    /// Follows Docker's events, watching anew after Docker goes away, as it
    /// does on a restart.
    async fn watch(&self) {
        let filters = encode(r#"{"type":["container","image"]}"#);
        loop {
            if let Ok(mut res) = request("GET", &format!("/events?filters={filters}")).await {
                while let Ok(Some(_)) = res.next().await {
                    let n = self.pulse.fetch_add(1, Ordering::SeqCst) + 1;
                    let (changed, pulse) = (self.changed.clone(), self.pulse.clone());
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        if pulse.load(Ordering::SeqCst) == n {
                            let _ = changed.send(());
                        }
                    });
                }
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }
}

/// encodeURIComponent, for a query parameter.
fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
