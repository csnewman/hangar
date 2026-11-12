//! Services the agent starts for the environment's user when the host first
//! asks for one, each on a unix socket only the guest reaches, and joins
//! each of the host's connections to: the Code tab's service and the
//! desktop's VNC server.

use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::UnixStream;

use crate::user::User;

/// How long a service has to start: covers the desktop's compositor still
/// coming up just after boot.
pub const START_TIMEOUT: Duration = Duration::from_secs(30);

/// A service that was started, on its socket.
pub struct Launched {
    pub socket: PathBuf,
    exited: Arc<AtomicBool>,
}

impl Launched {
    pub fn running(&self) -> bool {
        !self.exited.load(Ordering::Relaxed)
    }
}

/// Makes `dir`, where services' sockets are, the user's alone.
pub fn socket_dir(dir: &Path, user: &User) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    std::os::unix::fs::chown(dir, Some(user.uid), Some(user.gid))
}

/// Starts `cmd`, a service called `name` that listens on `socket`, writing
/// what it prints to `log`, and waits up to [`START_TIMEOUT`] for the socket
/// to answer.
pub async fn start(
    name: &'static str,
    mut cmd: Command,
    socket: PathBuf,
    log: &Path,
) -> io::Result<Launched> {
    let _ = fs::remove_file(&socket);
    let out = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o644)
        .open(log)?;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(out.try_clone()?))
        .stderr(Stdio::from(out));
    let mut child = tokio::process::Command::from(cmd)
        .spawn()
        .map_err(|e| io::Error::new(e.kind(), format!("starting {name}: {e}")))?;
    eprintln!("hangar-agent: {name} started: {}", child.id().unwrap_or(0));
    let exited = Arc::new(AtomicBool::new(false));
    let gone = exited.clone();
    let (stop_tx, mut stop) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        // Killed only when told: the sender going, once the service has
        // started, leaves it running.
        let status = tokio::select! {
            s = child.wait() => s,
            Ok(()) = &mut stop => {
                let _ = child.kill().await;
                child.wait().await
            }
        };
        eprintln!("hangar-agent: {name} exited: {status:?}");
        gone.store(true, Ordering::Relaxed);
    });

    let started = Instant::now();
    loop {
        if UnixStream::connect(&socket).await.is_ok() {
            return Ok(Launched { socket, exited });
        }
        if exited.load(Ordering::Relaxed) {
            return Err(io::Error::other(format!(
                "{name} exited while starting; see {}",
                log.display()
            )));
        }
        if started.elapsed() > START_TIMEOUT {
            let _ = stop_tx.send(());
            return Err(io::Error::other(format!(
                "{name} did not start in {}s; see {}",
                START_TIMEOUT.as_secs(),
                log.display()
            )));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Carries bytes between a connection and the service on `socket`, as they
/// stand, until either side closes.
pub async fn join<R, W>(mut rd: R, mut wr: W, socket: &Path) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let upstream = UnixStream::connect(socket).await?;
    let (mut ur, mut uw) = upstream.into_split();
    tokio::select! {
        _ = tokio::io::copy(&mut rd, &mut uw) => {}
        _ = tokio::io::copy(&mut ur, &mut wr) => {}
    }
    Ok(())
}

/// Where a program is on PATH, or the usual places when there is none.
pub fn look_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var("PATH").unwrap_or_else(|_| crate::user::PATH.to_string());
    path.split(':')
        .map(|d| Path::new(d).join(name))
        .find(|p| fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
}
