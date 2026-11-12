//! The environment's desktop, served to the host as VNC on vsock port 8103
//! in the protocol `internal/desktop` defines.
//!
//! The desktop is the image's own: a labwc session, headless, that draws
//! into memory whether or not anyone is looking (hangar-desktop.service).
//! The VNC server is wayvnc, which attaches to that session as another
//! Wayland client -- it copies the screen with the screencopy protocol and
//! injects input with the virtual pointer and keyboard ones -- so it can
//! come and go without disturbing anything on the desktop. It is started
//! when the host first asks for the desktop, and again whenever it has
//! exited. It listens on a unix socket only the guest reaches, and asks for
//! no password: the control plane has already decided who may reach it.
//!
//! A connection starts with one JSON line, a [`Request`]. For "vnc" the
//! connection is then joined to wayvnc's socket as it stands, so the host
//! carries RFB without reading it; with no desktop it ends without a byte
//! written, which a viewer sees as a connection that ended before it began.
//! For "resize" the desktop is resized and one JSON line, a [`Reply`],
//! answers. The size is set here rather than by a viewer through RFB, so a
//! desktop shared by several viewers has one owner of its size: with
//! wlr-randr, through the wlroots output management protocol.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

use crate::launch::{self, Launched};
use crate::user::{self, User};

pub const PORT: u32 = 8103;

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Request {
    #[serde(default)]
    pub op: String,
    #[serde(default)]
    pub width: i64,
    #[serde(default)]
    pub height: i64,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Reply {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub err: String,
}

/// Limits on the desktop's size. Both sides are rounded down to even
/// numbers, which video encoders want.
const MIN_WIDTH: i64 = 640;
const MIN_HEIGHT: i64 = 400;
const MAX_WIDTH: i64 = 3840;
const MAX_HEIGHT: i64 = 2160;

/// A requested size fitted within the limits.
pub fn clamp_size(w: i64, h: i64) -> (i64, i64) {
    (
        w.clamp(MIN_WIDTH, MAX_WIDTH) & !1,
        h.clamp(MIN_HEIGHT, MAX_HEIGHT) & !1,
    )
}

const NO_DESKTOP: &str = "this environment has no desktop running";
const SOCKET_DIR: &str = "/run/hangar/desktop";
const LOG: &str = "/var/log/hangar-desktop-vnc.log";
/// The one output a wlroots compositor's headless backend makes.
const OUTPUT: &str = "HEADLESS-1";

/// The VNC server for the environment's user's desktop.
pub struct Server {
    running: Mutex<Option<Launched>>,
}

impl Server {
    pub fn new() -> Self {
        Self {
            running: Mutex::new(None),
        }
    }

    /// Answers one connection from the host.
    pub async fn serve<S: AsyncRead + AsyncWrite + Send + 'static>(&self, conn: S) {
        let (rd, mut wr) = tokio::io::split(conn);
        let mut rd = BufReader::new(rd);
        let mut line = Vec::new();
        if !matches!(rd.read_until(b'\n', &mut line).await, Ok(n) if n > 0 && line.ends_with(b"\n"))
        {
            return;
        }
        let Ok(req) = serde_json::from_slice::<Request>(&line) else {
            return;
        };
        match req.op.as_str() {
            "vnc" => {
                let socket = match self.ensure().await {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("hangar-agent-rs: starting the desktop's VNC server: {e}");
                        return;
                    }
                };
                if let Err(e) = launch::join(rd, wr, &socket).await {
                    eprintln!("hangar-agent-rs: connecting to the desktop's VNC server: {e}");
                }
            }
            "resize" => {
                let reply = match resize(req.width, req.height).await {
                    Ok(()) => Reply::default(),
                    Err(e) => Reply { err: e.to_string() },
                };
                let mut b = serde_json::to_vec(&reply).unwrap_or_default();
                b.push(b'\n');
                let _ = wr.write_all(&b).await;
            }
            _ => {}
        }
    }

    /// The VNC server's socket, starting it if it is not running.
    async fn ensure(&self) -> io::Result<PathBuf> {
        let mut running = self.running.lock().await;
        if let Some(l) = running.as_ref().filter(|l| l.running()) {
            return Ok(l.socket.clone());
        }
        let l = start().await?;
        let socket = l.socket.clone();
        *running = Some(l);
        Ok(socket)
    }
}

