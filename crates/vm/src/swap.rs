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

/// The largest block a guest's disk may have: Linux refuses a disk whose
/// block is larger than its page, 4 KiB in Hangar's guests.
#[cfg(target_os = "linux")]
const GUEST_PAGE: u32 = 4096;

/// Whether the swap disk at path can bypass the host's page cache here.
/// The monitor makes the alignment direct I/O needs on the file its
/// guest's block size, and some file systems need more than a page: ZFS
/// its record size, 128 KiB by default, which leaves the guest unable to
/// use the disk and so unable to boot. Without direct I/O the host caches
/// what the guest swaps out, which is memory spent twice but no more.
#[cfg(target_os = "linux")]
pub fn direct_fits(path: &Path) -> bool {
    use std::os::fd::AsRawFd;
    let Ok(f) = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECT)
        .open(path)
    else {
        // The file system has no direct I/O at all.
        return false;
    };
    let mut stx = std::mem::MaybeUninit::<libc::statx>::zeroed();
    // SAFETY: f is open, and stx is a statx for the kernel to fill.
    let filled = unsafe {
        libc::statx(
            f.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH,
            libc::STATX_DIOALIGN,
            stx.as_mut_ptr(),
        )
    } == 0;
    if !filled {
        // The monitor assumes 512-byte sectors when the kernel can't say.
        return true;
    }
    // SAFETY: statx succeeded and filled stx.
    let stx = unsafe { stx.assume_init() };
    stx.stx_mask & libc::STATX_DIOALIGN == 0
        || stx.stx_dio_mem_align.max(stx.stx_dio_offset_align) <= GUEST_PAGE
}

/// Whether the swap disk at path can bypass the host's page cache here:
/// macOS's direct I/O (F_NOCACHE) asks no alignment of its own.
#[cfg(not(target_os = "linux"))]
pub fn direct_fits(_path: &Path) -> bool {
    true
}

/// Deletes the swap disk of a machine that has stopped, which the next
/// boot makes afresh, unless it is suspended: the guest it resumes wants
/// back what it swapped out.
pub fn drop_swap(dir: &Path, suspended: bool) {
    if !suspended {
        let _ = std::fs::remove_file(swap_file(dir));
    }
}
