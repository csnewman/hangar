//! A machine's swap disk.

use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// The machine's swap disk, in its directory.
pub fn swap_file(dir: &Path) -> std::path::PathBuf {
    dir.join("swap.img")
}

/// Gives a machine booting from its disks an empty swap disk of mib, or
/// takes away the one it had when mib is 0: what a guest swapped out is of
/// no use to the next boot. The file is sparse, and the guest writes the
/// swap header itself, for its own page size.
pub fn make_swap(dir: &Path, mib: u64) -> io::Result<()> {
    let path = swap_file(dir);
    if mib == 0 {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)?;
    f.set_len(mib << 20)
}

/// Deletes the swap disk of a machine that has stopped, which the next
/// boot makes afresh, unless it is suspended: the guest it resumes wants
/// back what it swapped out.
pub fn drop_swap(dir: &Path, suspended: bool) {
    if !suspended {
        let _ = std::fs::remove_file(swap_file(dir));
    }
}
