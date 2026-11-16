//! The agent's half of the hub (hangar-hub): where the guest has no vsock to
//! its worker, as on a Proxmox VM, the agent dials the worker over the
//! network instead, and stays connected. Every service the worker reaches on
//! a vsock port is reached on a stream of that link, handed to the service
//! as the connection a vsock listener would have given it; what the agent
//! dials on the host, the control channel and the shared files, it opens
//! as a stream.
//!
//! The worker gives the agent where to dial and with what secret in the
//! initramfs, which init keeps for the agent in /dev, the one filesystem
//! that passes into the new root untouched (systemd mounts its own /run).

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::os::fd::OwnedFd;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use hangar_hub::{Hello, Link, Settings};
use tokio::sync::watch;

use crate::fdio::FdStream;

/// Where init keeps the hub's settings for the agent: root's alone.
pub const SETTINGS: &str = "/dev/hangar/hub.json";

/// Where they arrive, in the initramfs.
pub const INITRAMFS_SETTINGS: &str = "/hub.json";

/// Where the agent relays the shared files' NFS on the guest's loopback,
/// for the kernel's client: a port only root may bind, so no other program
/// in the guest can take it while the agent restarts.
pub const NFS_RELAY_PORT: u16 = 849;

/// How long a stream to the host waits for the link, as when the agent has
/// just started or the guest has just resumed.
const LINK_WITHIN: Duration = Duration::from_secs(30);

/// The settings, if the worker gave any: the agent uses the hub.
pub fn settings() -> Option<&'static Settings> {
    static S: OnceLock<Option<Settings>> = OnceLock::new();
    S.get_or_init(|| {
        let b = std::fs::read(SETTINGS).ok()?;
        serde_json::from_slice(&b).ok()
    })
    .as_ref()
}

type Handler = Arc<dyn Fn(FdStream) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

fn handlers() -> &'static Mutex<HashMap<u32, Handler>> {
    static H: OnceLock<Mutex<HashMap<u32, Handler>>> = OnceLock::new();
    H.get_or_init(Default::default)
}

fn link() -> &'static watch::Sender<Option<Link>> {
    static L: OnceLock<watch::Sender<Option<Link>>> = OnceLock::new();
    L.get_or_init(|| watch::channel(None).0)
}

/// Answers streams the worker opens to port with handle, as a vsock
/// listener on the port would.
pub fn handle<F, Fut>(port: u32, handle: F)
where
    F: Fn(FdStream) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let h: Handler = Arc::new(move |s| Box::pin(handle(s)));
    handlers().lock().unwrap().insert(port, h);
}

/// Keeps the agent connected to the worker's hub for as long as the guest
/// runs, dialling again whenever the link drops.
pub async fn run(settings: &'static Settings) {
    let hello = Hello {
        environment: settings.environment.clone(),
        secret: settings.secret.clone(),
        agent: crate::service::BUILD.to_string(),
    };
    let mut wait = Duration::from_secs(1);
    let mut said = false;
    loop {
        match hangar_hub::connect(&settings.address, &settings.fingerprint, &hello).await {
            Ok((l, mut incoming)) => {
                wait = Duration::from_secs(1);
                said = false;
                link().send_replace(Some(l.clone()));
                while let Some(a) = incoming.next().await {
                    tokio::spawn(async move {
                        let Ok((port, stream)) = a.open().await else {
                            return;
                        };
                        let h = handlers().lock().unwrap().get(&port).cloned();
                        let Some(h) = h else { return };
                        let Ok((here, there)) = pair() else { return };
                        let Ok(conn) = FdStream::new(there) else {
                            return;
                        };
                        tokio::spawn(h(conn));
                        hangar_hub::relay(stream, here).await;
                    });
                }
                link().send_replace(None);
                // A link that was up has dropped, as a resumed guest's does:
                // dial again at once.
                continue;
            }
            Err(e) => {
                if !said {
                    said = true;
                    eprintln!("hangar-agent: hub {}: {e}", settings.address);
                }
            }
        }
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(Duration::from_secs(10));
    }
}

/// Opens a stream to port on the host over the hub, as a connection.
pub async fn open(port: u32) -> io::Result<FdStream> {
    let stream = stream(port).await?;
    let (here, there) = pair()?;
    tokio::spawn(hangar_hub::relay(stream, here));
    FdStream::new(there)
}

/// The host's port for the shared files, NFS's own.
const SHARED_FILES_PORT: u32 = 2049;

/// Relays the kernel's NFS client, on the guest's loopback, to the shared
/// files on the host, a stream for each connection it makes.
pub async fn relay_shared_files() {
    loop {
        let ln = match tokio::net::TcpListener::bind(("127.0.0.1", NFS_RELAY_PORT)).await {
            Ok(ln) => ln,
            Err(e) => {
                eprintln!("hangar-agent: shared files relay on port {NFS_RELAY_PORT}: {e}");
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        while let Ok((tcp, _)) = ln.accept().await {
            tokio::spawn(async move {
                if let Ok(s) = stream(SHARED_FILES_PORT).await {
                    hangar_hub::relay(tcp, s).await;
                }
            });
        }
    }
}

/// Opens a stream to port on the host over the hub.
async fn stream(port: u32) -> io::Result<hangar_hub::Stream> {
    let mut rx = link().subscribe();
    let l = tokio::time::timeout(
        LINK_WITHIN,
        rx.wait_for(|l| l.as_ref().is_some_and(|l| !l.is_closed())),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the worker's hub is not connected"))?
    .map_err(io::Error::other)?
    .clone()
    .expect("a link");
    l.open(port).await
}

/// A connected pair of sockets: one end relayed to a stream here, the other
/// given to a service as its connection.
fn pair() -> io::Result<(tokio::net::UnixStream, OwnedFd)> {
    let (a, b) = std::os::unix::net::UnixStream::pair()?;
    a.set_nonblocking(true)?;
    Ok((tokio::net::UnixStream::from_std(a)?, OwnedFd::from(b)))
}

/// Keeps the settings the initramfs carries for the agent, before init
/// hands over: in /dev, readable by root alone.
pub fn keep_settings() -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let Ok(b) = std::fs::read(INITRAMFS_SETTINGS) else {
        return Ok(());
    };
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create("/dev/hangar")?;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(SETTINGS)?;
    io::Write::write_all(&mut f, &b)?;
    let _ = std::fs::remove_file(INITRAMFS_SETTINGS);
    Ok(())
}
