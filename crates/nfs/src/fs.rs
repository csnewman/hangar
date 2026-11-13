//! Paths, file handles, and the system calls on the root a path takes.
//!
//! A path here is relative to the server's root, in bytes as the system
//! keeps names: "" is the root, which lists the sets; "<set>" a set's
//! directory; "<set>/a/b" a file in it.

use std::collections::HashMap;
use std::io;
use std::os::fd::RawFd;
use std::sync::Mutex;

use crate::proto::*;
use crate::sys;

/// The longest name.
pub const MAX_NAME: usize = 255;

/// A path's set, and the path in the set.
pub fn split(p: &[u8]) -> (&[u8], &[u8]) {
    match p.iter().position(|b| *b == b'/') {
        Some(i) => (&p[..i], &p[i + 1..]),
        None => (p, b""),
    }
}

/// A path's directory, "" for the root's.
pub fn dir_of(p: &[u8]) -> &[u8] {
    match p.iter().rposition(|b| *b == b'/') {
        Some(i) => &p[..i],
        None => b"",
    }
}

/// A name in a directory: the name alone in the root.
pub fn join(dir: &[u8], name: &[u8]) -> Vec<u8> {
    if dir.is_empty() {
        name.to_vec()
    } else {
        [dir, b"/", name].concat()
    }
}

/// Whether a name is one component, as a LOOKUP or a create gives it.
pub fn valid_name(n: &[u8]) -> u32 {
    if n.is_empty() || n == b"." || n == b".." {
        return ERR_BAD_NAME;
    }
    if n.len() > MAX_NAME {
        return ERR_NAME_TOO_LONG;
    }
    if n.contains(&b'/') || n.contains(&0) {
        return ERR_BAD_NAME;
    }
    NFS_OK
}

/// The NFS status for a system call's error.
pub fn errno(err: &io::Error) -> u32 {
    let Some(e) = err.raw_os_error() else {
        return ERR_SERVER_FAULT;
    };
    match e {
        libc::EPERM => ERR_PERM,
        libc::ENOENT => ERR_NO_ENT,
        libc::EIO => ERR_IO,
        libc::ENXIO => ERR_NXIO,
        libc::EACCES => ERR_ACCESS,
        libc::EEXIST => ERR_EXIST,
        libc::EXDEV => ERR_X_DEV,
        libc::ENOTDIR => ERR_NOT_DIR,
        libc::EISDIR => ERR_IS_DIR,
        libc::EINVAL => ERR_INVAL,
        libc::EFBIG => ERR_F_BIG,
        libc::ENOSPC => ERR_NO_SPC,
        libc::EROFS => ERR_ROFS,
        libc::EMLINK => ERR_M_LINK,
        libc::ENAMETOOLONG => ERR_NAME_TOO_LONG,
        libc::ENOTEMPTY => ERR_NOT_EMPTY,
        libc::EDQUOT => ERR_D_QUOT,
        libc::ESTALE => ERR_STALE,
        // A link in the way of a path: the root's resolution refuses to
        // follow it.
        libc::ELOOP => ERR_SYMLINK,
        libc::EAGAIN => ERR_DELAY,
        libc::EBADF => ERR_OPEN_MODE,
        _ => ERR_IO,
    }
}

/// A descriptor closed when it goes.
pub struct Fd(pub RawFd);

impl Drop for Fd {
    fn drop(&mut self) {
        // SAFETY: the descriptor is this one's alone.
        unsafe { libc::close(self.0) };
    }
}

pub fn cstr(p: &[u8]) -> io::Result<std::ffi::CString> {
    std::ffi::CString::new(p).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
}

/// The *at calls on a root, beneath it and through no symbolic link.
pub struct Root {
    pub fd: RawFd,
}

impl Root {
    /// Opens a directory of the root.
    pub fn open_dir(&self, p: &[u8]) -> io::Result<Fd> {
        if p.is_empty() {
            // SAFETY: the root's descriptor is open for the root's life.
            let fd = unsafe { libc::dup(self.fd) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            return Ok(Fd(fd));
        }
        sys::open_beneath(self.fd, p, false).map(Fd)
    }

    /// Opens the directory a path is in, and gives its last component.
    pub fn at<'p>(&self, p: &'p [u8]) -> io::Result<(Fd, &'p [u8])> {
        let (dir, name) = match p.iter().rposition(|b| *b == b'/') {
            Some(i) => (&p[..i], &p[i + 1..]),
            None => (&b""[..], p),
        };
        Ok((self.open_dir(dir)?, name))
    }

