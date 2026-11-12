//! Reading and writing, on tokio, a file descriptor of a kind tokio has no
//! type for: a vsock socket, a pseudo-terminal's master.
//!
//! A read waits for the descriptor to be readable or to have failed. tokio
//! wakes a reader only for the first, and a vsock connection the transport
//! resets -- as the monitor does to every one when it restores a snapshot
//! -- only fails: it would wait for ever on a connection that is gone.

use std::future::Future;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, ready};

use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, Interest, ReadBuf};

/// The most an `AsyncRead` read takes at once.
const READ_CHUNK: usize = 64 << 10;

/// A descriptor read and written without blocking. Reads and writes take
/// `&self`, so one task may read while another writes.
pub struct FdStream {
    fd: Arc<AsyncFd<OwnedFd>>,
    /// The read `AsyncRead` is waiting on, and what one read beyond the
    /// room its caller had.
    reading: Mutex<Reading>,
}

/// A read under way, which gives what it read.
type ReadFuture = Pin<Box<dyn Future<Output = io::Result<Vec<u8>>> + Send>>;

#[derive(Default)]
struct Reading {
    pending: Option<ReadFuture>,
    left: Vec<u8>,
    at: usize,
}

/// Reads what is there, once the descriptor is readable or has failed.
async fn read_ready(fd: &AsyncFd<OwnedFd>, buf: &mut [u8]) -> io::Result<usize> {
    loop {
        let mut ready = fd.ready(Interest::READABLE | Interest::ERROR).await?;
        if let Ok(r) = ready.try_io(|fd| sys_read(fd.as_raw_fd(), buf)) {
            return r;
        }
    }
}

impl FdStream {
    /// Wraps `fd`, making it non-blocking.
    pub fn new(fd: OwnedFd) -> io::Result<Self> {
        set_nonblocking(fd.as_raw_fd())?;
        // SAFETY: the stream owns the descriptor, which nothing replaces or
        // closes while it is registered.
        Ok(Self {
            fd: Arc::new(unsafe { AsyncFd::register(fd)? }),
            reading: Mutex::new(Reading::default()),
        })
    }

    pub fn raw(&self) -> RawFd {
        self.fd.get_ref().as_raw_fd()
    }

    /// Reads what is there, waiting for something to be.
    pub async fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        read_ready(&self.fd, buf).await
    }

    /// Writes all of `buf`.
    pub async fn write_all(&self, mut buf: &[u8]) -> io::Result<()> {
        while !buf.is_empty() {
            let mut ready = self.fd.writable().await?;
            match ready.try_io(|fd| sys_write(fd.as_raw_fd(), buf)) {
                Ok(Ok(0)) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(Ok(n)) => buf = &buf[n..],
                Ok(Err(e)) => return Err(e),
                Err(_) => {}
            }
        }
        Ok(())
    }
}

impl AsyncRead for FdStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let Self { fd, reading } = self.get_mut();
        let r = reading.get_mut().unwrap_or_else(|e| e.into_inner());
        if r.at < r.left.len() {
            let n = (r.left.len() - r.at).min(buf.remaining());
            buf.put_slice(&r.left[r.at..r.at + n]);
            r.at += n;
            return Poll::Ready(Ok(()));
        }
        if buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        let pending = r.pending.get_or_insert_with(|| {
            let (fd, cap) = (fd.clone(), buf.remaining().min(READ_CHUNK));
            Box::pin(async move {
                let mut b = vec![0; cap];
                let n = read_ready(&fd, &mut b).await?;
                b.truncate(n);
                Ok(b)
            })
        });
        let got = ready!(pending.as_mut().poll(cx));
        r.pending = None;
        let data = got?;
        let n = data.len().min(buf.remaining());
        buf.put_slice(&data[..n]);
        (r.left, r.at) = (data, n);
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for FdStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        loop {
            let mut ready = ready!(self.fd.poll_write_ready(cx))?;
            if let Ok(r) = ready.try_io(|fd| sys_write(fd.as_raw_fd(), buf)) {
                return Poll::Ready(r);
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    /// A socket is told no more is coming; anything else has no way to say
    /// so, and is left to be closed.
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        // SAFETY: shutdown(2) on a descriptor this stream owns.
        if unsafe { libc::shutdown(self.raw(), libc::SHUT_WR) } < 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() != Some(libc::ENOTSOCK) {
                return Poll::Ready(Err(e));
            }
        }
        Poll::Ready(Ok(()))
    }
}

fn sys_read(fd: RawFd, buf: &mut [u8]) -> io::Result<usize> {
    // SAFETY: read(2) into a buffer of the length given.
    let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(n as usize)
}

fn sys_write(fd: RawFd, buf: &[u8]) -> io::Result<usize> {
    // SAFETY: write(2) from a buffer of the length given.
    let n = unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(n as usize)
}

pub fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    // SAFETY: fcntl(2) flag calls on a descriptor the caller owns.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}
