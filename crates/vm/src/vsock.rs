//! vsock as a monitor that carries it on unix sockets gives it to the host.
//!
//! vsock is the agent's channel because it is not a network: a guest
//! reaches the host without an address, a route or a NIC, so an environment
//! whose networking is broken, still booting, or deliberately locked down is
//! still reachable. Nothing on the host's network can dial into a guest
//! either, because the only path is the one the monitor created. And a
//! vsock endpoint is a socket, so an environment carries many independent
//! connections at once, each its own stream on its own port.
//!
//! Cloud Hypervisor and libkrun both implement vsock in userspace: the
//! guest sees an ordinary virtio-vsock device, but the host end is a unix
//! socket. When the guest connects to port P, the monitor dials the unix
//! socket at "<base>_<P>", so the host listens there, and the guest needs no
//! changes.
//!
//! Identity is preserved by the path. The socket belongs to one guest, its
//! name is chosen by the host, and only the monitor holding that guest is
//! told where it is. So a connection arriving on a listener came from that
//! environment and no other, and the CID it reports is the host's own
//! allocation rather than anything the peer claimed. One listener serves
//! one environment's port.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::net::{UnixListener, UnixStream};

use crate::err;

/// The context ID a guest dials to reach its host. Every guest uses this
/// same number: the guest does not need to know where it is running.
pub const CID_HOST: u32 = 2;
/// The lowest CID assignable to a guest; 0-2 are reserved.
pub const FIRST_GUEST_CID: u32 = 3;

/// The longest path a unix socket may have, its terminator not counted.
#[cfg(target_os = "macos")]
const MAX_PATH: usize = 103;
#[cfg(not(target_os = "macos"))]
const MAX_PATH: usize = 107;

/// Where a monitor expects the host to be listening for connections the
/// guest makes to port.
pub fn hybrid_path(base: &Path, port: u32) -> PathBuf {
    let mut p = base.as_os_str().to_owned();
    p.push(format!("_{port}"));
    PathBuf::from(p)
}

/// Accepts the connections a guest makes to one port.
pub struct HybridListener {
    l: UnixListener,
    path: PathBuf,
    cid: u32,
}

impl HybridListener {
    /// Binds the unix socket the monitor dials when the guest connects to
    /// port. base is the monitor's vsock socket path, the same value it is
    /// configured with; cid is what accept reports, the environment's.
    pub fn listen(base: &Path, port: u32, cid: u32) -> io::Result<HybridListener> {
        let path = hybrid_path(base, port);
        // sockaddr_un's path is short, and a longer one is silently
        // truncated into a socket nobody can find.
        let n = path.as_os_str().len();
        if n > MAX_PATH {
            return Err(err(format!(
                "agent socket path is too long ({n} bytes, limit {MAX_PATH}): {}",
                path.display()
            )));
        }
        // A stale socket from a monitor that was killed rather than stopped
        // would otherwise make every later boot fail to bind. Only a socket
        // is removed: anything else at that path is a mistake worth
        // reporting.
        if let Ok(m) = std::fs::symlink_metadata(&path) {
            use std::os::unix::fs::FileTypeExt;
            if !m.file_type().is_socket() {
                return Err(err(format!(
                    "{} exists and is not a socket",
                    path.display()
                )));
            }
            std::fs::remove_file(&path)
                .map_err(|e| err(format!("removing a stale agent socket: {e}")))?;
        }
        let l = UnixListener::bind(&path).map_err(|e| {
            err(format!(
                "binding the agent socket at {}: {e}",
                path.display()
            ))
        })?;
        Ok(HybridListener { l, path, cid })
    }

    /// Waits up to timeout, or for ever for none, for the guest to connect,
    /// and gives the connection with the environment's CID.
    pub async fn accept(&self, timeout: Option<Duration>) -> io::Result<(UnixStream, u32)> {
        let accepted = match timeout {
            Some(t) => tokio::time::timeout(t, self.l.accept())
                .await
                .map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        "timed out waiting for a vsock connection",
                    )
                })?,
            None => self.l.accept().await,
        };
        let (c, _) = accepted.map_err(|e| err(format!("accepting the agent connection: {e}")))?;
        Ok((c, self.cid))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for HybridListener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
