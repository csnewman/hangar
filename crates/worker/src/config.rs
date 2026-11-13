//! A worker's YAML file. It holds only what a worker cannot be told by the
//! control plane: how to reach it, and facts about this machine. If two
//! workers could legitimately disagree about a setting it belongs here;
//! anything else is a property of an environment and lives in the database.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::io;
use std::path::Path;

use serde::Deserialize;
use serde::de::{self, Visitor};

/// Where a worker keeps its credential when its file does not say.
pub const DEFAULT_CREDENTIAL_FILE: &str = "/var/lib/hangar/worker-credential";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: Server,
    pub node: Node,
    pub storage: Storage,
    /// Capacity kept back for the host itself.
    pub reserved: Reserved,
    /// Multiplies what this machine offers, once Reserved is taken off;
    /// unset is 1, offering what there is. vCPUs are time-sliced, so more
    /// of them than cores only slows guests when they are busy at once.
    /// Memory beyond the machine's is for guests that leave theirs unused:
    /// the balloon hands a guest's free memory back to the host, and swap
    /// takes what is left, slowly, so memory overcommit wants swap to cover
    /// it.
    pub overcommit: Overcommit,
    /// What runs environments on this machine: "cloud-hypervisor" runs each
    /// as a virtual machine on Linux, and "krun" under libkrun, on Linux or
    /// macOS.
    pub runtime: String,
    /// Configures the cloud-hypervisor and krun runtimes.
    pub vm: VmConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Server {
    pub url: String,
    /// Holds the shared bootstrap token, exchanged once for a credential of
    /// this worker's own.
    pub token_file: String,
    /// Where the issued credential is kept. It is the worker's identity:
    /// lose it and an operator must remove the worker before its name can
    /// register again.
    pub credential_file: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Node {
    /// Defaults to the hostname.
    pub name: String,
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Storage {
    pub images: String,
    pub environments: String,
    pub caches: String,
    /// Holds the files environments share with others -- their owners'
    /// profiles and packs -- each file set a directory named by its ID,
    /// which each environment's NFS server serves it from. It is the same
    /// on every worker: a mount of one NFS share, or EFS. Empty serves
    /// none.
    pub files: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Reserved {
    pub cpus: i64,
    pub memory: Size,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Overcommit {
    pub cpus: f64,
    pub memory: f64,
}

/// How this machine runs environments as virtual machines. The files named
/// here are the node's, not the environment's: one kernel and one agent
/// serve every environment, whatever its image.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VmConfig {
    pub kernel: String,
    /// A hangar-agent built for the guest's architecture, static against
    /// musl. It is each environment's initramfs, as init, and then its
    /// agent.
    pub agent: String,
    /// An environment's room for its files and for Docker's store: one
    /// sparse disk of their sum.
    pub upper_gib: u64,
    pub docker_gib: u64,
    /// An environment's swap, as a multiple of its memory: a sparse disk the
    /// guest swaps to, through a compressed cache in its own memory, what
    /// its programs leave untouched for a while. Unset is 1; 0 gives none.
    pub swap: Option<f64>,
    /// How an environment's image reaches it: "disk" (the default), an
    /// EROFS image as a read-only virtio-blk disk; "pmem", the same image as
    /// virtio-pmem, mapped with DAX for an environment that asks.
    pub image_device: String,
    /// What an environment with a virtual GPU gets on this machine.
    pub gpu: VmGpu,
    /// Maps an image reference a template may name to a local copy this
    /// machine takes it from into its store. Every other reference is
    /// pulled from its registry.
    pub images: HashMap<String, VmImage>,
    /// Signs pulls in to registries, by host (ghcr.io,
    /// registry-1.docker.io, ...). Others are pulled from anonymously.
    pub registries: HashMap<String, RegistryAuth>,
}

/// How this machine gives environments a virtual GPU. Which of them can be
/// offered depends on the host's renderer, so it is the machine's to say
/// rather than a template's.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VmGpu {
    /// Offers Vulkan through the host's renderer as well as OpenGL. It needs
    /// a virglrenderer built with Venus.
    pub venus: bool,
    /// Carries a program's Vulkan state across a suspend, by recording what
    /// replaying it takes. Without it a Vulkan program ends on resume, as it
    /// would on losing a real GPU. Experimental.
    pub venus_restore: bool,
    /// Sizes the window the guest maps GPU resources into, which Venus and
    /// zero-copy buffers need. 0 is 512.
    pub window_mib: u32,
    /// What renders: "auto", a GPU if the host has one and its CPU if not;
    /// "hardware", a GPU only, offering no virtual GPU without one; or
    /// "software", the CPU always, through Mesa's llvmpipe and lavapipe.
    /// Empty is auto.
    pub renderer: String,
    /// The GPU to render on, as its render node, for a host with more than
    /// one: /dev/dri/renderD129. Empty is Mesa's choice.
    pub device: String,
}

/// A local copy of an image: its root filesystem, a directory, which
/// environments boot from.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VmImage {
    pub base: String,
}

/// How this machine signs in to a registry.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RegistryAuth {
    pub username: String,
    /// Holds the password or token, so it stays out of this file.
    pub password_file: String,
}

