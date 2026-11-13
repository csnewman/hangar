//! Starting a backend: the monitor, passt, the GPU's renderer.

use std::collections::VecDeque;
use std::io;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::watch;

use crate::err;

/// A running backend.
pub struct Backend {
    pid: u32,
    exited: watch::Receiver<Option<ExitStatus>>,
}

impl Backend {
    /// The backend's process ID.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// How it exited, once it has.
    pub fn exited(&self) -> Option<ExitStatus> {
        *self.exited.borrow()
    }

    /// Sends it a signal, unless it has exited (and so its ID may be
    /// another's).
    pub fn signal(&self, sig: libc::c_int) -> io::Result<()> {
        let st = self.exited.borrow();
        if st.is_some() {
            return Ok(());
        }
        // SAFETY: kill has no memory preconditions; the process is not yet
        // reaped while exited is None, so the ID is still its own.
        if unsafe { libc::kill(self.pid as libc::pid_t, sig) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn kill(&self) {
        let _ = self.signal(libc::SIGKILL);
    }

    /// Waits for it to exit.
    pub async fn wait(&self) -> ExitStatus {
        let mut rx = self.exited.clone();
        let st = rx.wait_for(Option::is_some).await;
        st.ok().and_then(|s| *s).unwrap_or_else(|| {
            use std::os::unix::process::ExitStatusExt;
            ExitStatus::from_raw(0)
        })
    }

    /// Kills it and waits for it to be gone.
    pub async fn stop(&self) {
        self.kill();
        self.wait().await;
    }
}

/// Starts a backend and waits for it to create the socket the monitor will
/// connect to, or none.
///
/// A backend that exits instead is reported at once, with what it wrote to
/// stderr, rather than as a socket that never appeared. The usual cause is
/// a binary that does not understand its arguments -- an older build
/// earlier on PATH, say -- and its own message says so where a timeout
/// would not.
///
/// log, if given, also receives everything the backend writes, for the
/// life of the process: a backend's own account of what it saved and
/// restored is often the only one there is.
///
/// The backend is tied to this process: when it dies, however it dies, the
/// kernel kills the backend too (on Linux; macOS has no such signal). The
/// signal follows the thread that started the backend, which is one of the
/// runtime's workers, alive as long as the runtime. A backend that changes
/// its credentials loses it; passt does, and ends with its monitor
/// instead.
pub async fn launch(
    mut cmd: tokio::process::Command,
    name: &str,
    socket: Option<&Path>,
    log: Option<&Path>,
    timeout: Duration,
) -> io::Result<Backend> {
    cmd.stderr(Stdio::piped()).stdin(Stdio::null());
    #[cfg(target_os = "linux")]
    // SAFETY: prctl is async-signal-safe, as pre_exec requires.
    unsafe {
        cmd.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| err(format!("starting {name}: {e}")))?;
    let pid = child.id().unwrap_or(0);
    let tail = Arc::new(Mutex::new(VecDeque::<u8>::new()));
    let mut log_file = match log {
        Some(p) => tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .await
            .ok(),
        None => None,
    };
    // The output is copied through this process for as long as the
    // backend runs, so the log stays open until it exits.
    let mut stderr = child.stderr.take();
    {
        let tail = tail.clone();
        tokio::spawn(async move {
            let Some(stderr) = stderr.as_mut() else {
                return;
            };
            let mut buf = [0u8; 4096];
            while let Ok(n) = stderr.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                {
                    let mut t = tail.lock().unwrap();
                    t.extend(&buf[..n]);
                    let over = t.len().saturating_sub(4096);
                    t.drain(..over);
                }
                if let Some(f) = log_file.as_mut() {
                    let _ = f.write_all(&buf[..n]).await;
                }
            }
        });
    }
    // Exited is when the process is: what it started may hold its stderr
    // open longer.
    let (tx, exited) = watch::channel(None);
    tokio::spawn(async move {
        let st = child.wait().await.ok();
        let _ = tx.send(Some(st.unwrap_or_else(|| {
            use std::os::unix::process::ExitStatusExt;
            ExitStatus::from_raw(1)
        })));
    });
    let b = Backend { pid, exited };
    let Some(socket) = socket else {
        return Ok(b);
    };
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if socket.exists() {
            return Ok(b);
        }
        if let Some(st) = b.exited() {
            let msg =
                String::from_utf8_lossy(&tail.lock().unwrap().iter().copied().collect::<Vec<u8>>())
                    .trim()
                    .to_string();
            let mut e = format!("{name} exited before creating {}: {st}", socket.display());
            if !msg.is_empty() {
                e += ":\n";
                e += &msg;
            }
            return Err(err(e));
        }
        if tokio::time::Instant::now() >= deadline {
            b.kill();
            return Err(err(format!(
                "{name} did not create {} within {timeout:?}",
                socket.display()
            )));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
