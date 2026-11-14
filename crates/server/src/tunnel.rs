//! Holds the connection a worker opens to the server (GET
//! /api/worker/v1/tunnel, a WebSocket), on which the server is yamux's
//! client, as the Rust worker's crates/worker/src/tunnel.rs serves it.

use std::collections::HashMap;
use std::io;
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll, ready};

use axum::extract::ws::{Message, WebSocket};
use futures_util::{Sink, Stream};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

type Opener = mpsc::Sender<oneshot::Sender<Result<yamux::Stream, String>>>;

/// The tunnels workers have open to this server.
#[derive(Default)]
pub struct Tunnels(Mutex<HashMap<Uuid, (u64, Opener)>>);

impl Tunnels {
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

/// A WebSocket as a byte stream for yamux: one binary message per write.
struct WsIo {
    ws: WebSocket,
    read: bytes::Bytes,
}

impl WsIo {
    fn new(ws: WebSocket) -> WsIo {
        WsIo {
            ws,
            read: bytes::Bytes::new(),
        }
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
        while self.read.is_empty() {
            match ready!(Pin::new(&mut self.ws).poll_next(cx)) {
                Some(Ok(Message::Binary(b))) => self.read = b,
                Some(Ok(Message::Close(_))) | None => return Poll::Ready(Ok(0)),
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Poll::Ready(Err(broken(e))),
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
