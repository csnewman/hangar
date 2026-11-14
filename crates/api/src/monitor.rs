//! What the worker and the programs that run its machines tell each other:
//! hangar-krun's configuration (krun.json) and what it was built with, and
//! what a GPU backend (hangar-gpu, hangar-krun) reports it renders with.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

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
    /// The socket's path, which the monitor connects to; empty with `fd`.
    pub socket: String,
    pub mac: String,
    /// A unix stream socket, passt's on Linux; otherwise a datagram one,
    /// gvisor-tap-vsock's in vfkit mode, as on macOS.
    #[serde(default, skip_serializing_if = "is_false")]
    pub stream: bool,
    /// A connected stream socket the monitor inherits as this descriptor,
    /// in place of a path to connect to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fd: Option<i32>,
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
