//! The system calls the server makes that differ between Linux and macOS,
//! and acting as another user for them.

use std::ffi::CString;
use std::io;
use std::os::fd::RawFd;

fn cstr(p: &[u8]) -> io::Result<CString> {
    CString::new(p).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
}

fn check(r: libc::c_int) -> io::Result<libc::c_int> {
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r)
    }
}

/// Who filesystem calls are made as.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct User {
    pub uid: u32,
    pub gid: u32,
    pub groups: Vec<u32>,
}

/// The process's own filesystem identity, for a server run as root.
pub fn root() -> User {
    User::default()
}

/// Whether the process can act as others: it runs as root.
pub fn privileged() -> bool {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() == 0 }
}

/// Runs f with the calling thread's filesystem identity set to u, and set
/// back to what it was after: as another user, the kernel checks
/// permissions and gives new files their owner as it would for them. A
/// filesystem uid other than 0 drops the filesystem capabilities with it,
/// which return with 0. A process not running as root cannot set it, and
/// on macOS no thread has one: f runs as the process.
pub fn act_as<T>(u: &User, f: impl FnOnce() -> T) -> T {
    #[cfg(target_os = "linux")]
    if privileged() {
        // SAFETY: setfsuid and setfsgid with -1 only read the current
        // value; the groups are set by the raw call, which changes only
        // this thread's (the C library's changes every thread's).
        unsafe {
            let prev_uid = libc::setfsuid(u32::MAX);
            let prev_gid = libc::setfsgid(u32::MAX);
            let n = libc::syscall(libc::SYS_getgroups, 0, std::ptr::null_mut::<libc::gid_t>());
            let mut prev_groups = vec![0 as libc::gid_t; n.max(0) as usize];
            if n > 0 {
                libc::syscall(libc::SYS_getgroups, n, prev_groups.as_mut_ptr());
            }
            libc::syscall(libc::SYS_setgroups, u.groups.len(), u.groups.as_ptr());
            libc::setfsgid(u.gid);
            libc::setfsuid(u.uid);
            let out = f();
            libc::setfsuid(prev_uid as u32);
            libc::setfsgid(prev_gid as u32);
            libc::syscall(libc::SYS_setgroups, prev_groups.len(), prev_groups.as_ptr());
            return out;
        }
    }
    let _ = u;
    f()
}

/// The lock commands: OFD locks belong to the open file description, so
/// each open and lock owner holds its own, as NFS's do. macOS's are in its
/// sys/fcntl.h.
#[cfg(target_os = "linux")]
pub const OFD_SETLK: libc::c_int = libc::F_OFD_SETLK;
#[cfg(target_os = "linux")]
pub const OFD_GETLK: libc::c_int = libc::F_OFD_GETLK;
#[cfg(target_os = "macos")]
pub const OFD_SETLK: libc::c_int = 90;
#[cfg(target_os = "macos")]
pub const OFD_GETLK: libc::c_int = 92;

/// macOS's O_SEARCH, from sys/fcntl.h: a directory opened to search, not
/// read; and O_RESOLVE_BENEATH: resolution may not leave the directory it
/// starts from.
#[cfg(target_os = "macos")]
const O_SEARCH: libc::c_int = 0x4000_0000 | libc::O_DIRECTORY;
#[cfg(target_os = "macos")]
const O_RESOLVE_BENEATH: libc::c_int = 0x0000_1000;
#[cfg(target_os = "macos")]
const O_NOFOLLOW_ANY: libc::c_int = 0x2000_0000;

/// Opens the server's root, for the *at calls.
pub fn open_root(root: &[u8]) -> io::Result<RawFd> {
    let c = cstr(root)?;
    #[cfg(target_os = "linux")]
    let flags = libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC;
    #[cfg(target_os = "macos")]
    let flags = O_SEARCH | libc::O_CLOEXEC;
    // SAFETY: c is a valid NUL-terminated path.
    check(unsafe { libc::open(c.as_ptr(), flags) })
}

