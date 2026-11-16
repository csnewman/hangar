//! Proxmox VE's API, over HTTPS with an API token.
//!
//! A request is a connection of its own: Proxmox's API is asked a few times
//! a second at most, and a connection that is never reused is never found
//! closed under a request. The node's certificate is taken by its SHA-256
//! fingerprint, as a node's own certificate is signed by its cluster's
//! authority alone, or, with none given, as the system's roots say.

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::Frame;
use hyper_util::rt::TokioIo;
use serde_json::Value;
use tokio::net::TcpStream;

use crate::err;

/// How long one request may take, an upload aside.
const REQUEST_WITHIN: Duration = Duration::from_secs(60);

/// One Proxmox VE API endpoint, and the token it is asked with.
#[derive(Clone)]
pub struct Client {
    host: String,
    port: u16,
    auth: String,
    tls: tokio_rustls::TlsConnector,
}

impl Client {
    /// A client of the API at url (https://host:8006), with token
    /// (USER@REALM!ID=SECRET), taking the certificate with fingerprint, or
    /// with none, one the system's roots vouch for.
    pub fn new(url: &str, token: &str, fingerprint: Option<&str>) -> io::Result<Client> {
        let rest = url
            .trim_end_matches('/')
            .strip_prefix("https://")
            .ok_or_else(|| err(format!("proxmox url {url:?}: want https://host:port")))?;
        let (host, port) = match rest.rsplit_once(':') {
            Some((h, p)) if !h.ends_with(']') || h.starts_with('[') => (
                h.trim_matches(['[', ']']).to_string(),
                p.parse()
                    .map_err(|_| err(format!("proxmox url {url:?}: a bad port")))?,
            ),
            _ => (rest.trim_matches(['[', ']']).to_string(), 8006),
        };
        let token = token.trim();
        if !token.contains('!') || !token.contains('=') {
            return Err(err(
                "a Proxmox API token is USER@REALM!ID=SECRET, as its page shows it",
            ));
        }
        let config = match fingerprint.filter(|f| !f.trim().is_empty()) {
            Some(f) => {
                hangar_hub::tls::pinned(f, &[&rustls::version::TLS13, &rustls::version::TLS12])?
            }
            None => {
                use rustls_platform_verifier::BuilderVerifierExt;
                rustls::ClientConfig::builder_with_provider(Arc::new(
                    rustls::crypto::ring::default_provider(),
                ))
                .with_safe_default_protocol_versions()
                .map_err(io::Error::other)?
                .with_platform_verifier()
                .map_err(io::Error::other)?
                .with_no_client_auth()
            }
        };
        Ok(Client {
            host,
            port,
            auth: format!("PVEAPIToken={token}"),
            tls: tokio_rustls::TlsConnector::from(Arc::new(config)),
        })
    }

    pub async fn get(&self, path: &str) -> io::Result<Value> {
        self.call("GET", path, &[]).await
    }

    pub async fn post(&self, path: &str, form: &[(&str, &str)]) -> io::Result<Value> {
        self.call("POST", path, form).await
    }

    pub async fn put(&self, path: &str, form: &[(&str, &str)]) -> io::Result<Value> {
        self.call("PUT", path, form).await
    }

    pub async fn delete(&self, path: &str, form: &[(&str, &str)]) -> io::Result<Value> {
        self.call("DELETE", path, form).await
    }

