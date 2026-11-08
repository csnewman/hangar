//! The WebSockets the app opens to a server: a terminal session's, the
//! Code service's. Reads wait briefly, so one thread can both read and send.
//! A quiet connection is pinged, and one that stops answering is given up,
//! for its owner to dial again.

use std::fmt;
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde::Deserialize;
use tungstenite::client::IntoClientRequest;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Connector, WebSocket};

pub type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

/// How long a read waits before the connection looks for something to
/// send: the most a key or a request waits to go.
pub const POLL: Duration = Duration::from_millis(8);

/// Whether a read error is only the wait running out.
pub fn idle(e: &tungstenite::Error) -> bool {
    matches!(e, tungstenite::Error::Io(e)
        if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut))
}

/// Why a socket could not be opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectError {
    /// The server answered, refusing: its HTTP status (0 for a request
    /// that could not be made) and why.
    Refused(u16, String),
    /// The server could not be reached, which may pass.
    Unreachable(String),
}

impl ConnectError {
    /// Whether trying again may succeed: the network, or a server that is
    /// restarting, overloaded, or holding an environment that is not running
    /// yet. A refusal of who is asking or of what is asked for is final.
    pub fn passes(&self) -> bool {
        match self {
            ConnectError::Unreachable(_) => true,
            ConnectError::Refused(status, _) => {
                matches!(status, 408 | 409 | 425 | 429) || *status >= 500
            }
        }
    }
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectError::Refused(_, why) | ConnectError::Unreachable(why) => f.write_str(why),
        }
    }
}

impl From<ConnectError> for String {
    fn from(e: ConnectError) -> String {
        e.to_string()
    }
}

/// Opens the WebSocket, through TLS checked as the OS checks it for wss.
pub fn connect(url: &str, auth: &str) -> Result<Socket, ConnectError> {
    let refused = |why: String| ConnectError::Refused(0, why);
    let unreachable = |e: std::io::Error| ConnectError::Unreachable(e.to_string());
    let mut req = url
        .into_client_request()
        .map_err(|e| refused(e.to_string()))?;
    req.headers_mut().insert(
        "Authorization",
        auth.parse().map_err(|_| refused("a bad token".into()))?,
    );
    let uri = req.uri().clone();
    let host = uri
        .host()
        .ok_or_else(|| refused("no host".into()))?
        .to_string();
    let tls = uri.scheme_str() == Some("wss");
    let port = uri.port_u16().unwrap_or(if tls { 443 } else { 80 });
    let tcp = TcpStream::connect((host.as_str(), port)).map_err(unreachable)?;
    tcp.set_nodelay(true).map_err(unreachable)?;
    let connector = if tls {
        use rustls_platform_verifier::ConfigVerifierExt;
        let config =
            rustls::ClientConfig::with_platform_verifier().map_err(|e| refused(e.to_string()))?;
        Connector::Rustls(Arc::new(config))
    } else {
        Connector::Plain
    };
    let (ws, _) = tungstenite::client_tls_with_config(req, tcp, None, Some(connector)).map_err(
        |e| match e {
            tungstenite::HandshakeError::Failure(tungstenite::Error::Http(r)) => {
                let body = r
                    .body()
                    .as_ref()
                    .map(|b| String::from_utf8_lossy(b).to_string())
                    .unwrap_or_default();
                #[derive(Deserialize)]
                struct Body {
                    error: String,
                }
                let why = serde_json::from_str::<Body>(&body)
                    .map(|b| b.error)
                    .unwrap_or_else(|_| format!("the server answered {}", r.status()));
                ConnectError::Refused(r.status().as_u16(), why)
            }
            e => ConnectError::Unreachable(e.to_string()),
        },
    )?;
    // Reads wait only briefly, so what is typed goes out between them.
    let sock = match ws.get_ref() {
        MaybeTlsStream::Plain(s) => s,
        MaybeTlsStream::Rustls(s) => s.get_ref(),
        _ => return Err(refused("an unknown stream".into())),
    };
    sock.set_read_timeout(Some(POLL)).map_err(unreachable)?;
    Ok(ws)
}

/// How long a connection is quiet before it is pinged, and how long a ping
/// has to be answered.
pub const PING_AFTER: Duration = Duration::from_secs(15);
pub const ANSWER_WITHIN: Duration = Duration::from_secs(10);

/// Whether a connection is still there: pinged when nothing has come for a
/// while, and given up when a ping goes unanswered. Timed by the wall
/// clock, which runs on while the machine sleeps (the monotonic one stops),
/// so a connection is checked as soon as the machine wakes.
pub struct Liveness {
    heard: SystemTime,
    pinged: Option<SystemTime>,
}

impl Default for Liveness {
    fn default() -> Self {
        Liveness {
            heard: SystemTime::now(),
            pinged: None,
        }
    }
}

impl Liveness {
    /// Something came: the connection is there.
    pub fn heard(&mut self) {
        self.heard = SystemTime::now();
        self.pinged = None;
    }

    /// What is due at now: Ok(true) to ping, or why the connection is
    /// taken to be gone.
    pub fn due(&mut self, now: SystemTime) -> Result<bool, String> {
        let since = |t: SystemTime| now.duration_since(t).unwrap_or_default();
        match self.pinged {
            Some(p) if since(p) >= ANSWER_WITHIN => {
                Err(format!("no answer for {}s", since(self.heard).as_secs()))
            }
            Some(_) => Ok(false),
            None if since(self.heard) >= PING_AFTER => {
                self.pinged = Some(now);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Pings the connection if it is due, or says why it is gone.
    pub fn check(&mut self, ws: &mut Socket) -> Result<(), String> {
        if self.due(SystemTime::now())? {
            ws.send(tungstenite::Message::Ping(Vec::new().into()))
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}
