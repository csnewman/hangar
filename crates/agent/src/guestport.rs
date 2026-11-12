//! The environment's own TCP ports, reached by the host on vsock port 8108
//! in the protocol `internal/guestport` defines: for each connection the
//! agent connects to the port the host names on the guest's own loopback,
//! then carries bytes both ways.
//!
//! The host writes the port, two bytes big-endian. The agent answers one
//! byte: CONNECTED, after which the stream is the connection; or REFUSED,
//! when nothing listens on the port, or FAILED, and it closes.

use std::io;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

pub const PORT: u32 = 8108;

pub const CONNECTED: u8 = 0;
pub const REFUSED: u8 = 1;
pub const FAILED: u8 = 2;

/// How long the agent tries to connect to the port.
const DIAL_TIMEOUT: Duration = Duration::from_secs(10);

/// Answers one request on `conn`, and carries the connection until both
/// sides have finished, passing each side's end of stream to the other.
pub async fn serve<S: AsyncRead + AsyncWrite + Unpin>(mut conn: S) {
    let mut req = [0u8; 2];
    if conn.read_exact(&mut req).await.is_err() {
        return;
    }
    let port = u16::from_be_bytes(req);
    let target = tokio::time::timeout(DIAL_TIMEOUT, TcpStream::connect(("localhost", port))).await;
    let mut target = match target {
        Ok(Ok(t)) => t,
        Ok(Err(e)) if e.kind() == io::ErrorKind::ConnectionRefused => {
            let _ = conn.write_all(&[REFUSED]).await;
            return;
        }
        _ => {
            let _ = conn.write_all(&[FAILED]).await;
            return;
        }
    };
    if conn.write_all(&[CONNECTED]).await.is_err() {
        return;
    }
    let _ = tokio::io::copy_bidirectional(&mut conn, &mut target).await;
}
