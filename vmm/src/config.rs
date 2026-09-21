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

    /// Outbound networking, through passt.
    #[serde(default)]
    pub net: Option<Net>,

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
pub struct Net {
    /// The guest's MAC address, as six colon-separated bytes. Absent picks a
    /// fixed locally administered one, which is fine while a host runs one
    /// guest per network.
    #[serde(default)]
    pub mac: Option<String>,
    /// MTU handed to the guest over DHCP.
    #[serde(default = "default_mtu")]
    pub mtu: u16,
}

fn default_mtu() -> u16 {
    1500
}

impl Net {
    /// The MAC to put in the device's configuration space.
    pub fn mac_bytes(&self) -> Result<[u8; 6], String> {
        let Some(text) = &self.mac else {
            // Locally administered, and not one of the ranges a real card
            // would claim.
            return Ok([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        };
        let parts: Vec<&str> = text.split(':').collect();
        if parts.len() != 6 {
            return Err(format!("{text:?} is not a MAC address"));
        }
        let mut out = [0u8; 6];
        for (i, p) in parts.iter().enumerate() {
            out[i] =
                u8::from_str_radix(p, 16).map_err(|_| format!("{text:?} is not a MAC address"))?;
        }
        Ok(out)
    }
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
