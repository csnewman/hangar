//! Connections to a port in the guest, as its monitor carries vsock.

use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};
use tokio::net::UnixStream;

use crate::err;

/// Where libkrun listens for connections to the guest's port: a socket per
/// port beside the vsock base.
pub fn krun_socket(base: &Path, port: u32) -> PathBuf {
    let mut p = base.as_os_str().to_owned();
    p.push(format!("-g{port}"));
    PathBuf::from(p)
}

/// A connection to the guest, with what was read past its handshake.
pub struct GuestConn {
    r: BufReader<UnixStream>,
}

impl GuestConn {
    pub fn into_inner(self) -> BufReader<UnixStream> {
        self.r
    }
}

impl AsyncRead for GuestConn {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.r).poll_read(cx, buf)
    }
}

impl AsyncWrite for GuestConn {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(self.r.get_mut()).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(self.r.get_mut()).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(self.r.get_mut()).poll_shutdown(cx)
    }
}

/// Connects to port in the guest of a Cloud Hypervisor machine, whose vsock
/// is one unix socket: asked "CONNECT <port>", it answers "OK <host port>"
/// and the connection is the guest's from there.
pub async fn dial_cloud_hypervisor(socket: &Path, port: u32) -> io::Result<GuestConn> {
    let mut c = UnixStream::connect(socket).await?;
    c.write_all(format!("CONNECT {port}\n").as_bytes()).await?;
    let mut r = BufReader::new(c);
    let mut line = String::new();
    r.read_line(&mut line)
        .await
        .map_err(|e| err(format!("connecting to guest port {port}: {e}")))?;
    if !line.starts_with("OK ") {
        return Err(err(format!(
            "connecting to guest port {port}: {}",
            line.trim()
        )));
    }
    Ok(GuestConn { r })
}

/// Connects to port in the guest of a libkrun machine, on the port's own
/// socket.
pub async fn dial_krun(base: &Path, port: u32) -> io::Result<GuestConn> {
    Ok(GuestConn {
        r: BufReader::new(UnixStream::connect(krun_socket(base, port)).await?),
    })
}