impl Config {
    /// Reads and checks a worker's YAML file.
    pub fn load(path: &Path) -> io::Result<Config> {
        let text = std::fs::read_to_string(path)?;
        Config::parse(&text).map_err(|e| io::Error::other(format!("{}: {e}", path.display())))
    }

    /// Reads and checks a worker's configuration, as its YAML file holds it.
    pub fn parse(text: &str) -> Result<Config, String> {
        let mut c: Config = if text.trim().is_empty() {
            Config::default()
        } else {
            serde_saphyr::from_str(text).map_err(|e| e.to_string())?
        };
        if c.server.url.is_empty() {
            return Err("server.url is required".into());
        }
        c.server.url = c.server.url.trim_end_matches('/').to_string();
        if c.server.credential_file.is_empty() {
            c.server.credential_file = DEFAULT_CREDENTIAL_FILE.into();
        }
        if c.node.name.is_empty() {
            c.node.name = hostname()
                .ok_or("node.name is unset and the hostname is unavailable".to_string())?;
        }
        let bad = |r: f64| r < 0.0 || (r > 0.0 && r < 1.0);
        if bad(c.overcommit.cpus) || bad(c.overcommit.memory) {
            return Err("overcommit ratios are 1 or more; reserved keeps capacity back".into());
        }
        match c.vm.swap {
            None => c.vm.swap = Some(1.0),
            Some(s) if s < 0.0 => {
                return Err("vm.swap is a multiple of an environment's memory, 0 or more".into());
            }
            Some(_) => {}
        }
        match c.runtime.as_str() {
            "" => return Err("runtime is required".into()),
            "cloud-hypervisor" | "krun" => {
                if c.vm.kernel.is_empty() {
                    return Err(format!(
                        "vm.kernel is required for the {} runtime",
                        c.runtime
                    ));
                }
                if c.storage.environments.is_empty() || c.storage.images.is_empty() {
                    return Err(format!(
                        "storage.environments and storage.images are required for the {} runtime",
                        c.runtime
                    ));
                }
            }
            _ => {}
        }
        Ok(c)
    }
}

fn hostname() -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: gethostname writes at most buf.len() bytes into buf.
    let r = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if r != 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let name = String::from_utf8_lossy(&buf[..end]).into_owned();
    (!name.is_empty()).then_some(name)
}

/// An amount of memory in bytes, written in YAML as a plain number of bytes
/// or with a binary suffix: 512MiB, 4GiB.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Size(pub u64);

impl Size {
    /// The size in whole mebibytes, rounded down.
    pub fn mib(self) -> u64 {
        self.0 >> 20
    }

    /// Reads a size such as "4GiB", "512MiB" or "1073741824".
    pub fn parse(v: &str) -> Result<Size, String> {
        let v = v.trim();
        let mut num = v;
        let mut mult = 1u64;
        for (suffix, m) in [
            ("KiB", 1u64 << 10),
            ("MiB", 1 << 20),
            ("GiB", 1 << 30),
            ("TiB", 1 << 40),
        ] {
            if let Some(n) = v.strip_suffix(suffix) {
                num = n.trim();
                mult = m;
                break;
            }
        }
        let bad =
            || format!("invalid size {num:?}: want a number of bytes, or KiB, MiB, GiB or TiB");
        let n: u64 = num.parse().map_err(|_| bad())?;
        n.checked_mul(mult).map(Size).ok_or_else(bad)
    }
}

impl<'de> Deserialize<'de> for Size {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Size, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = Size;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a number of bytes, or one with KiB, MiB, GiB or TiB")
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Size, E> {
                Ok(Size(v))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Size, E> {
                u64::try_from(v)
                    .map(Size)
                    .map_err(|_| E::custom(format!("invalid size {v}: want 0 or more")))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Size, E> {
                Size::parse(v).map_err(E::custom)
            }
        }
        d.deserialize_any(V)
    }
}