/// Opens a directory beneath dir, through no symbolic link: for the *at
/// calls, or to read if read. A link made in a set cannot lead a later call
/// out of it.
pub fn open_beneath(dir: RawFd, p: &[u8], read: bool) -> io::Result<RawFd> {
    let c = cstr(p)?;
    #[cfg(target_os = "linux")]
    {
        let flags = if read { libc::O_RDONLY } else { libc::O_PATH };
        // SAFETY: a zeroed open_how is a valid one.
        let mut how: libc::open_how = unsafe { std::mem::zeroed() };
        how.flags = (flags | libc::O_DIRECTORY | libc::O_CLOEXEC) as u64;
        how.resolve =
            libc::RESOLVE_BENEATH | libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_MAGICLINKS;
        // SAFETY: how is a valid open_how of the size given.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_openat2,
                dir,
                c.as_ptr(),
                &how as *const libc::open_how,
                std::mem::size_of::<libc::open_how>(),
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(fd as RawFd)
    }
    #[cfg(target_os = "macos")]
    {
        let flags = if read { libc::O_RDONLY } else { O_SEARCH };
        // SAFETY: c is a valid NUL-terminated path.
        check(unsafe {
            libc::openat(
                dir,
                c.as_ptr(),
                flags | libc::O_DIRECTORY | libc::O_CLOEXEC | O_NOFOLLOW_ANY | O_RESOLVE_BENEATH,
            )
        })
    }
}

/// Reserves space for a range, growing the file to hold it, or if punch
/// makes the range read as zeros and keeps the file's size.
pub fn allocate(fd: RawFd, punch: bool, off: i64, n: i64) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        let mode = if punch {
            libc::FALLOC_FL_PUNCH_HOLE | libc::FALLOC_FL_KEEP_SIZE
        } else {
            0
        };
        // SAFETY: fd is the caller's open file.
        check(unsafe { libc::fallocate(fd, mode, off, n) }).map(|_| ())
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: st is written by fstat.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        check(unsafe { libc::fstat(fd, &mut st) })?;
        let end = off + n;
        if !punch {
            if end <= st.st_size {
                return Ok(());
            }
            let mut store = libc::fstore_t {
                fst_flags: libc::F_ALLOCATEALL,
                fst_posmode: libc::F_PEOFPOSMODE,
                fst_offset: 0,
                fst_length: end - st.st_size,
                fst_bytesalloc: 0,
            };
            // A failed reservation leaves the growth to the truncate.
            unsafe { libc::fcntl(fd, libc::F_PREALLOCATE, &mut store) };
            return check(unsafe { libc::ftruncate(fd, end) }).map(|_| ());
        }
        // The whole blocks in the range are punched out, the parts of
        // blocks at its ends zeroed.
        let end = end.min(st.st_size);
        if off >= end {
            return Ok(());
        }
        let bs = if st.st_blksize > 0 {
            st.st_blksize as i64
        } else {
            4096
        };
        let (mut first, mut last) = ((off + bs - 1) / bs * bs, end / bs * bs);
        if first < last {
            let p = libc::fpunchhole_t {
                fp_flags: 0,
                reserved: 0,
                fp_offset: first,
                fp_length: last - first,
            };
            check(unsafe { libc::fcntl(fd, libc::F_PUNCHHOLE, &p) })?;
        } else {
            (first, last) = (end, end);
        }
        zero(fd, off, first)?;
        zero(fd, last.max(off), end)
    }
}

/// Writes zeros over [from, to).
#[cfg(target_os = "macos")]
fn zero(fd: RawFd, mut from: i64, to: i64) -> io::Result<()> {
    let buf = vec![0u8; 64 << 10];
    while from < to {
        let k = (buf.len() as i64).min(to - from) as usize;
        // SAFETY: buf holds k bytes.
        let w = unsafe { libc::pwrite(fd, buf.as_ptr().cast(), k, from) };
        if w < 0 {
            return Err(io::Error::last_os_error());
        }
        from += w as i64;
    }
    Ok(())
}

/// Makes a FIFO or socket in dir.
pub fn mknod_at(dir: RawFd, name: &[u8], mode: u32) -> io::Result<()> {
    let c = cstr(name)?;
    // SAFETY: c is a valid NUL-terminated name.
    check(unsafe { libc::mknodat(dir, c.as_ptr(), mode as libc::mode_t, 0) }).map(|_| ())
}
