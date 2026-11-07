//! The WebSockets the app opens to a server: a terminal session's, the
//! Code service's. Reads wait briefly, so one thread can both read and send.

use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

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

/// Opens the WebSocket, through TLS checked as the OS checks it for wss.
pub fn connect(url: &str, auth: &str) -> Result<Socket, String> {
    let mut req = url.into_client_request().map_err(|e| e.to_string())?;
    req.headers_mut().insert(
        "Authorization",
        auth.parse().map_err(|_| "a bad token".to_string())?,
    );
    let uri = req.uri().clone();
    let host = uri.host().ok_or("no host")?.to_string();
    let tls = uri.scheme_str() == Some("wss");
    let port = uri.port_u16().unwrap_or(if tls { 443 } else { 80 });
    let tcp = TcpStream::connect((host.as_str(), port)).map_err(|e| e.to_string())?;
    tcp.set_nodelay(true).map_err(|e| e.to_string())?;
    let connector = if tls {
        use rustls_platform_verifier::ConfigVerifierExt;
        let config = rustls::ClientConfig::with_platform_verifier().map_err(|e| e.to_string())?;
        Connector::Rustls(Arc::new(config))
    } else {
        Connector::Plain
    };
    let (ws, resp) = tungstenite::client_tls_with_config(req, tcp, None, Some(connector)).map_err(
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
                serde_json::from_str::<Body>(&body)
                    .map(|b| b.error)
                    .unwrap_or_else(|_| format!("the server answered {}", r.status()))
            }
            e => e.to_string(),
        },
    )?;
    let _ = resp;
    // Reads wait only briefly, so what is typed goes out between them.
    let sock = match ws.get_ref() {
        MaybeTlsStream::Plain(s) => s,
        MaybeTlsStream::Rustls(s) => s.get_ref(),
        _ => return Err("an unknown stream".into()),
    };
    sock.set_read_timeout(Some(POLL))
        .map_err(|e| e.to_string())?;
    Ok(ws)
}
