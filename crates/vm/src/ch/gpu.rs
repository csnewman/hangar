//! hangar-gpu: the renderer behind a Cloud Hypervisor guest's virtio-gpu.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::launch::{Backend, launch};
use super::state::{log_path_for, ready_path, save_state, wait_file};
use crate::err;

/// A running hangar-gpu, rendering for one guest.
pub struct GpuBackend {
    backend: Backend,
    socket: PathBuf,
    state: Option<PathBuf>,
}

/// Where the renderer lives: on PATH, a local build during development, or
/// where the published image installs it.
pub fn find_gpu_backend() -> io::Result<PathBuf> {
    if let Some(p) = std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .map(|d| d.join("hangar-gpu"))
        .find(|p| p.is_file())
    {
        return Ok(p);
    }
    const SEARCH: [&str; 2] = ["gpu/target/release/hangar-gpu", "/usr/local/bin/hangar-gpu"];
    for p in SEARCH {
        if Path::new(p).exists() {
            return std::path::absolute(p);
        }
    }
    Err(err(format!(
        "hangar-gpu not found on PATH or in {SEARCH:?}"
    )))
}

impl GpuBackend {
    /// Renders for one guest from a process of its own.
    ///
    /// The renderer links native graphics libraries whose syscall surface
    /// is not ours to enumerate, and loads whichever driver the host has at
    /// run time. Keeping it out of the monitor means that surface, and any
    /// crash in it, belongs to a process holding no guest memory it was not
    /// handed.
    ///
    /// It owns the socket and must be listening before the monitor starts,
    /// which connects to it as a client.
    ///
    /// state is where the objects the guest holds are recorded; see
    /// save_state. env is what gpu_env gives for the renderer and device
    /// the worker chose. venus_restore has the renderer carry Venus
    /// contexts across a suspend; without it a Venus program is told at
    /// once that its device is gone.
    pub async fn start(
        socket: &Path,
        venus: bool,
        venus_restore: bool,
        state: Option<&Path>,
        env: &[(String, String)],
    ) -> io::Result<GpuBackend> {
        let bin = find_gpu_backend()?;
        let _ = std::fs::remove_file(socket);
        std::fs::create_dir_all(socket.parent().unwrap_or(Path::new("/")))?;
        let mut cmd = tokio::process::Command::new(bin);
        cmd.arg("--socket").arg(socket);
        if venus {
            cmd.args(["--venus", "true"]);
        }
        if let Some(state) = state {
            cmd.arg("--state").arg(state);
        }
        if venus_restore {
            cmd.arg("--venus-restore");
        }
        cmd.envs(env.iter().map(|(k, v)| (k, v)));
        let log = state.map(log_path_for);
        let backend = launch(
            cmd,
            "the gpu backend",
            Some(socket),
            log.as_deref(),
            Duration::from_secs(15),
        )
        .await?;
        Ok(GpuBackend {
            backend,
            socket: socket.to_path_buf(),
            state: state.map(Path::to_path_buf),
        })
    }

    /// The path the monitor connects to.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// The backend's process ID; the processes it starts, such as Venus's
    /// render server, are its children.
    pub fn pid(&self) -> u32 {
        self.backend.pid()
    }

    /// Records the objects the guest holds.
    ///
    /// Nothing behind them can be carried across -- a renderer's state is
    /// not readable -- so this is what lets a restored backend tell the
    /// guest precisely which of its objects are gone, rather than answer as
    /// if it had invented them.
    pub async fn save_state(&self, timeout: Duration) -> io::Result<()> {
        let Some(state) = &self.state else {
            return Err(err("the gpu backend was started without a state file"));
        };
        save_state(&self.backend, state, timeout).await
    }

    /// Stops the backend and removes its socket.
    pub async fn close(&self) {
        self.backend.stop().await;
        let _ = std::fs::remove_file(&self.socket);
    }

    /// Waits until a restored session has been built again in the
    /// renderer: every resource with its contents, and every context's
    /// objects and bindings. The guest must not run before then, since the
    /// first thing a program mid-render does is use them.
    pub async fn wait_restored(&self, timeout: Duration) -> io::Result<()> {
        let Some(state) = &self.state else {
            return Err(err("the gpu backend was started without a state file"));
        };
        wait_file(&ready_path(state), timeout).await
    }
}

/// The renderers a worker may ask its GPU backends for: whatever Mesa finds
/// (a GPU if there is one, the CPU if not), a GPU only, or the CPU only.
pub const RENDERER_AUTO: &str = "auto";
pub const RENDERER_HARDWARE: &str = "hardware";
pub const RENDERER_SOFTWARE: &str = "software";

