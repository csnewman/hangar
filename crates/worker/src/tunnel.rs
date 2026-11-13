//! Streams from the control plane to the worker, over a connection the
//! worker opens.
//!
//! A worker dials out, never in: it may be behind NAT or in a pod with no
//! ingress, and the control plane has no route to it. So the worker opens
//! one WebSocket to the server and holds it; the server multiplexes streams
//! back over it with yamux, each one the server's to open. Everything
//! interactive about an environment, such as a terminal, travels this way,
//! as a stream per use.
//!
//! Each stream starts with one JSON line, a Header, saying what it is for.
//! What follows is that use's own protocol, which the tunnel does not read.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll, ready};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use hangar_vm::log::Log;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};
use tokio_util::compat::{Compat, FuturesAsyncReadCompatExt};

/// Kinds of stream.
pub const KIND_TERMINAL: &str = "terminal";
/// One VNC (RFB) connection to the environment's desktop.
pub const KIND_DESKTOP: &str = "desktop";
/// One connection to the environment's Code tab service.
pub const KIND_CODE: &str = "code";
/// The environment's profile session, which the server holds open while the
/// environment runs.
pub const KIND_PROFILE: &str = "profile";
/// Requests about the processes running in the environment.
pub const KIND_PROCESSES: &str = "processes";
/// One SSH connection from the gateway to the environment's SSH server.
pub const KIND_SSH: &str = "ssh";
/// One connection to the environment's port forwarder, which the control
/// plane asks for a port: one of the environment's own web servers.
pub const KIND_PORTS: &str = "ports";
/// One request for part of a log the worker holds, and its reply: the
/// worker answers it itself.
pub const KIND_LOGS: &str = "logs";

/// Opens a stream.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    pub kind: String,
    #[serde(default)]
    pub environment: String,
}

/// One stream the control plane opened, read from just after its header.
/// Its reads and writes must be polled from one task: a yamux stream's
/// reads send window updates down the same channel its writes use, and
/// that channel wakes only the task that last waited on it, so a writer in
/// a task of its own can wait forever with credit to spare.
pub type Stream = BufReader<Compat<yamux::Stream>>;

/// Where the server takes a worker's tunnel, beneath its URL.
pub const TUNNEL_PATH: &str = "/api/worker/v1/tunnel";

/// Long enough for the server's keepalive, every 20 s, to have missed two:
/// a tunnel that hears nothing for this long is dead.
const SILENT_FOR: Duration = Duration::from_secs(65);

/// The server refused the worker's tunnel: its credential is no good.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused;

/// Holds a tunnel open to the server at base, redialling whenever it drops,
/// until the server refuses the credential. Each stream is handed to handle
/// in a task of its own.
pub async fn serve<F, Fut>(base: &str, credential: &str, log: Log, handle: F) -> Refused
where
    F: Fn(Header, Stream) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let url = tunnel_url(base);
    let handle = Arc::new(handle);
    let mut backoff = Duration::from_secs(1);
    loop {
        let (connected, why) = match serve_once(&url, credential, handle.clone()).await {
            Ok(connected) => (connected, "the connection closed".to_string()),
            Err(Failure::Refused) => return Refused,
            Err(Failure::Dropped(connected, why)) => (connected, why),
        };
        // The backoff is for a server that cannot be reached; once one was,
        // a drop is a new outage, and the first retry comes soon.
        if connected {
            backoff = Duration::from_secs(1);
        }
        log.warn(
            "worker tunnel dropped, redialling",
            &[("err", &why), ("after", &format!("{backoff:?}"))],
        );
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

/// The tunnel's WebSocket URL, beneath a server's.
pub fn tunnel_url(base: &str) -> String {
    format!(
        "{}{TUNNEL_PATH}",
        base.trim_end_matches('/').replacen("http", "ws", 1)
    )
}

enum Failure {
    Refused,
    /// Whether the tunnel was made, and why it ended.
    Dropped(bool, String),
}

/// Holds one tunnel until it drops, and says whether it was made.
async fn serve_once<F, Fut>(url: &str, credential: &str, handle: Arc<F>) -> Result<bool, Failure>
where
    F: Fn(Header, Stream) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let ws = dial(url, credential).await?;
    let refused = Arc::new(AtomicBool::new(false));
    let io = WsIo::new(ws, refused.clone());
    let mut cfg = yamux::Config::default();
    cfg.set_max_num_streams(4096);
    let mut conn = yamux::Connection::new(io, cfg, yamux::Mode::Server);
    loop {
        let next = std::future::poll_fn(|cx| conn.poll_next_inbound(cx)).await;
        let stream = match next {
            Some(Ok(s)) => s,
            Some(Err(e)) => {
                if refused.load(Ordering::Relaxed) {
                    return Err(Failure::Refused);
                }
                return Err(Failure::Dropped(true, e.to_string()));
            }
            None => {
                if refused.load(Ordering::Relaxed) {
                    return Err(Failure::Refused);
                }
                return Ok(true);
            }
        };
        let handle = handle.clone();
        tokio::spawn(async move {
            let mut r = BufReader::new(stream.compat());
            let mut line = Vec::new();
            match r.read_until(b'\n', &mut line).await {
                Ok(n) if n > 0 && line.ends_with(b"\n") => {}
                _ => return,
            }
            let Ok(h) = serde_json::from_slice::<Header>(&line) else {
                return;
            };
            handle(h, r).await;
        });
    }
}

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn dial(url: &str, credential: &str) -> Result<Socket, Failure> {
    let dropped = |why: String| Failure::Dropped(false, why);
    let mut req = url
        .into_client_request()
        .map_err(|e| dropped(e.to_string()))?;
    req.headers_mut().insert(
        "Authorization",
        format!("Bearer {credential}")
            .parse()
            .map_err(|_| Failure::Refused)?,
    );
    let connector = if url.starts_with("wss:") {
        use rustls_platform_verifier::ConfigVerifierExt;
        let config =
            rustls::ClientConfig::with_platform_verifier().map_err(|e| dropped(e.to_string()))?;
        Connector::Rustls(Arc::new(config))
    } else {
        Connector::Plain
    };
    let config = WebSocketConfig::default()
        .max_message_size(None)
        .max_frame_size(None);
    match tokio_tungstenite::connect_async_tls_with_config(req, Some(config), true, Some(connector))
        .await
    {
        Ok((ws, _)) => Ok(ws),
        Err(tungstenite::Error::Http(r)) if r.status() == 401 => Err(Failure::Refused),
        Err(e) => Err(dropped(e.to_string())),
    }
}

