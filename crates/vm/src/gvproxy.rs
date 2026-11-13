//! A libkrun guest's network on macOS: gvproxy (gvisor-tap-vsock's), a
//! TCP/IP stack in user space that answers the guest's DHCP and DNS and
//! makes its connections out through the host's own sockets. The guest's
//! device reaches it over a unix datagram socket libkrun sends its frames
//! to, in vfkit's protocol.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::ch::{Backend, launch};
use crate::err;

/// The guest's address, which its network leases to its device's MAC.
pub const GUEST_IP: &str = "192.168.127.2";

/// gvproxy's configuration: the guest's subnet, its gateway (which answers
/// DNS), and the lease of GUEST_IP to the guest's device. It names no
/// forwards, which gvproxy's defaults have without a configuration (the
/// host's port 2222 to the guest's SSH, the SSH gateway's port). Nothing on
/// the host's loopback is to be reachable from a guest -- a guest talks to
/// the worker over vsock -- but gvproxy NATs the host's address in the
/// subnet to the host's loopback whenever no NAT is named, and has no way
/// to name none; so it is NATed to 192.0.2.1, one of RFC 5737's addresses
/// for documentation, which nothing answers on.
const CONFIG: &str = "\
stack:
  mtu: 1500
  subnet: 192.168.127.0/24
  gatewayIP: 192.168.127.1
  hostIP: 192.168.127.254
  gatewayMacAddress: 5a:94:ef:e4:0c:dd
  nat:
    192.168.127.254: 192.0.2.1
  dhcpStaticLeases:
    192.168.127.2: 5a:94:ef:e4:0c:ee
";

/// gvproxy: bin if named, else on PATH, or beside hangar-krun.
pub fn find(bin: Option<&Path>) -> io::Result<PathBuf> {
    if let Some(b) = bin {
        return Ok(b.to_path_buf());
    }
    if let Some(p) = std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .map(|d| d.join("gvproxy"))
        .find(|p| p.is_file())
    {
        return Ok(p);
    }
    crate::krun::tool("gvproxy")
        .map_err(|e| err(format!("no gvproxy for the guest's network: {e}")))
}

/// A running gvproxy, serving one guest.
pub struct Network {
    backend: Backend,
    socket: PathBuf,
}

impl Network {
    /// Starts gvproxy listening for the guest's frames on socket, its
    /// configuration beside it, its log in log.
    pub async fn start(bin: &Path, socket: &Path, log: &Path) -> io::Result<Network> {
        let _ = std::fs::remove_file(socket);
        let conf = socket.with_extension("yaml");
        std::fs::write(&conf, CONFIG)?;
        let mut cmd = tokio::process::Command::new(bin);
        cmd.arg("-config")
            .arg(&conf)
            .arg("-listen-vfkit")
            .arg(format!("unixgram://{}", socket.display()))
            .arg("-log-file")
            .arg(log);
        let backend = launch(cmd, "gvproxy", Some(socket), None, Duration::from_secs(10)).await?;
        Ok(Network {
            backend,
            socket: socket.to_path_buf(),
        })
    }

    pub fn pid(&self) -> u32 {
        self.backend.pid()
    }

    /// Stops gvproxy and removes its socket.
    pub async fn close(&self) {
        self.backend.stop().await;
        let _ = std::fs::remove_file(&self.socket);
    }
}
