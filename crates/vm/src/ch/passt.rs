//! passt, a guest's network.

use std::io;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::OnceCell;

use super::launch::{Backend, launch};
use crate::err;

/// A running passt, giving one guest outbound networking.
pub struct Passt {
    backend: Backend,
    socket: Option<PathBuf>,
}

/// Keeps passt as root rather than changing to nobody.
const AS_ROOT: [&str; 2] = ["--runas", "0:0"];

/// Set once passt has been found unable to sandbox itself as nobody on this
/// host, so every later one starts as root.
static AS_ROOT_FOUND: AtomicBool = AtomicBool::new(false);

/// Whether passt can run as nobody here, decided once.
static PROBED: OnceCell<()> = OnceCell::const_new();

fn find_passt() -> io::Result<PathBuf> {
    std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .map(|d| d.join("passt"))
        .find(|p| p.is_file())
        .ok_or_else(|| err("passt not found (apt install passt)"))
}

fn remove_sockets(socket: &Path) {
    let _ = std::fs::remove_file(socket);
    let mut repair = socket.as_os_str().to_owned();
    repair.push(".repair");
    let _ = std::fs::remove_file(repair);
}

fn root() -> bool {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() == 0 }
}

impl Passt {
    /// Gives one guest a network through passt.
    ///
    /// passt translates the guest's traffic into ordinary host sockets, so
    /// the machine rides the host's normal networking with no TAP device
    /// and no NET_ADMIN. Cloud Hypervisor has no socket-based network
    /// backend, only tap, a tap file descriptor, or vhost-user -- so with
    /// vhost_user, vhost-user is how the two meet. Without it passt speaks
    /// its stream protocol on the socket, frames prefixed with their
    /// length, which is how libkrun reaches it.
    ///
    /// passt is the backend and owns the socket, so it must be listening
    /// before the monitor starts: the monitor connects as the client.
    ///
    /// One passt serves one machine. vhost-user is a point-to-point
    /// conversation about one guest's memory, so this is not a shared
    /// service.
    pub async fn start(socket: &Path, vhost_user: bool) -> io::Result<Passt> {
        let bin = find_passt()?;
        // passt also binds a second socket beside the one it is given, for
        // moving TCP connections during a migration. Both have to go: one
        // left by a passt that was killed makes the next refuse to start.
        remove_sockets(socket);
        let dir = socket.parent().unwrap_or(Path::new("/"));
        std::fs::create_dir_all(dir)?;
        PROBED.get_or_init(|| probe(&bin)).await;

        let mut args: Vec<&str> = vec![
            "--socket-path",
            socket
                .to_str()
                .ok_or_else(|| err("a socket path that is not UTF-8"))?,
            // Stay in the foreground so this process supervises it rather
            // than losing track of a daemon that forked away.
            "--foreground",
            // Exit when the monitor disconnects. passt clears the
            // parent-death signal as it drops its privileges, so this is
            // what ends it when the process that started it dies uncleanly:
            // the monitor dies with that process, and passt follows. Every
            // boot and resume starts a passt of its own, so one monitor is
            // all it ever serves.
            "--one-off",
            "--quiet",
        ];
        if vhost_user {
            args.push("--vhost-user");
        }
        let start = |as_root: bool| {
            let mut cmd = tokio::process::Command::new(&bin);
            cmd.args(&args);
            if as_root {
                cmd.args(AS_ROOT);
            }
            launch(cmd, "passt", Some(socket), None, Duration::from_secs(10))
        };
        let as_root = AS_ROOT_FOUND.load(Ordering::Relaxed);
        let backend = match start(as_root).await {
            Err(e) if !as_root && root() && e.to_string().contains("Failed to sandbox") => {
                // Started as root, passt changes to nobody and then
                // sandboxes itself in new namespaces, a user namespace among
                // them. Ubuntu refuses an unprivileged process one unless its
                // AppArmor profile allows it
                // (kernel.apparmor_restrict_unprivileged_userns), which
                // passt's own profile does, but not in a container with
                // AppArmor turned off, as the worker's is. Kept as root, the
                // process that makes the namespaces has the capabilities to,
                // and passt is still sandboxed in them.
                AS_ROOT_FOUND.store(true, Ordering::Relaxed);
                remove_sockets(socket);
                start(true).await?
            }
            r => r?,
        };
        Ok(Passt {
            backend,
            socket: Some(socket.to_path_buf()),
        })
    }

