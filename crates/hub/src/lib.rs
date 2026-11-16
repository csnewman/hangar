//! The hub: an environment's agent and its worker joined over the network,
//! where no vsock joins them, as on a Proxmox VM, whose host gives a guest
//! no vsock.
//!
//! The agent dials its worker: one TCP connection, TLS 1.3 to the worker's
//! own certificate, which the agent knows by its SHA-256 fingerprint and
//! takes no other in its place. Its first line, a [`Hello`], names the
//! environment and the secret the worker gave it for this boot; the worker
//! answers with a [`Welcome`], refusing an environment it does not hold or
//! a wrong secret. From there the connection is yamux, and either end opens
//! streams: each begins with an [`Open`] line naming the vsock port it
//! stands in for, so the services at both ends keep their ports and their
//! protocols.

use std::collections::VecDeque;
use std::future::Future;
use std::io;
use std::task::Poll;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::compat::{Compat, FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};

pub mod tls;

/// The name the worker's certificate carries, and the agent asks for.
pub const SERVER_NAME: &str = "hangar-hub";

/// The longest line either end reads before the streams start.
const MAX_LINE: u64 = 64 << 10;

/// What an agent needs to dial its worker, which the worker gives the guest
/// with its boot: where the hub is, its certificate's fingerprint, and the
/// environment's name and secret for this boot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// The hub's host and port, as the guest reaches it.
    pub address: String,
    pub fingerprint: String,
    pub environment: String,
    pub secret: String,
}

/// What an agent says first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub environment: String,
    /// The secret the worker gave the guest for this boot.
    pub secret: String,
    /// The agent's build, for the worker's log.
    #[serde(default)]
    pub agent: String,
}

/// The worker's answer to a Hello: empty `error` lets the agent in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Welcome {
    #[serde(default)]
    pub error: String,
}

/// A stream's first line: the vsock port it stands in for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Open {
    pub port: u32,
}

/// One stream over a link, read from just after its Open line. Its reads
/// and writes must be polled from one task: a yamux stream's reads send
/// window updates down the same channel its writes use, and that channel
/// wakes only the task that last waited on it.
pub type Stream = BufReader<Compat<yamux::Stream>>;

/// The connection a link runs over, the TLS session with what was read past
/// the handshake's lines.
trait Io: AsyncRead + AsyncWrite + Unpin + Send + 'static {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + 'static> Io for T {}

/// Dials the worker's hub at addr, whose certificate has fingerprint, and
/// says hello. A refusal is PermissionDenied, with the worker's reason.
pub async fn connect(addr: &str, fingerprint: &str, hello: &Hello) -> io::Result<(Link, Incoming)> {
    let tcp = TcpStream::connect(addr).await?;
    keep_alive(&tcp)?;
    let config = tls::client(fingerprint)?;
    let name = rustls::pki_types::ServerName::try_from(SERVER_NAME)
        .map_err(io::Error::other)?
        .to_owned();
    let tls = tokio_rustls::TlsConnector::from(config)
        .connect(name, tcp)
        .await?;
    let mut io = BufReader::new(tls);
    write_line(&mut io, hello).await?;
    let w: Welcome = read_line(&mut io).await?;
    if !w.error.is_empty() {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, w.error));
    }
    Ok(Link::new(io, yamux::Mode::Client))
}

/// The worker's half: TLS with its certificate, then each agent's Hello.
#[derive(Clone)]
pub struct Acceptor {
    tls: tokio_rustls::TlsAcceptor,
}

impl Acceptor {
    /// An acceptor presenting cert (DER) with its PKCS #8 key.
    pub fn new(cert: Vec<u8>, key: Vec<u8>) -> io::Result<Acceptor> {
        Ok(Acceptor {
            tls: tokio_rustls::TlsAcceptor::from(tls::server(cert, key)?),
        })
    }

    /// Takes a connection through TLS and reads its Hello, within timeout:
    /// a connection that says nothing is dropped rather than held.
    pub async fn hello(&self, tcp: TcpStream, timeout: Duration) -> io::Result<Greeting> {
        keep_alive(&tcp)?;
        tokio::time::timeout(timeout, async {
            let tls = self.tls.accept(tcp).await?;
            let mut io = BufReader::new(tls);
            let hello: Hello = read_line(&mut io).await?;
            Ok(Greeting {
                hello,
                io: Box::new(io),
            })
        })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no hello"))?
    }
}

/// An agent that has said hello, waiting to be let in or refused.
pub struct Greeting {
    pub hello: Hello,
    io: Box<dyn Io>,
}

impl Greeting {
    /// Lets the agent in: the link's streams start.
    pub async fn welcome(mut self) -> io::Result<(Link, Incoming)> {
        write_line(&mut self.io, &Welcome::default()).await?;
        Ok(Link::new(self.io, yamux::Mode::Server))
    }

    /// Turns the agent away, telling it why.
    pub async fn refuse(mut self, why: &str) {
        let w = Welcome { error: why.into() };
        let _ = write_line(&mut self.io, &w).await;
        let _ = self.io.shutdown().await;
    }
}

/// TCP's own keepalive, so a link whose other end has gone (a machine
/// stopped, or suspended and resumed elsewhere) fails within a minute even
/// when nothing is being sent.
fn keep_alive(tcp: &TcpStream) -> io::Result<()> {
    tcp.set_nodelay(true)?;
    let ka = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(15))
        .with_interval(Duration::from_secs(5))
        .with_retries(4);
    socket2::SockRef::from(tcp).set_tcp_keepalive(&ka)
}

