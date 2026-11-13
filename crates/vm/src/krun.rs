//! hangar-krun, which runs one environment's machine under libkrun: the
//! monitor on macOS, where Cloud Hypervisor cannot run, and on Linux the
//! other to Cloud Hypervisor. The worker starts it with a Config as JSON;
//! libkrun takes the process over, and it exits when the guest does.
//!
//! libkrun is built from source with dev/krun-build.sh, which also builds
//! hangar-krun against it, and on macOS signs it with the hypervisor
//! entitlement macOS requires.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::ch::GpuProbe;
use crate::err;

fn is_zero(n: &u32) -> bool {
    *n == 0
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// The machine hangar-krun runs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub cpus: u32,
    /// The guest's RAM from boot. hotplug_mib, if set, is a virtio-mem
    /// region beyond it that memory is plugged into while the machine runs,
    /// plugged_mib of it to begin with.
    pub memory_mib: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub hotplug_mib: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub plugged_mib: u32,
    pub kernel: String,
    pub initrd: String,
    pub cmdline: String,
    /// The file the guest's console is written to.
    pub console: String,
    /// Attached in order, as /dev/vda, /dev/vdb and on.
    pub disks: Vec<Disk>,
    /// The vsock ports the guest connects out on, each to the unix socket
    /// the host listens on.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub host_ports: BTreeMap<u32, String>,
    /// The vsock ports the guest listens on, each reached by connecting to
    /// the unix socket libkrun listens on.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub guest_ports: BTreeMap<u32, String>,
    /// The unix socket carrying the guest's network, with its MAC address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net: Option<Net>,
    /// A virtio-gpu: Vulkan through Venus, and OpenGL through virgl where
    /// the host renders it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu: Option<Gpu>,
    /// The snapshot the machine resumes from, rather than booting: one
    /// hangar-krun wrote of a machine configured the same.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub restore: String,
}

/// The guest's virtual GPU.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gpu {
    /// Sizes the window the guest maps GPU memory through.
    pub window_mib: u32,
    /// Offers OpenGL as well: on Linux, where virglrenderer draws it with
    /// the host's OpenGL. The Mac has none for it to use.
    #[serde(default, skip_serializing_if = "is_false")]
    pub virgl: bool,
}

/// A virtio-blk device.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Disk {
    pub id: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub read_only: bool,
    /// Bypasses the host's own cache, for the swap disk.
    #[serde(default, skip_serializing_if = "is_false")]
    pub direct: bool,
}

/// The guest's network device.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Net {
    pub socket: String,
    pub mac: String,
    /// A unix stream socket, passt's on Linux; otherwise a datagram one,
    /// gvisor-tap-vsock's in vfkit mode, as on macOS.
    #[serde(default, skip_serializing_if = "is_false")]
    pub stream: bool,
}

/// What hangar-krun's libkrun was built with, and what it renders with.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Features {
    #[serde(default)]
    pub gpu: bool,
    /// The Vulkan driver a guest's GPU renders with on the Mac:
    /// VULKAN_KOSMICKRISP or VULKAN_MOLTENVK; empty without a GPU.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub vulkan: String,
    /// Whether a guest's OpenGL can be drawn on the GPU, by Mesa's OpenGL
    /// on Vulkan (zink): the driver can share images between the guest's
    /// programs, which dev/krun-build.sh's KosmicKrisp can.
    #[serde(default, skip_serializing_if = "is_false")]
    pub zink: bool,
}

/// LunarG's driver in Mesa, on Metal 4 (macOS 26 and later): conformant
/// Vulkan 1.4, robustness2 among it.
pub const VULKAN_KOSMICKRISP: &str = "kosmickrisp";
/// Khronos's, on any Metal.
pub const VULKAN_MOLTENVK: &str = "moltenvk";

/// Where hangar-krun is: on PATH, or what dev/krun-build.sh builds, from
/// the repository's root. Linux's is apart from the Mac's, which a Linux
/// machine sharing the checkout also sees.
pub fn find() -> io::Result<PathBuf> {
    if let Some(p) = std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .map(|d| d.join("hangar-krun"))
        .find(|p| p.is_file())
    {
        return Ok(p);
    }
    let out = if cfg!(target_os = "linux") {
        "krun-linux"
    } else {
        "krun"
    };
    let p = Path::new("out").join(out).join("bin/hangar-krun");
    if p.exists() {
        return std::path::absolute(p);
    }
    Err(err(format!(
        "hangar-krun not found on PATH or in {}: build it with dev/krun-build.sh",
        p.display()
    )))
}

/// A program built beside hangar-krun for a worker on macOS, such as
/// mkfs.ext4.
pub fn tool(name: &str) -> io::Result<PathBuf> {
    let bin = find()?;
    let p = bin.parent().unwrap_or(Path::new("/")).join(name);
    if !p.exists() {
        return Err(err(format!(
            "{name} is not beside hangar-krun: build it with dev/krun-build.sh"
        )));
    }
    Ok(p)
}

async fn ask(arg: &str, env: &[(String, String)], timeout: Duration) -> io::Result<Vec<u8>> {
    let bin = find()?;
    let out = tokio::time::timeout(
        timeout,
        tokio::process::Command::new(bin)
            .arg(arg)
            .envs(env.iter().map(|(k, v)| (k, v)))
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| {
        err(format!(
            "hangar-krun {arg} did not answer within {timeout:?}"
        ))
    })??;
    if !out.status.success() {
        return Err(err(format!("hangar-krun {arg}: {}", out.status)));
    }
    Ok(out.stdout)
}

/// What hangar-krun's libkrun was built with.
pub async fn read_features() -> io::Result<Features> {
    let out = ask("-features", &[], Duration::from_secs(30))
        .await
        .map_err(|e| err(format!("asking hangar-krun what it offers: {e}")))?;
    serde_json::from_slice(&out).map_err(|e| err(format!("hangar-krun's features: {e}")))
}

/// What the host renders with under env, as hangar-krun's libkrun's GPU
/// renders on Linux.
pub async fn probe_gpu(env: &[(String, String)]) -> io::Result<GpuProbe> {
    let out = ask("-probe", env, Duration::from_secs(30))
        .await
        .map_err(|e| err(format!("probing the GPU with hangar-krun: {e}")))?;
    serde_json::from_slice(&out).map_err(|e| err(format!("hangar-krun's GPU probe: {e}")))
}
