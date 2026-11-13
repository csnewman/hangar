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
    /// For a tethered backend, the pipe its watcher waits on: closed, by
    /// stop, by dropping the backend, or by the kernel as this process
    /// dies, the watcher stops the backend.
    tether: Mutex<Option<tokio::process::ChildStdin>>,
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

    /// Kills it and waits for it to be gone. A tethered backend's watcher
    /// is let go first, while the backend's ID is still its own.
    pub async fn stop(&self) {
        self.tether.lock().unwrap().take();
        self.kill();
        self.wait().await;
    }
}

/// Starts a program as a tethered backend: `sh` keeps the pipe it is given
/// as stdin on a watcher it forks, then becomes the program, which so keeps
/// the ID the backend is known by. When the pipe closes the watcher sends
/// that ID SIGTERM, if its command line still names the program. It is for macOS, where
/// nothing ends a process with the one that started it: a backend left by a
/// worker that died would otherwise run on.
pub fn tethered(bin: &Path) -> tokio::process::Command {
    const WATCH: &str = r#"bin=$1
exec 3<&0 </dev/null
(
  read -r line <&3
  case "$(ps -o command= -p $$ 2>/dev/null)" in
    *"$bin"*) kill $$ ;;
  esac
) >/dev/null 2>&1 &
exec "$@" 3<&-"#;
    let mut cmd = tokio::process::Command::new("/bin/sh");
    cmd.arg("-c").arg(WATCH).arg("sh").arg(bin);
    cmd
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
/// kernel kills the backend too (on Linux; macOS has no such signal, and a
/// backend there is started from `tethered` and by `launch_tethered`). The
/// signal follows the thread that started the backend, which is one of the
/// runtime's workers, alive as long as the runtime. A backend that changes
/// its credentials loses it; passt does, and ends with its monitor
/// instead.
pub async fn launch(
    cmd: tokio::process::Command,
    name: &str,
    socket: Option<&Path>,
    log: Option<&Path>,
    timeout: Duration,
) -> io::Result<Backend> {
    start(cmd, name, socket, log, timeout, false).await
}

/// launch for a command made by `tethered`: its stdin is the pipe its
/// watcher waits on, which the backend holds.
pub async fn launch_tethered(
    cmd: tokio::process::Command,
    name: &str,
    socket: Option<&Path>,
    log: Option<&Path>,
    timeout: Duration,
) -> io::Result<Backend> {
    start(cmd, name, socket, log, timeout, true).await
}

async fn start(
    mut cmd: tokio::process::Command,
    name: &str,
    socket: Option<&Path>,
    log: Option<&Path>,
    timeout: Duration,
    tether: bool,
) -> io::Result<Backend> {
    cmd.stderr(Stdio::piped()).stdin(if tether {
        Stdio::piped()
    } else {
        Stdio::null()
    });
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
    let tether = Mutex::new(child.stdin.take());
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
    let b = Backend {
        pid,
        exited,
        tether,
    };
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