    /// One request, its parameters as a form (GET and DELETE in the
    /// query), and the `data` of its answer.
    async fn call(&self, method: &str, path: &str, form: &[(&str, &str)]) -> io::Result<Value> {
        let encoded = encode_form(form);
        let in_query = matches!(method, "GET" | "DELETE");
        let uri = if in_query && !encoded.is_empty() {
            format!("/api2/json{path}?{encoded}")
        } else {
            format!("/api2/json{path}")
        };
        let mut req = hyper::Request::builder()
            .method(method)
            .uri(&uri)
            .header("Host", self.host_header())
            .header("Authorization", &self.auth);
        let body = if in_query {
            Bytes::new()
        } else {
            req = req.header("Content-Type", "application/x-www-form-urlencoded");
            Bytes::from(encoded)
        };
        let req = req.body(Full::new(body)).map_err(io::Error::other)?;
        tokio::time::timeout(REQUEST_WITHIN, self.send(req, method, path))
            .await
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("proxmox {method} {path}: no answer"),
                )
            })?
    }

    fn host_header(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    async fn connect(&self) -> io::Result<tokio_rustls::client::TlsStream<TcpStream>> {
        let tcp = TcpStream::connect((self.host.as_str(), self.port)).await?;
        tcp.set_nodelay(true)?;
        let name =
            rustls::pki_types::ServerName::try_from(self.host.clone()).map_err(io::Error::other)?;
        self.tls.connect(name, tcp).await
    }

    async fn send<B>(&self, req: hyper::Request<B>, method: &str, path: &str) -> io::Result<Value>
    where
        B: hyper::body::Body + Send + 'static,
        B::Data: Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let tls = self.connect().await?;
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
            .await
            .map_err(io::Error::other)?;
        tokio::spawn(conn);
        let res = sender.send_request(req).await.map_err(io::Error::other)?;
        let status = res.status();
        let body = res
            .into_body()
            .collect()
            .await
            .map_err(io::Error::other)?
            .to_bytes();
        let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        if !status.is_success() {
            let why = v["message"]
                .as_str()
                .map(str::trim)
                .filter(|m| !m.is_empty())
                .map(str::to_string)
                .or_else(|| status.canonical_reason().map(str::to_string))
                .unwrap_or_default();
            let errors = match &v["errors"] {
                Value::Object(e) if !e.is_empty() => format!(" {}", Value::Object(e.clone())),
                _ => String::new(),
            };
            let kind = match status.as_u16() {
                401 | 403 => io::ErrorKind::PermissionDenied,
                404 => io::ErrorKind::NotFound,
                _ => io::ErrorKind::Other,
            };
            return Err(io::Error::new(
                kind,
                format!("proxmox {method} {path}: {} {why}{errors}", status.as_u16()),
            ));
        }
        Ok(v["data"].clone())
    }

    /// Opens a websocket to a path of the API (`/nodes/...`, its query
    /// included), signed in with the token.
    pub async fn websocket(
        &self,
        path: &str,
    ) -> io::Result<tokio_tungstenite::WebSocketStream<tokio_rustls::client::TlsStream<TcpStream>>>
    {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let url = format!("wss://{}/api2/json{path}", self.host_header());
        let mut req = url.into_client_request().map_err(io::Error::other)?;
        let h = req.headers_mut();
        h.insert(
            "Authorization",
            self.auth.parse().map_err(io::Error::other)?,
        );
        h.insert(
            "Sec-WebSocket-Protocol",
            hyper::header::HeaderValue::from_static("binary"),
        );
        let tls = tokio::time::timeout(REQUEST_WITHIN, self.connect())
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "connecting to proxmox"))??;
        let (ws, _) =
            tokio::time::timeout(REQUEST_WITHIN, tokio_tungstenite::client_async(req, tls))
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "proxmox's websocket"))?
                .map_err(io::Error::other)?;
        Ok(ws)
    }

    /// Waits for a task (its UPID, as an asynchronous call answers) to end,
    /// and says how it failed if it did.
    pub async fn wait(&self, node: &str, upid: &str, within: Duration) -> io::Result<()> {
        let path = format!("/nodes/{}/tasks/{}/status", enc(node), enc(upid));
        let deadline = tokio::time::Instant::now() + within;
        loop {
            let st = self.get(&path).await?;
            if st["status"] == "stopped" {
                let exit = st["exitstatus"].as_str().unwrap_or_default();
                if exit == "OK" || exit.starts_with("WARNINGS") {
                    return Ok(());
                }
                let log = self
                    .get(&format!(
                        "/nodes/{}/tasks/{}/log?limit=50",
                        enc(node),
                        enc(upid)
                    ))
                    .await
                    .unwrap_or_default();
                let tail: Vec<&str> = log
                    .as_array()
                    .map(|l| l.iter().filter_map(|x| x["t"].as_str()).collect())
                    .unwrap_or_default();
                return Err(err(format!(
                    "proxmox task {}: {exit}: {}",
                    st["type"].as_str().unwrap_or_default(),
                    tail.iter()
                        .rev()
                        .take(3)
                        .rev()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" / ")
                )));
            }
            if tokio::time::Instant::now() > deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("proxmox task {upid} is still running"),
                ));
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    /// Uploads a file into a storage's content of a kind (import, iso,
    /// vztmpl) as name, and gives the task copying it into place.
    pub async fn upload(
        &self,
        node: &str,
        storage: &str,
        content: &str,
        file: &Path,
        name: &str,
    ) -> io::Result<String> {
        let boundary = format!("hangar-{:016x}", rand_u64());
        let head = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"content\"\r\n\r\n{content}\r\n\
             --{boundary}\r\nContent-Disposition: form-data; name=\"filename\"; filename=\"{name}\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n"
        );
        let tail = format!("\r\n--{boundary}--\r\n");
        let f = tokio::fs::File::open(file).await?;
        let size = f.metadata().await?.len();
        let length = head.len() as u64 + size + tail.len() as u64;
        use futures_util::StreamExt;
        let parts = futures_util::stream::iter([Ok::<_, io::Error>(Bytes::from(head))])
            .chain(tokio_util::io::ReaderStream::with_capacity(f, 1 << 20))
            .chain(futures_util::stream::iter([Ok(Bytes::from(tail))]))
            .map(|b| b.map(Frame::data));
        let path = format!("/nodes/{}/storage/{}/upload", enc(node), enc(storage));
        let req = hyper::Request::builder()
            .method("POST")
            .uri(format!("/api2/json{path}"))
            .header("Host", self.host_header())
            .header("Authorization", &self.auth)
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .header("Content-Length", length)
            .body(StreamBody::new(parts))
            .map_err(io::Error::other)?;
        let upid = self.send(req, "POST", &path).await?;
        Ok(upid.as_str().unwrap_or_default().to_string())
    }
}

/// A path segment or form value, percent-encoded but for what needs none.
pub fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn encode_form(form: &[(&str, &str)]) -> String {
    form.iter()
        .map(|(k, v)| format!("{}={}", enc(k), enc(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn rand_u64() -> u64 {
    let mut b = [0u8; 8];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut b)
        .expect("the system's random numbers");
    u64::from_le_bytes(b)
}