/// The environment a GPU backend runs with to render as asked: on the
/// surfaceless platform always, which a host without /dev/dri needs; on
/// the CPU for software, through Mesa's llvmpipe and lavapipe; and on
/// device, a render node such as /dev/dri/renderD129, when one is named.
pub fn gpu_env(renderer: &str, device: &str) -> io::Result<Vec<(String, String)>> {
    let kv = |k: &str, v: &str| (k.to_string(), v.to_string());
    let mut env = vec![kv("EGL_PLATFORM", "surfaceless")];
    match renderer {
        "" | RENDERER_AUTO | RENDERER_HARDWARE => {}
        RENDERER_SOFTWARE => {
            if !device.is_empty() {
                return Err(err(
                    "a device is for rendering on a GPU, and the renderer is software",
                ));
            }
            env.push(kv("LIBGL_ALWAYS_SOFTWARE", "1"));
            if let Some(icd) = lavapipe_icd() {
                env.push(kv("VK_DRIVER_FILES", &icd.to_string_lossy()));
            }
            return Ok(env);
        }
        other => {
            return Err(err(format!(
                "renderer {other:?}: want auto, hardware or software"
            )));
        }
    }
    if !device.is_empty() {
        env.extend(device_selection(device)?);
    }
    Ok(env)
}

/// lavapipe's Vulkan driver manifest, where the host's packages put it.
fn lavapipe_icd() -> Option<PathBuf> {
    for dir in [
        "/usr/share/vulkan/icd.d",
        "/usr/local/share/vulkan/icd.d",
        "/etc/vulkan/icd.d",
    ] {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut found: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("lvp_icd") && n.ends_with(".json"))
            })
            .collect();
        found.sort();
        if let Some(p) = found.into_iter().next() {
            return Some(p);
        }
    }
    None
}

/// Points Mesa at one GPU, for OpenGL by its PCI address (DRI_PRIME) and
/// for Vulkan by its vendor and device IDs, with the others hidden
/// (MESA_VK_DEVICE_SELECT's "!").
fn device_selection(device: &str) -> io::Result<Vec<(String, String)>> {
    let node = Path::new(device).file_name().unwrap_or_default();
    let sys = Path::new("/sys/class/drm").join(node).join("device");
    let target =
        std::fs::canonicalize(&sys).map_err(|e| err(format!("GPU device {device}: {e}")))?;
    let read = |name: &str| {
        std::fs::read_to_string(target.join(name)).map(|s| {
            let s = s.trim();
            s.strip_prefix("0x").unwrap_or(s).to_string()
        })
    };
    let vendor =
        read("vendor").map_err(|e| err(format!("GPU device {device} is not a PCI device: {e}")))?;
    let id = read("device").map_err(|e| err(format!("GPU device {device}: {e}")))?;
    // A PCI address such as 0000:02:00.0, written as Mesa's id_path_tag.
    let addr = target
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .replace([':', '.'], "_");
    Ok(vec![
        ("DRI_PRIME".into(), format!("pci-{addr}")),
        ("MESA_VK_DEVICE_SELECT".into(), format!("{vendor}:{id}!")),
    ])
}

/// What the GPU backend, or hangar-krun, reports it renders with, under
/// the environment it was given.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GpuProbe {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gl: Option<ProbedGl>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub gl_error: String,
    #[serde(default)]
    pub vulkan: Option<Vec<ProbedVulkan>>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub vulkan_error: String,
}

/// The OpenGL renderer a surfaceless EGL context gets.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProbedGl {
    pub vendor: String,
    pub renderer: String,
    pub version: String,
    /// Whether it renders on the CPU: Mesa's llvmpipe or softpipe.
    pub software: bool,
}

/// A Vulkan device.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProbedVulkan {
    pub name: String,
    /// Whether it is a CPU device, such as lavapipe.
    pub software: bool,
}

/// Asks the GPU backend what it renders with, under env.
pub async fn probe_gpu(env: &[(String, String)]) -> io::Result<GpuProbe> {
    let bin = find_gpu_backend()?;
    let out = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new(bin)
            .arg("--probe")
            .envs(env.iter().map(|(k, v)| (k, v)))
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| err("probing the GPU backend: it did not answer within 30s"))?
    .map_err(|e| err(format!("probing the GPU backend: {e}")))?;
    if !out.status.success() {
        return Err(err(format!("probing the GPU backend: {}", out.status)));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| err(format!("the GPU backend's probe: {e}")))
}
