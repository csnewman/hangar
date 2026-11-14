//! Carries streams from the server to a worker over a connection the worker
//! opens (GET /api/worker/v1/tunnel, a WebSocket): the server is yamux's
//! client on it and opens a stream for each use, which starts with a JSON
//! line naming what it is for (internal/tunnel's Header), as the Rust
//! worker's crates/worker/src/tunnel.rs serves it.
//!
//! A stream's reads and writes are polled from one task: a yamux stream's
//! reads send window updates down the same channel its writes use, which
//! wakes only the task that last waited on it.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll, ready};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use futures_util::{Sink, Stream};
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot};
use tokio_util::compat::{Compat, FuturesAsyncReadCompatExt};
use uuid::Uuid;

/// A stream to a worker, its header written.
pub type Conn = Compat<yamux::Stream>;

/// The kinds of stream the server opens.
pub const KIND_TERMINAL: &str = "terminal";
pub const KIND_DESKTOP: &str = "desktop";
pub const KIND_CODE: &str = "code";
pub const KIND_PROCESSES: &str = "processes";
pub const KIND_PORTS: &str = "ports";
pub const KIND_LOGS: &str = "logs";
pub const KIND_SSH: &str = "ssh";

/// How often the server pings a tunnel: often enough to keep it through a
/// proxy's idle timeout, and for the worker, which drops one it has heard
/// nothing on for 65 seconds, to know the server is there.
const PING: Duration = Duration::from_secs(20);

/// A tunnel the worker has said nothing on for this long, pongs included,
/// is dead.
const SILENT_FOR: Duration = Duration::from_secs(65);

/// What a stream is for: one of the worker's kinds, and the environment.
#[derive(serde::Serialize)]
pub struct Header<'a> {
    pub kind: &'a str,
    pub environment: &'a str,
}

/// Why a stream could not be opened.
#[derive(Debug)]
pub enum OpenError {
    /// The worker has no tunnel to this server.
    NoTunnel,
    /// It had one, which failed.
    Failed(String),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::NoTunnel => f.write_str("the worker is not connected to this server"),
            OpenError::Failed(e) => write!(f, "the worker's tunnel: {e}"),
        }
    }
}

type Opener = mpsc::Sender<oneshot::Sender<Result<yamux::Stream, String>>>;

/// The tunnels workers have open to this server.
#[derive(Default)]
pub struct Tunnels(Mutex<HashMap<Uuid, (u64, Opener)>>);

impl Tunnels {
    /// Opens a stream to the worker with id, header written.
    pub async fn open(&self, worker: Uuid, header: &Header<'_>) -> Result<Conn, OpenError> {
        let opener = self
            .0
            .lock()
            .unwrap()
            .get(&worker)
            .map(|(_, o)| o.clone())
            .ok_or(OpenError::NoTunnel)?;
        let (tx, rx) = oneshot::channel();
        opener.send(tx).await.map_err(|_| OpenError::NoTunnel)?;
        let stream = rx
            .await
            .map_err(|_| OpenError::NoTunnel)?
            .map_err(OpenError::Failed)?;
        let mut conn = stream.compat();
        let mut line = serde_json::to_vec(header).map_err(|e| OpenError::Failed(e.to_string()))?;
        line.push(b'\n');
        conn.write_all(&line)
            .await
            .map_err(|e| OpenError::Failed(e.to_string()))?;
        Ok(conn)
    }