    /// Opens a directory of the root to read its entries.
    pub fn open_dir_read(&self, p: &[u8]) -> io::Result<Fd> {
        sys::open_beneath(self.fd, p, true).map(Fd)
    }

    /// A file's attributes, without following a link.
    pub fn lstat(&self, p: &[u8]) -> io::Result<libc::stat> {
        if p.is_empty() {
            return Err(io::Error::other("the root has no file"));
        }
        let (fd, name) = self.at(p)?;
        fstatat(fd.0, name)
    }

    /// Opens a file of the root, never through a link.
    pub fn open_file(&self, p: &[u8], flags: libc::c_int, mode: u32) -> io::Result<Fd> {
        let (fd, name) = self.at(p)?;
        let c = cstr(name)?;
        // SAFETY: c is a valid NUL-terminated name.
        let f = unsafe {
            libc::openat(
                fd.0,
                c.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                mode as libc::c_uint,
            )
        };
        if f < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Fd(f))
    }
}

/// A file's attributes in dir, without following a link.
pub fn fstatat(dir: RawFd, name: &[u8]) -> io::Result<libc::stat> {
    let c = cstr(name)?;
    // SAFETY: st is written by fstatat.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::fstatat(dir, c.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) };
    if r < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(st)
}

/// Gives each path a filehandle, and finds it again by one. A handle is an
/// ID and the server's boot verifier: one from before the server started
/// is stale. The root's is always the same, so a client whose server
/// started again finds every name again from there.
pub struct Handles {
    pub boot: [u8; 8],
    inner: Mutex<HandleMap>,
}

struct HandleMap {
    next: u64,
    ids: HashMap<Vec<u8>, u64>,
    paths: HashMap<u64, Vec<u8>>,
}

/// The root's handle, whichever server gave it.
pub const ROOT_HANDLE: &[u8] = b"hangar-nfs-root\0";

impl Handles {
    pub fn new(boot: [u8; 8]) -> Handles {
        Handles {
            boot,
            inner: Mutex::new(HandleMap {
                next: 2,
                ids: HashMap::from([(Vec::new(), 1)]),
                paths: HashMap::from([(1, Vec::new())]),
            }),
        }
    }

    pub fn handle(&self, p: &[u8]) -> Vec<u8> {
        if p.is_empty() {
            return ROOT_HANDLE.to_vec();
        }
        let mut m = self.inner.lock().unwrap();
        let id = match m.ids.get(p) {
            Some(id) => *id,
            None => {
                let id = m.next;
                m.next += 1;
                m.ids.insert(p.to_vec(), id);
                m.paths.insert(id, p.to_vec());
                id
            }
        };
        let mut fh = id.to_be_bytes().to_vec();
        fh.extend_from_slice(&self.boot);
        fh
    }

    pub fn path(&self, fh: &[u8]) -> Result<Vec<u8>, u32> {
        if fh == ROOT_HANDLE {
            return Ok(Vec::new());
        }
        if fh.len() != 16 {
            return Err(ERR_BAD_HANDLE);
        }
        if fh[8..] != self.boot {
            return Err(ERR_STALE);
        }
        let id = u64::from_be_bytes(fh[..8].try_into().unwrap());
        self.inner
            .lock()
            .unwrap()
            .paths
            .get(&id)
            .cloned()
            .ok_or(ERR_STALE)
    }

    /// Follows a rename: from, and everything under it, are at to.
    pub fn moved(&self, from: &[u8], to: &[u8]) {
        let mut m = self.inner.lock().unwrap();
        if let Some(id) = m.ids.remove(to) {
            m.paths.remove(&id);
        }
        let under = [from, b"/"].concat();
        let moving: Vec<(Vec<u8>, u64)> = m
            .ids
            .iter()
            .filter(|(p, _)| p.as_slice() == from || p.starts_with(&under))
            .map(|(p, id)| (p.clone(), *id))
            .collect();
        for (p, id) in moving {
            let np = [to, &p[from.len()..]].concat();
            m.ids.remove(&p);
            m.ids.insert(np.clone(), id);
            m.paths.insert(id, np);
        }
    }

    /// Forgets a path removed: its handle is stale.
    pub fn removed(&self, p: &[u8]) {
        let mut m = self.inner.lock().unwrap();
        if let Some(id) = m.ids.remove(p) {
            m.paths.remove(&id);
        }
    }
}
