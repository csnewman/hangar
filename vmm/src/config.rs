//! The description of one environment, as Go writes it.
//!
//! This is the whole interface: the process is handed a JSON document and
//! runs exactly the machine it describes. There are no defaults worth
//! guessing at here, because the only caller is `internal/vmm`, which knows
//! what it wants.

use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    /// Name used in log lines, so several guests can share a log.
    #[serde(default = "default_name")]
    pub name: String,

    /// Guest kernel: an arm64 `Image`.
    pub kernel: PathBuf,
    /// Optional initramfs.
    #[serde(default)]
    pub initrd: Option<PathBuf>,
    /// Kernel command line, passed through untouched.
    #[serde(default)]
    pub cmdline: String,

    pub memory_mib: u64,
    pub cpus: u8,

    /// Block devices, in order. The first becomes `/dev/vda`.
    #[serde(default)]
    pub disks: Vec<Disk>,

    /// Host directory exported as a virtio-fs filesystem.
    #[serde(default)]
    pub fs: Option<Fs>,

    /// vsock context ID for the guest. The agent dials the host on it.
    #[serde(default)]
    pub vsock_cid: Option<u32>,

    /// Balloon, for passive memory reclaim.
    #[serde(default)]
    pub balloon: Option<Balloon>,

    /// Where the guest's serial console goes. Absent means stdout.
    #[serde(default)]
    pub console: Option<PathBuf>,
}

fn default_name() -> String {
    "hangar-env".to_string()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Disk {
    pub path: PathBuf,
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Fs {
    /// Host directory the guest sees.
    pub shared_dir: PathBuf,
    /// Mount tag, as `mount -t virtiofs <tag> /mnt` names it.
    pub tag: String,
    /// Size of the DAX window in MiB. Zero switches DAX off, leaving the
    /// filesystem to serve every read as a FUSE request.
    #[serde(default = "default_dax_mib")]
    pub dax_mib: u64,
    /// Request queues. Each one gets a thread.
    #[serde(default = "default_fs_queues")]
    pub queues: u16,
}

fn default_dax_mib() -> u64 {
    1024
}

fn default_fs_queues() -> u16 {
    1
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Balloon {
    /// Report free pages back to the host as the guest frees them.
    #[serde(default = "default_true")]
    pub free_page_reporting: bool,
}

fn default_true() -> bool {
    true
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if self.cpus == 0 {
            return Err("cpus must be at least 1".into());
        }
        if self.memory_mib < 64 {
            return Err("memoryMib must be at least 64".into());
        }
        if !self.kernel.exists() {
            return Err(format!("kernel {} does not exist", self.kernel.display()));
        }
        if let Some(fs) = &self.fs {
            if !fs.shared_dir.is_dir() {
                return Err(format!(
                    "sharedDir {} is not a directory",
                    fs.shared_dir.display()
                ));
            }
            if fs.dax_mib % 2 != 0 {
                return Err("daxMib must be a multiple of 2, to keep the window \
                            aligned to a memory subsection"
                    .into());
            }
        }
        Ok(())
    }
}