async fn write_line<W: AsyncWrite + Unpin, T: Serialize>(w: &mut W, v: &T) -> io::Result<()> {
    let mut line = serde_json::to_vec(v).map_err(io::Error::other)?;
    line.push(b'\n');
    w.write_all(&line).await?;
    w.flush().await
}

async fn read_line<R: AsyncBufReadExt + Unpin, T: DeserializeOwned>(r: &mut R) -> io::Result<T> {
    let mut line = Vec::new();
    let n = (&mut *r)
        .take(MAX_LINE)
        .read_until(b'\n', &mut line)
        .await?;
    if n == 0 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    if !line.ends_with(b"\n") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "a hub line too long or cut off",
        ));
    }
    serde_json::from_slice(&line).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// A request to open a stream, and where its answer goes.
type Opener = oneshot::Sender<Result<yamux::Stream, yamux::ConnectionError>>;

/// One connection between an agent and its worker, over which either end
/// opens streams. A task of its own drives it until it closes, or until
/// this and its [`Incoming`] are dropped.
#[derive(Clone)]
pub struct Link {
    opens: mpsc::UnboundedSender<Opener>,
    closed: watch::Receiver<bool>,
}

/// The streams the other end opens.
pub struct Incoming {
    rx: mpsc::UnboundedReceiver<yamux::Stream>,
}

impl Link {
    fn new<T: Io>(io: T, mode: yamux::Mode) -> (Link, Incoming) {
        let mut cfg = yamux::Config::default();
        cfg.set_max_num_streams(4096);
        let conn = yamux::Connection::new(io.compat(), cfg, mode);
        let (opens, open_rx) = mpsc::unbounded_channel();
        let (inbound, rx) = mpsc::unbounded_channel();
        let (closed_tx, closed) = watch::channel(false);
        tokio::spawn(drive(conn, open_rx, inbound, closed_tx));
        (Link { opens, closed }, Incoming { rx })
    }

    /// Opens a stream standing in for the other end's vsock port.
    pub async fn open(&self, port: u32) -> io::Result<Stream> {
        let (tx, rx) = oneshot::channel();
        self.opens.send(tx).map_err(|_| gone())?;
        let s = rx.await.map_err(|_| gone())?.map_err(io::Error::other)?;
        let mut s = s.compat();
        write_line(&mut s, &Open { port }).await?;
        Ok(BufReader::new(s))
    }

    /// Whether the connection has closed.
    pub fn is_closed(&self) -> bool {
        *self.closed.borrow()
    }

    /// Waits for the connection to close.
    pub fn closed(&self) -> impl Future<Output = ()> + Send + 'static {
        let mut rx = self.closed.clone();
        async move {
            let _ = rx.wait_for(|c| *c).await;
        }
    }
}

impl Incoming {
    /// The next stream the other end opened, its Open line not yet read;
    /// none once the connection has closed.
    pub async fn next(&mut self) -> Option<Arriving> {
        self.rx.recv().await.map(Arriving)
    }
}

/// A stream the other end opened.
pub struct Arriving(yamux::Stream);

impl Arriving {
    /// Acknowledges the stream and reads which port it is for. The other
    /// end's yamux gives up on the whole connection when a stream it opened
    /// goes unacknowledged for long, so this is done at once, in a task of
    /// the stream's own.
    pub async fn open(self) -> io::Result<(u32, Stream)> {
        let mut s = self.0.compat();
        // An empty write sends the acknowledging frame at once.
        if s.write(&[]).await.is_err() {
            return Err(gone());
        }
        let mut r = BufReader::new(s);
        let o: Open = read_line(&mut r).await?;
        Ok((o.port, r))
    }
}

fn gone() -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionAborted, "the hub link has closed")
}

/// Drives a connection: opens what is asked for, and hands on what the
/// other end opens, until it fails or closes, or nothing here holds it.
async fn drive<T: futures_util::AsyncRead + futures_util::AsyncWrite + Unpin>(
    mut conn: yamux::Connection<T>,
    mut opens: mpsc::UnboundedReceiver<Opener>,
    inbound: mpsc::UnboundedSender<yamux::Stream>,
    closed: watch::Sender<bool>,
) {
    let mut waiting: VecDeque<Opener> = VecDeque::new();
    let mut asking = true;
    std::future::poll_fn(|cx| {
        while asking {
            match opens.poll_recv(cx) {
                Poll::Ready(Some(o)) => waiting.push_back(o),
                Poll::Ready(None) => asking = false,
                Poll::Pending => break,
            }
        }
        while !waiting.is_empty() {
            match conn.poll_new_outbound(cx) {
                Poll::Ready(r) => {
                    let failed = r.is_err();
                    if let Some(o) = waiting.pop_front() {
                        let _ = o.send(r);
                    }
                    if failed {
                        return Poll::Ready(());
                    }
                }
                Poll::Pending => break,
            }
        }
        loop {
            match conn.poll_next_inbound(cx) {
                Poll::Ready(Some(Ok(s))) => {
                    let _ = inbound.send(s);
                }
                Poll::Ready(Some(Err(_)) | None) => return Poll::Ready(()),
                Poll::Pending => break,
            }
        }
        // Nothing here can open a stream or take one: the link is over.
        if !asking && inbound.is_closed() {
            return conn.poll_close(cx).map(|_| ());
        }
        Poll::Pending
    })
    .await;
    for o in waiting {
        let _ = o.send(Err(yamux::ConnectionError::Closed));
    }
    let _ = closed.send(true);
}

/// Carries bytes both ways between a stream and a connection until both
/// directions have ended, from one task, as a stream needs.
pub async fn relay<A, B>(mut a: A, mut b: B)
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    let _ = tokio::io::copy_bidirectional(&mut a, &mut b).await;
    let _ = a.shutdown().await;
    let _ = b.shutdown().await;
}
