//! The agent's half of the hub (hangar-hub): where the guest has no vsock to
//! its worker, as on a Proxmox VM, the agent dials the worker over the
//! network instead, and stays connected. Every service the worker reaches on
//! a vsock port is reached on a stream of that link, handed to the service
//! as the connection a vsock listener would have given it; what the agent
//! dials on the host, the control channel and the shared files, it opens
//! as a stream.
//!
//! The worker gives the agent where to dial and with what secret as JSON
//! (hangar_hub::Settings) in /dev/hangar/hub.json, root's alone: in the
//! initramfs, which init moves there, or, on a Proxmox VM, whose boot is
//! shared, written by the worker through the guest agent channel
//! (`crate::qga`) at every start, which the kernel command line says
//! (`hangar.hub=channel`). Nothing of it is in the VM's configuration,
//! which anyone who may see the VM can read, and /dev does not outlive the
//! boot.

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

/// Where the hub's settings are kept for the agent: root's alone.
pub const SETTINGS: &str = "/dev/hangar/hub.json";

/// Where they arrive, in the initramfs.
pub const INITRAMFS_SETTINGS: &str = "/hub.json";

/// The kernel command line's word for settings that come through the guest
/// agent channel, after the agent has started.
const CHANNEL_ARG: &str = "hangar.hub=channel";

/// Where the agent relays the shared files' NFS on the guest's loopback,
/// for the kernel's client: a port only root may bind, so no other program
/// in the guest can take it while the agent restarts.
pub const NFS_RELAY_PORT: u16 = 849;

/// How long a stream to the host waits for the link, as when the agent has
/// just started or the guest has just resumed.
const LINK_WITHIN: Duration = Duration::from_secs(30);

/// Whether the agent reaches its worker through the hub: its settings
/// came in the initramfs, or will come through the guest agent channel.
pub fn used() -> bool {
    static U: OnceLock<bool> = OnceLock::new();
    *U.get_or_init(|| {
        std::path::Path::new(SETTINGS).exists()
            || std::fs::read_to_string("/proc/cmdline")
                .unwrap_or_default()
                .split_whitespace()
                .any(|a| a == CHANNEL_ARG)
    })
}

/// The settings as they are, if they are whole: a file being written is
/// not yet.
fn current() -> Option<Settings> {
    let s: Settings = serde_json::from_slice(&std::fs::read(SETTINGS).ok()?).ok()?;
    let whole = !s.address.is_empty() && !s.fingerprint.is_empty();
    (whole && !s.environment.is_empty() && !s.secret.is_empty()).then_some(s)
}

/// The settings, once there are whole ones.
async fn settings() -> Settings {
    loop {
        if let Some(s) = current() {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

type Handler = Arc<dyn Fn(FdStream) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

fn handlers() -> &'static Mutex<HashMap<u32, Handler>> {
    static H: OnceLock<Mutex<HashMap<u32, Handler>>> = OnceLock::new();
    H.get_or_init(Default::default)
}

/// Counts the settings written through the guest agent channel, as the
/// worker writes them at every start of a Proxmox VM, a resume from a
/// hibernate included: each as the file is opened, empty, to be written.
fn secrets() -> &'static watch::Sender<u64> {
    static S: OnceLock<watch::Sender<u64>> = OnceLock::new();
    S.get_or_init(|| watch::channel(0).0)
}

/// Says settings have been written, as at a resume, when the link may be
/// dead although the guest has not heard so (the worker closed it while the
/// guest was frozen): the agent tries it at once (alive) rather than waiting
/// for TCP to find out.
pub fn settings_written() {
    secrets().send_modify(|n| *n += 1);
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
pub async fn run() {
    let mut wait = Duration::from_secs(1);
    let mut said = false;
    let mut written = secrets().subscribe();
    loop {
        // Read again at each dial: settings come through the guest agent
        // channel anew at each boot, with a secret of their own.
        let settings = settings().await;
        let hello = Hello {
            environment: settings.environment.clone(),
            secret: settings.secret.clone(),
            agent: crate::service::BUILD.to_string(),
        };
        written.borrow_and_update();
        match hangar_hub::connect(&settings.address, &settings.fingerprint, &hello).await {
            Ok((l, mut incoming)) => {
                wait = Duration::from_secs(1);
                said = false;
                link().send_replace(Some(l.clone()));
                loop {
                    let a = tokio::select! {
                        a = incoming.next() => a,
                        _ = written.changed() => {
                            let l = l.clone();
                            tokio::spawn(async move {
                                if !alive(&l).await {
                                    eprintln!("hangar-agent: hub: the link did not answer; dialling again");
                                    l.close();
                                }
                            });
                            continue;
                        }
                    };
                    let Some(a) = a else { break };
                    let l = l.clone();
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
                        l.relay(stream, here).await;
                    });
                }
                link().send_replace(None);
                eprintln!("hangar-agent: hub: the link dropped; dialling again");
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
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = written.changed() => {}
        }
        wait = (wait * 2).min(Duration::from_secs(10));
    }
}

/// A port the worker serves nothing on, and so answers by closing a
/// stream to it.
const PROBE_PORT: u32 = 0;

/// How long the worker has to answer a probe.
const PROBE_WITHIN: Duration = Duration::from_secs(3);

/// Whether the worker is still at the other end of a link: a stream to
/// PROBE_PORT is answered by its closing. Writing it on a link the worker
/// has closed draws TCP's reset, which ends the link anyway.
async fn alive(l: &Link) -> bool {
    use tokio::io::AsyncReadExt;
    let probe = async {
        let mut s = l.open(PROBE_PORT).await?;
        let mut b = [0u8; 1];
        s.read(&mut b).await
    };
    tokio::time::timeout(PROBE_WITHIN, probe)
        .await
        .is_ok_and(|r| r.is_ok())
}

/// Opens a stream to port on the host over the hub, as a connection.
pub async fn open(port: u32) -> io::Result<FdStream> {
    let (l, stream) = stream(port).await?;
    let (here, there) = pair()?;
    tokio::spawn(l.relay(stream, here));
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
                if let Ok((l, s)) = stream(SHARED_FILES_PORT).await {
                    l.relay(s, tcp).await;
                }
            });
        }
    }
}

/// Opens a stream to port on the host over the hub.
async fn stream(port: u32) -> io::Result<(Link, hangar_hub::Stream)> {
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
    let s = l.open(port).await?;
    Ok((l, s))
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