    /// Gives one libkrun machine a network through passt, over a connected
    /// pair of sockets: passt has one, speaking its stream protocol on it,
    /// and the other, returned, is the monitor's.
    ///
    /// passt accepts nothing on a connected socket. One listening on a path
    /// accepts its monitor after it has sandboxed itself, when the path is
    /// outside its mount namespace, and an AppArmor that mediates unix
    /// sockets by path refuses that (Ubuntu's 7.0 kernels, under passt's
    /// own profile): the monitor's frames then wait in the socket, and the
    /// guest never has a network.
    ///
    /// passt exits when the monitor's end closes.
    pub async fn start_connected() -> io::Result<(Passt, OwnedFd)> {
        let bin = find_passt()?;
        PROBED.get_or_init(|| probe(&bin)).await;
        let (ours, theirs) = socket_pair()?;
        let mut cmd = tokio::process::Command::new(&bin);
        // Stay in the foreground so this process supervises it rather than
        // losing track of a daemon that forked away.
        cmd.args(["--fd", "3", "--foreground", "--quiet"]);
        if AS_ROOT_FOUND.load(Ordering::Relaxed) {
            cmd.args(AS_ROOT);
        }
        inherit_as_3(&mut cmd, theirs.as_raw_fd());
        let backend = launch(cmd, "passt", None, None, Duration::from_secs(10)).await?;
        drop(theirs);
        Ok((
            Passt {
                backend,
                socket: None,
            },
            ours,
        ))
    }

    /// The path the monitor connects to, for a passt started on one.
    pub fn socket(&self) -> Option<&Path> {
        self.socket.as_deref()
    }

    /// The backend's process ID.
    pub fn pid(&self) -> u32 {
        self.backend.pid()
    }

    /// Stops passt and removes its socket.
    pub async fn close(&self) {
        self.backend.stop().await;
        if let Some(s) = &self.socket {
            remove_sockets(s);
        }
    }
}

/// A connected pair of stream sockets, closed on exec.
fn socket_pair() -> io::Result<(OwnedFd, OwnedFd)> {
    let (a, b) = std::os::unix::net::UnixStream::pair()?;
    Ok((a.into(), b.into()))
}

/// Has cmd's process inherit fd as its descriptor 3, the one passt's
/// `--fd 3` names. fd stays open until cmd is spawned.
fn inherit_as_3(cmd: &mut tokio::process::Command, fd: RawFd) {
    // SAFETY: the closure calls only async-signal-safe functions.
    unsafe {
        cmd.pre_exec(move || {
            // dup2 leaves the copy open across exec; a descriptor that is 3
            // already has its flag cleared instead.
            if fd == 3 {
                libc::fcntl(3, libc::F_SETFD, 0);
            } else if libc::dup2(fd, 3) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// Finds out whether passt can sandbox itself as nobody on this host, and
/// if it cannot, has every passt run as root. A passt listening on a path
/// binds it before sandboxing, then exits when that fails, leaving a socket
/// nothing answers, and the monitor waits on it for a minute; one given a
/// connected socket fails as quietly. So a passt is started on a socket of
/// its own and watched until it has had time to sandbox itself.
async fn probe(bin: &Path) {
    if !root() {
        return;
    }
    let Ok((ours, theirs)) = socket_pair() else {
        return;
    };
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(["--fd", "3", "--foreground"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    inherit_as_3(&mut cmd, theirs.as_raw_fd());
    let child = cmd.spawn();
    drop(theirs);
    if let Ok(child) = child {
        // Exited within a second, it said why; still running, it has
        // sandboxed itself, and is killed as it is dropped.
        if let Ok(Ok(out)) =
            tokio::time::timeout(Duration::from_secs(1), child.wait_with_output()).await
            && String::from_utf8_lossy(&out.stderr).contains("Failed to sandbox")
        {
            AS_ROOT_FOUND.store(true, Ordering::Relaxed);
        }
    }
    drop(ours);
}