/// A WebSocket as a stream of bytes: each message written is one binary
/// message, and the messages read are read through in turn.
struct WsIo {
    ws: Socket,
    rbuf: tungstenite::Bytes,
    wbuf: Vec<u8>,
    /// Set when the server closes the socket as a policy violation, which
    /// is how it refuses a worker it has revoked.
    refused: Arc<AtomicBool>,
    silence: Pin<Box<tokio::time::Sleep>>,
}

/// How much is written before it is sent, whether or not it has been
/// flushed: yamux writes each frame's header and body apart, and they go
/// best as one message.
const SEND_AT: usize = 256 << 10;

impl WsIo {
    fn new(ws: Socket, refused: Arc<AtomicBool>) -> WsIo {
        WsIo {
            ws,
            rbuf: tungstenite::Bytes::new(),
            wbuf: Vec::new(),
            refused,
            silence: Box::pin(tokio::time::sleep(SILENT_FOR)),
        }
    }

    fn heard(&mut self) {
        self.silence
            .as_mut()
            .reset(tokio::time::Instant::now() + SILENT_FOR);
    }

    fn poll_send(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.wbuf.is_empty() {
            return Poll::Ready(Ok(()));
        }
        ready!(self.ws.poll_ready_unpin(cx)).map_err(io::Error::other)?;
        let msg = std::mem::take(&mut self.wbuf);
        self.ws
            .start_send_unpin(Message::Binary(msg.into()))
            .map_err(io::Error::other)?;
        Poll::Ready(Ok(()))
    }
}

impl futures_util::io::AsyncRead for WsIo {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        loop {
            if !this.rbuf.is_empty() {
                let n = out.len().min(this.rbuf.len());
                out[..n].copy_from_slice(&this.rbuf.split_to(n));
                return Poll::Ready(Ok(n));
            }
            match this.ws.poll_next_unpin(cx) {
                Poll::Ready(Some(Ok(Message::Binary(b)))) => {
                    this.heard();
                    this.rbuf = b;
                }
                Poll::Ready(Some(Ok(Message::Close(frame)))) => {
                    if frame.is_some_and(|f| f.code == CloseCode::Policy) {
                        this.refused.store(true, Ordering::Relaxed);
                    }
                    return Poll::Ready(Ok(0));
                }
                Poll::Ready(Some(Ok(Message::Text(_)))) => {
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "a text message on the tunnel",
                    )));
                }
                Poll::Ready(Some(Ok(_))) => this.heard(),
                Poll::Ready(Some(Err(e))) => return Poll::Ready(Err(io::Error::other(e))),
                Poll::Ready(None) => return Poll::Ready(Ok(0)),
                Poll::Pending => {
                    if this.silence.as_mut().poll(cx).is_ready() {
                        return Poll::Ready(Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "the server has said nothing for over a minute",
                        )));
                    }
                    return Poll::Pending;
                }
            }
        }
    }
}

impl futures_util::io::AsyncWrite for WsIo {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.wbuf.len() >= SEND_AT {
            ready!(this.poll_send(cx))?;
        }
        this.wbuf.extend_from_slice(data);
        Poll::Ready(Ok(data.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        ready!(this.poll_send(cx))?;
        this.ws.poll_flush_unpin(cx).map_err(io::Error::other)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        ready!(this.poll_send(cx))?;
        this.ws.poll_close_unpin(cx).map_err(io::Error::other)
    }
}
