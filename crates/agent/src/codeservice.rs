//! The Code tab's service, started for the host on vsock port 104 in the
//! protocol `internal/code` defines.
//!
//! The service is this binary's `code` command (`src/code`), run as the
//! environment's user, one per root folder, on a unix socket. A connection
//! starts with one JSON line, a [`Request`] naming the root; after it the
//! connection is joined to that root's service as it stands, the host
//! carrying the service's frames without reading them.

use std::collections::HashMap;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, BufReader};
use tokio::sync::Mutex;

use crate::launch::{self, Launched};
use crate::user::{self, User};

pub const PORT: u32 = 104;

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Request {
    /// The folder the Code tab shows; the user's home if none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub root: String,
}

/// The folders whose code the service may run, written by the worker at each
/// boot: a language server starts only in one of them, or in a folder the
/// person trusts from the Code tab.
const TRUST_FILE: &str = "/run/hangar/trusted-folders.json";
const SOCKET_DIR: &str = "/run/hangar/code";
const LOG: &str = "/var/log/hangar-code.log";

/// The service for the environment's user, a process per root.
pub struct Server {
    running: Mutex<HashMap<PathBuf, Launched>>,
    started: AtomicU64,
}

impl Server {
    pub fn new() -> Self {
        Self {
            running: Mutex::new(HashMap::new()),
            started: AtomicU64::new(0),
        }
    }

    /// Answers one connection from the host.
    pub async fn serve<S: AsyncRead + AsyncWrite + Send + 'static>(&self, conn: S) {
        let (rd, wr) = tokio::io::split(conn);
        let mut rd = BufReader::new(rd);
        let mut line = Vec::new();
        if !matches!(rd.read_until(b'\n', &mut line).await, Ok(n) if n > 0 && line.ends_with(b"\n"))
        {
            return;
        }
        let Ok(req) = serde_json::from_slice::<Request>(&line) else {
            return;
        };
        let socket = match self.ensure(&req.root).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "hangar-agent: starting the code service for {:?}: {e}",
                    req.root
                );
                return;
            }
        };
        if let Err(e) = launch::join(rd, wr, &socket).await {
            eprintln!("hangar-agent: connecting to the code service: {e}");
        }
    }

    async fn ensure(&self, root: &str) -> io::Result<PathBuf> {
        let u = User::lookup(user::DEFAULT_USER)
            .ok_or_else(|| io::Error::other(format!("no user {:?}", user::DEFAULT_USER)))?;
        let root = if root.starts_with('/') {
            clean(Path::new(root))
        } else {
            u.home.clone()
        };
        let mut running = self.running.lock().await;
        if let Some(l) = running.get(&root).filter(|l| l.running()) {
            return Ok(l.socket.clone());
        }
        let l = self.start(&u, &root).await?;
        let socket = l.socket.clone();
        running.insert(root, l);
        Ok(socket)
    }

    async fn start(&self, u: &User, root: &Path) -> io::Result<Launched> {
        let dir = Path::new(SOCKET_DIR);
        launch::socket_dir(dir, u)?;
        let n = self.started.fetch_add(1, Ordering::Relaxed);
        let socket = dir.join(format!("{n}.sock"));
        let exe = std::env::current_exe()?;
        let mut cmd = Command::new(exe);
        cmd.arg("code")
            .arg("--socket")
            .arg(&socket)
            .arg("--root")
            .arg(root)
            .arg("--trust")
            .arg(TRUST_FILE)
            .current_dir(&u.home)
            .env_clear()
            .env("HOME", &u.home)
            .env("SSH_AUTH_SOCK", user::SSH_AUTH_SOCK)
            .env("USER", &u.name)
            .env("LOGNAME", &u.name)
            .env("PATH", user::PATH)
            .env("LANG", "C.UTF-8")
            .envs(user::session_env());
        user::prepare(&mut cmd, Some(u), user::oom::SERVICE);
        launch::start("the code service", cmd, socket, Path::new(LOG)).await
    }
}

impl Default for Server {
    fn default() -> Self {
        Self::new()
    }
}

/// An absolute path with `.`, `..` and repeated slashes taken out, as
/// written, without looking at the files.
fn clean(p: &Path) -> PathBuf {
    let mut out = PathBuf::from("/");
    for c in p.components() {
        match c {
            Component::Normal(n) => out.push(n),
            Component::ParentDir => {
                out.pop();
            }
            _ => {}
        }
    }
    out
}