impl Default for Server {
    fn default() -> Self {
        Self::new()
    }
}

fn desktop_user() -> io::Result<User> {
    User::lookup(user::DEFAULT_USER)
        .ok_or_else(|| io::Error::other(format!("no user {:?}", user::DEFAULT_USER)))
}

fn runtime_dir(u: &User) -> PathBuf {
    PathBuf::from(format!("/run/user/{}", u.uid))
}

/// The compositor's socket in the user's runtime directory, if there is
/// one. The compositor takes the first free name, wayland-0 on a desktop
/// nothing else has started.
fn wayland_display(runtime_dir: &Path) -> Option<String> {
    use std::os::unix::fs::FileTypeExt;
    let mut names: Vec<String> = std::fs::read_dir(runtime_dir)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_socket()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("wayland-") && !n.ends_with(".lock"))
        .collect();
    names.sort();
    names.into_iter().next()
}

async fn start() -> io::Result<Launched> {
    let Some(wayvnc) = launch::look_path("wayvnc") else {
        return Err(io::Error::other(format!(
            "{NO_DESKTOP}: its image has no wayvnc"
        )));
    };
    let u = desktop_user()?;
    let runtime = runtime_dir(&u);
    // The compositor may still be starting just after boot; a headless
    // environment never starts it at all.
    let waiting = Instant::now();
    let display = loop {
        if let Some(d) = wayland_display(&runtime) {
            break d;
        }
        if waiting.elapsed() > launch::START_TIMEOUT {
            return Err(io::Error::other(NO_DESKTOP));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };

    let dir = Path::new(SOCKET_DIR);
    launch::socket_dir(dir, &u)?;
    let socket = dir.join("vnc.sock");
    // No configuration: wayvnc's defaults are no authentication, which is
    // what a socket only the agent reaches wants. An empty file stops it
    // reading the user's own.
    let config = dir.join("config");
    std::fs::write(&config, "")?;

    let mut cmd = Command::new(wayvnc);
    cmd.arg("--config")
        .arg(&config)
        .arg("--unix-socket")
        // Its own control socket, not the default in the user's runtime
        // directory, which a wayvnc the user starts would want.
        .arg("--socket")
        .arg(dir.join("ctl.sock"))
        // The desktop's size is Hangar's to set (resize), not a viewer's.
        .arg("--disable-resizing")
        // The desktop has no hardware cursor for a viewer to be sent, so
        // the pointer is drawn into the picture.
        .arg("--render-cursor")
        .arg(&socket)
        .current_dir(&u.home)
        .env_clear()
        .env("HOME", &u.home)
        .env("USER", &u.name)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("WAYLAND_DISPLAY", &display)
        .env("PATH", user::PATH)
        .envs(user::session_env());
    user::prepare(&mut cmd, Some(&u), user::oom::SERVICE);
    launch::start("the desktop's VNC server", cmd, socket, Path::new(LOG)).await
}

/// Sets the size of the desktop's output. Everyone watching sees the desktop
/// change size, as with a physical monitor's resolution.
async fn resize(width: i64, height: i64) -> io::Result<()> {
    let (width, height) = clamp_size(width, height);
    let u = desktop_user()?;
    let runtime = runtime_dir(&u);
    let display = wayland_display(&runtime).ok_or_else(|| io::Error::other(NO_DESKTOP))?;
    let mut cmd = Command::new("wlr-randr");
    cmd.arg("--output")
        .arg(OUTPUT)
        .arg("--custom-mode")
        .arg(format!("{width}x{height}"))
        .env_clear()
        .env("HOME", &u.home)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("WAYLAND_DISPLAY", &display)
        .env("PATH", user::PATH);
    user::prepare(&mut cmd, Some(&u), user::oom::USER);
    let out = tokio::process::Command::from(cmd).output().await?;
    if !out.status.success() {
        let mut said = out.stdout;
        said.extend_from_slice(&out.stderr);
        return Err(io::Error::other(format!(
            "resizing the desktop: {}: {}",
            out.status,
            String::from_utf8_lossy(&said)
        )));
    }
    Ok(())
}
