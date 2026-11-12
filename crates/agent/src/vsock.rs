//! vsock: the sockets between the guest and its host. The guest's services
//! listen on a port each, and the host dials them.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use tokio::io::unix::AsyncFd;

use crate::fdio::FdStream;

/// A socket listening on a vsock port, for connections from the host alone.
///
/// The agent's services act as root, or as any user they are asked to, and
/// trust whoever reaches them to have been let in by Hangar: the worker,
/// which the server's gateway and API reach them through. A process in the
/// guest can dial the guest's own ports too, through vsock's loopback
/// (CID 1), so every peer but the host is refused. The agent's ports are
/// below 1024, which only a privileged process may bind, so no other
/// process in the guest can take one while the agent restarts.
pub struct Listener {
    fd: AsyncFd<OwnedFd>,
}

impl Listener {
    pub fn bind(port: u32) -> io::Result<Self> {
        // SAFETY: socket(2), then bind(2) and listen(2) on the descriptor it
        // returned, with an address of the size given; the listener owns the
        // descriptor, which nothing else closes while it is registered.
        unsafe {
            let fd = libc::socket(
                libc::AF_VSOCK,
                libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                0,
            );
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let fd = OwnedFd::from_raw_fd(fd);
            let mut addr: libc::sockaddr_vm = std::mem::zeroed();
            addr.svm_family = libc::AF_VSOCK as libc::sa_family_t;
            addr.svm_port = port;
            addr.svm_cid = libc::VMADDR_CID_ANY;
            if libc::bind(
                fd.as_raw_fd(),
                (&addr as *const libc::sockaddr_vm).cast(),
                std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
            ) < 0
                || libc::listen(fd.as_raw_fd(), 128) < 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                fd: AsyncFd::register(fd)?,
            })
        }
    }

    /// The next connection from the host. One from anywhere else is
    /// closed as it arrives.
    pub async fn accept(&self) -> io::Result<FdStream> {
        loop {
            let mut ready = self.fd.readable().await?;
            let accepted = ready.try_io(|fd| {
                // SAFETY: accept4(2) on a listening socket, into an address
                // of the size given.
                unsafe {
                    let mut peer: libc::sockaddr_vm = std::mem::zeroed();
                    let mut len = std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t;
                    let c = libc::accept4(
                        fd.as_raw_fd(),
                        (&raw mut peer).cast(),
                        &mut len,
                        libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                    );
                    if c < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    // A descriptor accept4 just returned, owned by no one
                    // else.
                    Ok((OwnedFd::from_raw_fd(c), peer.svm_cid))
                }
            });
            match accepted {
                Ok(Ok((conn, libc::VMADDR_CID_HOST))) => return FdStream::new(conn),
                Ok(Ok((_, cid))) => {
                    eprintln!("hangar-agent: refused a connection from CID {cid}, not the host");
                }
                Ok(Err(e)) => return Err(e),
                Err(_would_block) => {}
            }
        }
    }
}

/// The host's context ID, which the guest dials it at.
pub const HOST: u32 = libc::VMADDR_CID_HOST;

/// Dials `port` on `cid`. The connect blocks the calling thread until the
/// other end answers or refuses, as vsock's do at once or after its connect
/// timeout.
pub fn connect(cid: u32, port: u32) -> io::Result<FdStream> {
    // SAFETY: socket(2), then connect(2) on the descriptor it returned with
    // an address of the size given; the descriptor is owned from the start.
    let fd = unsafe {
        let fd = libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = OwnedFd::from_raw_fd(fd);
        let mut addr: libc::sockaddr_vm = std::mem::zeroed();
        addr.svm_family = libc::AF_VSOCK as libc::sa_family_t;
        addr.svm_port = port;
        addr.svm_cid = cid;
        if libc::connect(
            fd.as_raw_fd(),
            (&addr as *const libc::sockaddr_vm).cast(),
            std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
        ) < 0
        {
            return Err(io::Error::last_os_error());
        }
        fd
    };
    FdStream::new(fd)
}