    /// Holds a worker's tunnel until it drops, opening streams on it as
    /// they are asked for. A newer tunnel from the same worker takes its
    /// place.
    pub async fn serve(&self, worker: Uuid, ws: WebSocket) {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let generation = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (tx, mut opens) = mpsc::channel(16);
        self.0.lock().unwrap().insert(worker, (generation, tx));
        let mut conn =
            yamux::Connection::new(WsIo::new(ws), yamux::Config::default(), yamux::Mode::Client);
        let mut pending: Option<oneshot::Sender<Result<yamux::Stream, String>>> = None;
        std::future::poll_fn(|cx| {
            loop {
                if pending.is_none()
                    && let Poll::Ready(Some(reply)) = opens.poll_recv(cx)
                {
                    pending = Some(reply);
                }
                if pending.is_some()
                    && let Poll::Ready(r) = conn.poll_new_outbound(cx)
                {
                    let _ = pending.take().unwrap().send(r.map_err(|e| e.to_string()));
                    continue;
                }
                match conn.poll_next_inbound(cx) {
                    // The worker opens none: one it did is closed at once.
                    Poll::Ready(Some(Ok(_))) => continue,
                    Poll::Ready(_) => return Poll::Ready(()),
                    Poll::Pending => return Poll::Pending,
                }
            }
        })
        .await;
        let mut held = self.0.lock().unwrap();
        if held.get(&worker).is_some_and(|(g, _)| *g == generation) {
            held.remove(&worker);
        }
    }
}

/// A WebSocket as a byte stream for yamux: one binary message per write,
/// with a ping every PING, and an end when the worker is silent for
/// SILENT_FOR.
struct WsIo {
    ws: WebSocket,
    read: bytes::Bytes,
    ping: Pin<Box<tokio::time::Sleep>>,
    silence: Pin<Box<tokio::time::Sleep>>,
}

impl WsIo {
    fn new(ws: WebSocket) -> WsIo {
        WsIo {
            ws,
            read: bytes::Bytes::new(),
            ping: Box::pin(tokio::time::sleep(PING)),
            silence: Box::pin(tokio::time::sleep(SILENT_FOR)),
        }
    }

    fn heard(&mut self) {
        self.silence
            .as_mut()
            .reset(tokio::time::Instant::now() + SILENT_FOR);
    }

    /// Sends a ping when one is due and the socket can take it.
    fn poll_ping(&mut self, cx: &mut Context<'_>) {
        if self.ping.as_mut().poll(cx).is_pending() {
            return;
        }
        if let Poll::Ready(Ok(())) = Pin::new(&mut self.ws).poll_ready(cx)
            && Pin::new(&mut self.ws)
                .start_send(Message::Ping(bytes::Bytes::new()))
                .is_ok()
        {
            let _ = Pin::new(&mut self.ws).poll_flush(cx);
        }
        self.ping.as_mut().reset(tokio::time::Instant::now() + PING);
        let _ = self.ping.as_mut().poll(cx);
    }
}

fn broken(e: axum::Error) -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, e)
}

impl futures_util::AsyncRead for WsIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        self.poll_ping(cx);
        while self.read.is_empty() {
            match Pin::new(&mut self.ws).poll_next(cx) {
                Poll::Ready(Some(Ok(Message::Binary(b)))) => {
                    self.heard();
                    self.read = b;
                }
                Poll::Ready(Some(Ok(Message::Close(_)))) | Poll::Ready(None) => {
                    return Poll::Ready(Ok(0));
                }
                Poll::Ready(Some(Ok(_))) => self.heard(),
                Poll::Ready(Some(Err(e))) => return Poll::Ready(Err(broken(e))),
                Poll::Pending => {
                    if self.silence.as_mut().poll(cx).is_ready() {
                        return Poll::Ready(Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "the worker has said nothing for over a minute",
                        )));
                    }
                    return Poll::Pending;
                }
            }
        }
        let n = buf.len().min(self.read.len());
        buf[..n].copy_from_slice(&self.read[..n]);
        self.read = self.read.slice(n..);
        Poll::Ready(Ok(n))
    }
}

impl futures_util::AsyncWrite for WsIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        ready!(Pin::new(&mut self.ws).poll_ready(cx)).map_err(broken)?;
        Pin::new(&mut self.ws)
            .start_send(Message::Binary(bytes::Bytes::copy_from_slice(buf)))
            .map_err(broken)?;
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.ws).poll_flush(cx).map_err(broken)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.ws).poll_close(cx).map_err(broken)
    }
}
