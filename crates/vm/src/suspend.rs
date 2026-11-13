//! A suspended machine: its disks plus a snapshot of it, its memory and
//! device state as the monitor wrote them, and beside them the state each
//! backend wrote of what the guest was holding.
//!
//! SUSPENDED_MARK is written last, once all of that is complete, so a
//! machine with it is resumable and one without it boots afresh. It records
//! what the guest had open that lives outside its writable disks -- the
//! base, and any read-only disk beside it -- because a guest resumed
//! against a different one would find files it holds changed underneath it.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const SUSPENDED_MARK: &str = "suspended.json";
pub const SNAPSHOT_DIR: &str = "snapshot";

/// What a suspended machine was running against.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    #[serde(default)]
    pub base: String,
    /// How the base was attached: the snapshot holds its devices, which a
    /// machine attaching it another way does not have.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub image: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_only: Vec<String>,
    /// How a machine under libkrun laid out its memory, which it is resumed
    /// with whatever its memory has been set to since: its snapshot holds
    /// RAM and a virtio-mem region of these sizes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub krun_memory: Option<KrunMemory>,
}

/// A machine's RAM from boot and its virtio-mem region beyond, in MiB.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KrunMemory {
    pub boot: u32,
    pub region: u32,
}

impl Record {
    /// Whether two records describe the same machine. A pulled copy has no
    /// base to fingerprint, its image's fingerprint saying the same, so a
    /// base only one of them records is passed over.
    pub fn same_machine(&self, o: &Record) -> bool {
        let same_base = self.base == o.base || self.base.is_empty() || o.base.is_empty();
        same_base && self.image == o.image && self.read_only == o.read_only
    }
}

/// Identifies a file or directory as the one it is: made again in the same
/// place, even from the same source, it is a different one.
pub fn fingerprint(path: &Path) -> String {
    use std::os::unix::fs::MetadataExt;
    if path.as_os_str().is_empty() {
        return String::new();
    }
    let Ok(m) = std::fs::metadata(path) else {
        return String::new();
    };
    let mtime = m.mtime() as i128 * 1_000_000_000 + m.mtime_nsec() as i128;
    format!(
        "{}:{}:{mtime}:{}:{}",
        path.display(),
        m.size(),
        m.dev(),
        m.ino()
    )
}

/// A machine's suspend record, if it is suspended.
pub fn suspended(dir: &Path) -> Option<Record> {
    let b = std::fs::read(dir.join(SUSPENDED_MARK)).ok()?;
    serde_json::from_slice(&b).ok()
}

/// Forgets a suspended machine's memory. Its disks are left as the guest
/// had them, which is what a power cut leaves.
pub fn discard(dir: &Path) {
    let _ = std::fs::remove_file(dir.join(SUSPENDED_MARK));
    let _ = std::fs::remove_dir_all(dir.join(SNAPSHOT_DIR));
    let _ = std::fs::remove_dir_all(dir.join(format!("{SNAPSHOT_DIR}.new")));
}

/// Removes what backends saved of an earlier guest, which a fresh boot's
/// backends would otherwise restore into a guest that never held it.
pub fn clear_backend_state(dir: &Path) {
    for f in ["gpu.json", "gpu.ready"] {
        let _ = std::fs::remove_file(dir.join(f));
    }
}

/// Writes a file whole or not at all.
pub fn write_atomic(path: &Path, b: &[u8]) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    std::fs::write(&tmp, b)?;
    std::fs::rename(&tmp, path)
}

/// Marks a machine suspended against what r records, once its snapshot is
/// complete.
pub fn mark(dir: &Path, r: &Record) -> io::Result<()> {
    write_atomic(
        &dir.join(SUSPENDED_MARK),
        &serde_json::to_vec(r).map_err(io::Error::other)?,
    )
}
