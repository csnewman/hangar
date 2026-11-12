//! Reading and writing, on tokio, a file descriptor of a kind tokio has no
//! type for: a vsock socket, a pseudo-terminal's master.

use std::io;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// A descriptor read and written without blocking. Reads and writes take
/// `&self`, so one task may read while another writes.
pub struct FdStream {
    fd: AsyncFd<OwnedFd>,
}

impl FdStream {
    /// Wraps `fd`, making it non-blocking.
    pub fn new(fd: OwnedFd) -> io::Result<Self> {
        set_nonblocking(fd.as_raw_fd())?;
        // SAFETY: the stream owns the descriptor, which nothing replaces or
        // closes while it is registered.
        Ok(Self {
            fd: unsafe { AsyncFd::register(fd)? },
        })
    }

    pub fn raw(&self) -> RawFd {
        self.fd.get_ref().as_raw_fd()
    }

    /// Reads what is there, waiting for something to be.
    pub async fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let mut ready = self.fd.readable().await?;
            if let Ok(r) = ready.try_io(|fd| sys_read(fd.as_raw_fd(), buf)) {
                return r;
            }
        }
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
        loop {
            let mut ready = ready!(self.fd.poll_read_ready(cx))?;
            let unfilled = buf.initialize_unfilled();
            match ready.try_io(|fd| sys_read(fd.as_raw_fd(), unfilled)) {
                Ok(Ok(n)) => {
                    buf.advance(n);
                    return Poll::Ready(Ok(()));
                }
                Ok(Err(e)) => return Poll::Ready(Err(e)),
                Err(_) => {}
            }
        }
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
