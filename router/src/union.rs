// SPDX-License-Identifier: Apache-2.0

//! The union: a writable upper directory over read-only lower ones, as the
//! guest's root.
//!
//! It keeps overlayfs's on-disk format in the upper, so a disk overlayfs
//! wrote is read as it was: a whiteout is a character device 0/0, an opaque
//! directory has `trusted.overlay.opaque` set to `y`. The router's clients
//! may run an overlay of their own on it (Docker's overlay2 does), so theirs
//! are kept apart: their `trusted.overlay.*` attributes are stored as
//! `trusted.overlay.overlay.*`, as overlayfs itself escapes a nested
//! overlay's, and a 0/0 device they make is tagged `trusted.hangar.real` so it
//! is not mistaken for the router's whiteout.
//!
//! Every node is held open by `O_PATH` descriptors on its real files, and
//! every operation is made relative to them.
//!
//! What it does not do yet: rename a directory with anything in a lower layer
//! (`EXDEV`, as overlayfs without `redirect_dir`); and a lower file opened for
//! writing while it is open for reading elsewhere is refused with `EBUSY`,
//! since the kernel allows one passthrough backing file per inode.

use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString, OsStr};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fuser::{
    AccessFlags, BackingId, CopyFileRangeFlags, Errno, FileAttr, FileHandle, FileType,
    Filesystem, FopenFlags, Generation, INodeNo, InitFlags, KernelConfig, LockOwner, OpenFlags,
    RenameFlags, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory, ReplyDirectoryPlus,
    ReplyEmpty, ReplyEntry, ReplyLock, ReplyLseek, ReplyOpen, ReplyStatfs, ReplyWrite,
    ReplyXattr, Request, TimeOrNow, WriteFlags,
};
use parking_lot::{Mutex, RwLock};

const OPAQUE: &CStr = c"trusted.overlay.opaque";
/// Marks a 0/0 character device a client made, so it is not read as a
/// whiteout.
const REAL: &CStr = c"trusted.hangar.real";
/// overlayfs's own attributes, as a client sees and sets them.
const OVL_PREFIX: &[u8] = b"trusted.overlay.";
/// Where a client's are stored, as overlayfs escapes a nested overlay's.
const ESCAPED_PREFIX: &[u8] = b"trusted.overlay.overlay.";
/// The router's own, never shown.
const HANGAR_PREFIX: &[u8] = b"trusted.hangar.";

const EMPTY: &CStr = c"";

struct Node {
    /// The file in the upper, once there is one.
    upper: RwLock<Option<Arc<OwnedFd>>>,
    /// For a directory, the lower directories merged under it, topmost
    /// first; for anything else, the lower file it shows while it has no
    /// upper.
    lowers: RwLock<Vec<Arc<OwnedFd>>>,
    /// Where it is, for copying it up: its parent's node and its name there.
    place: Mutex<(u64, CString)>,
    is_dir: bool,
    is_symlink: bool,
    key: Mutex<(u64, u64)>,
    lookups: AtomicU64,
    /// Whether the file is known to have no `security.capability`, which
    /// the kernel asks for before every write and change of owner: the low
    /// bit says so, and the rest counts changes to that attribute, so an
    /// answer read before a change cannot be recorded after it.
    caps: AtomicU64,
}

/// `Node::caps` when the file is known to have no `security.capability`.
const NO_CAPS: u64 = 1;

const CAPS_NAME: &[u8] = b"security.capability";

impl Node {
    fn upper(&self) -> Option<Arc<OwnedFd>> {
        self.upper.read().clone()
    }

    /// The file whose contents and attributes it shows.
    fn real(&self) -> Arc<OwnedFd> {
        if let Some(u) = self.upper() {
            return u;
        }
        self.lowers.read()[0].clone()
    }

    fn has_lower(&self) -> bool {
        !self.lowers.read().is_empty()
    }
}

enum Handle {
    File {
        /// This open's own descriptor, for its locks, `fsync`, `lseek` and
        /// `fallocate`, and for its contents when not passed through.
        file: File,
        /// Held while the file is open, so the kernel keeps passing its
        /// contents to the backing file.
        _backing: Option<Arc<BackingId>>,
    },
}

#[derive(Default)]
struct Table {
    nodes: HashMap<u64, Arc<Node>>,
    by_key: HashMap<(u64, u64), u64>,
}

pub struct Router {
    table: RwLock<Table>,
    next_ino: AtomicU64,
    handles: RwLock<HashMap<u64, Arc<Handle>>>,
    next_fh: AtomicU64,
    ttl: Duration,
    passthrough: bool,
    /// Whether to ask for FUSE over io_uring.
    uring: bool,
    /// The backing file registered for each inode's contents. The kernel
    /// allows one per inode at a time, so every open of an inode shares it,
    /// and it goes when the last of them is released.
    backings: Mutex<HashMap<u64, Weak<BackingId>>>,
    /// Serialises copy-up, which creates parents before children.
    copy_up: Mutex<()>,
}

fn errno(e: io::Error) -> Errno {
    Errno::from_i32(e.raw_os_error().unwrap_or(libc::EIO))
}

fn last() -> Errno {
    errno(io::Error::last_os_error())
}

fn err(code: i32) -> Errno {
    Errno::from_i32(code)
}

fn cstr(name: &OsStr) -> Result<CString, Errno> {
    CString::new(name.as_bytes()).map_err(|_| err(libc::EINVAL))
}

/// `/proc/self/fd/N`, which reopens or names the file an `O_PATH`
/// descriptor holds. Calls that follow links reach that file through it --
/// for a symlink's descriptor, the symlink itself -- while the `l` variants
/// would act on the `/proc` link.
fn proc_path(fd: RawFd) -> CString {
    CString::new(format!("/proc/self/fd/{fd}")).unwrap()
}

/// fchmodat2(2), which the C library may not wrap: the same number on
/// every architecture.
const SYS_FCHMODAT2: libc::c_long = 452;

/// Changes the mode of an `O_PATH` descriptor's file. fchmod(2) refuses
/// such a descriptor; fchmodat2(2) (Linux 6.6) with an empty path does not,
/// and on an older kernel the file is named through `/proc`.
fn chmod_fd(fd: RawFd, mode: u32) -> Result<(), Errno> {
    let r = unsafe { libc::syscall(SYS_FCHMODAT2, fd, EMPTY.as_ptr(), mode, libc::AT_EMPTY_PATH) };
    if r == 0 {
        return Ok(());
    }
    let e = last();
    if e.code() != libc::ENOSYS {
        return Err(e);
    }
    if unsafe { libc::chmod(proc_path(fd).as_ptr(), mode) } < 0 { Err(last()) } else { Ok(()) }
}

fn stat_fd(fd: RawFd) -> Result<libc::stat, Errno> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let r = unsafe {
        libc::fstatat(fd, EMPTY.as_ptr(), &mut st, libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW)
    };
    if r < 0 { Err(last()) } else { Ok(st) }
}

fn open_path(dir: RawFd, name: &CStr) -> Result<OwnedFd, Errno> {
    let fd = unsafe { libc::openat(dir, name.as_ptr(), libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
    if fd < 0 { Err(last()) } else { Ok(unsafe { OwnedFd::from_raw_fd(fd) }) }
}

fn is_dir(st: &libc::stat) -> bool {
    st.st_mode & libc::S_IFMT == libc::S_IFDIR
}

/// The value of an attribute of an `O_PATH` descriptor's file, if set.
fn xattr(fd: RawFd, name: &CStr) -> Option<Vec<u8>> {
    let path = proc_path(fd);
    let mut buf = vec![0u8; 256];
    let n = unsafe { libc::getxattr(path.as_ptr(), name.as_ptr(), buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
    if n < 0 {
        return None;
    }
    buf.truncate(n as usize);
    Some(buf)
}

fn set_xattr(fd: RawFd, name: &CStr, value: &[u8]) -> Result<(), Errno> {
    let path = proc_path(fd);
    let r = unsafe {
        libc::setxattr(path.as_ptr(), name.as_ptr(), value.as_ptr() as *const libc::c_void, value.len(), 0)
    };
    if r < 0 { Err(last()) } else { Ok(()) }
}

/// Whether a file is a whiteout: a 0/0 character device the router made.
fn whiteout(fd: RawFd, st: &libc::stat) -> bool {
    st.st_mode & libc::S_IFMT == libc::S_IFCHR && st.st_rdev == 0 && xattr(fd, REAL).is_none()
}

fn opaque(fd: RawFd) -> bool {
    xattr(fd, OPAQUE).as_deref() == Some(b"y")
}

fn to_time(sec: i64, nsec: i64) -> SystemTime {
    if sec >= 0 {
        UNIX_EPOCH + Duration::new(sec as u64, nsec as u32)
    } else {
        UNIX_EPOCH - Duration::new((-sec) as u64, 0) + Duration::from_nanos(nsec as u64)
    }
}

fn kind(mode: u32) -> FileType {
    match mode & libc::S_IFMT {
        libc::S_IFDIR => FileType::Directory,
        libc::S_IFLNK => FileType::Symlink,
        libc::S_IFCHR => FileType::CharDevice,
        libc::S_IFBLK => FileType::BlockDevice,
        libc::S_IFIFO => FileType::NamedPipe,
        libc::S_IFSOCK => FileType::Socket,
        _ => FileType::RegularFile,
    }
}

fn attr(ino: u64, st: &libc::stat) -> FileAttr {
    FileAttr {
        ino: INodeNo(ino),
        size: st.st_size as u64,
        blocks: st.st_blocks as u64,
        atime: to_time(st.st_atime, st.st_atime_nsec),
        mtime: to_time(st.st_mtime, st.st_mtime_nsec),
        ctime: to_time(st.st_ctime, st.st_ctime_nsec),
        crtime: UNIX_EPOCH,
        kind: kind(st.st_mode),
        perm: (st.st_mode & 0o7777) as u16,
        nlink: st.st_nlink as u32,
        uid: st.st_uid,
        gid: st.st_gid,
        rdev: st.st_rdev as u32,
        blksize: st.st_blksize as u32,
        flags: 0,
    }
}

/// A directory's entries, with their types.
fn list(fd: RawFd) -> Result<Vec<(CString, u8)>, Errno> {
    let dfd = unsafe { libc::openat(fd, c".".as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) };
    if dfd < 0 {
        return Err(last());
    }
    let dir = unsafe { libc::fdopendir(dfd) };
    if dir.is_null() {
        unsafe { libc::close(dfd) };
        return Err(last());
    }
    let mut out = Vec::new();
    loop {
        let ent = unsafe { libc::readdir(dir) };
        if ent.is_null() {
            break;
        }
        let ent = unsafe { &*ent };
        let name = unsafe { CStr::from_ptr(ent.d_name.as_ptr()) };
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        out.push((name.to_owned(), ent.d_type));
    }
    unsafe { libc::closedir(dir) };
    Ok(out)
}

fn dtype(d: u8) -> FileType {
    match d {
        libc::DT_DIR => FileType::Directory,
        libc::DT_LNK => FileType::Symlink,
        libc::DT_CHR => FileType::CharDevice,
        libc::DT_BLK => FileType::BlockDevice,
        libc::DT_FIFO => FileType::NamedPipe,
        libc::DT_SOCK => FileType::Socket,
        _ => FileType::RegularFile,
    }
}

/// How a client's attribute name is stored.
fn escape(name: &CStr) -> Result<CString, Errno> {
    let b = name.to_bytes();
    if b.starts_with(HANGAR_PREFIX) {
        return Err(err(libc::EPERM));
    }
    if b.starts_with(OVL_PREFIX) {
        let mut out = ESCAPED_PREFIX.to_vec();
        out.extend_from_slice(&b[OVL_PREFIX.len()..]);
        return Ok(CString::new(out).unwrap());
    }
    Ok(name.to_owned())
}

/// How a stored attribute name is shown to clients, or `None` for the
/// router's own.
fn unescape(name: &[u8]) -> Option<Vec<u8>> {
    if name.starts_with(ESCAPED_PREFIX) {
        let mut out = OVL_PREFIX.to_vec();
        out.extend_from_slice(&name[ESCAPED_PREFIX.len()..]);
        return Some(out);
    }
    if name.starts_with(OVL_PREFIX) || name.starts_with(HANGAR_PREFIX) {
        return None;
    }
    Some(name.to_vec())
}

impl Router {
    pub fn new(upper: &Path, lowers: &[std::path::PathBuf], ttl: Duration, passthrough: bool) -> io::Result<Self> {
        let open_dir = |p: &Path| -> io::Result<OwnedFd> {
            let c = CString::new(p.as_os_str().as_bytes())?;
            let fd = unsafe { libc::open(c.as_ptr(), libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC) };
            if fd < 0 { Err(io::Error::last_os_error()) } else { Ok(unsafe { OwnedFd::from_raw_fd(fd) }) }
        };
        let up = Arc::new(open_dir(upper)?);
        let mut lows = Vec::new();
        for l in lowers {
            lows.push(Arc::new(open_dir(l)?));
        }
        let st = stat_fd(up.as_raw_fd()).map_err(|e| io::Error::from_raw_os_error(e.code()))?;
        let key = (st.st_dev, st.st_ino);
        let root = Node {
            upper: RwLock::new(Some(up)),
            lowers: RwLock::new(lows),
            place: Mutex::new((INodeNo::ROOT.0, CString::default())),
            is_dir: true,
            is_symlink: false,
            key: Mutex::new(key),
            lookups: AtomicU64::new(1 << 32),
            caps: AtomicU64::new(0),
        };
        let mut table = Table::default();
        table.nodes.insert(INodeNo::ROOT.0, Arc::new(root));
        table.by_key.insert(key, INodeNo::ROOT.0);
        Ok(Router {
            table: RwLock::new(table),
            next_ino: AtomicU64::new(2),
            handles: RwLock::new(HashMap::new()),
            next_fh: AtomicU64::new(1),
            ttl,
            passthrough,
            uring: false,
            backings: Mutex::new(HashMap::new()),
            copy_up: Mutex::new(()),
        })
    }

    /// Asks the kernel for FUSE over io_uring, which the session must then
    /// serve.
    pub fn with_uring(mut self, on: bool) -> Self {
        self.uring = on;
        self
    }

    fn node(&self, ino: INodeNo) -> Result<Arc<Node>, Errno> {
        self.table.read().nodes.get(&ino.0).cloned().ok_or(err(libc::ESTALE))
    }

    fn handle(&self, fh: FileHandle) -> Result<(Arc<Handle>, RawFd), Errno> {
        let h = self.handles.read().get(&fh.0).cloned().ok_or(err(libc::EBADF))?;
        let fd = match &*h {
            Handle::File { file, .. } => file.as_raw_fd(),
        };
        Ok((h, fd))
    }

    /// Resolves a name under a directory through the layers.
    fn resolve(&self, parent: &Node, name: &CStr) -> Result<(Option<OwnedFd>, Vec<OwnedFd>, libc::stat), Errno> {
        let mut upper = None;
        let mut top: Option<libc::stat> = None;
        let mut search_lower = true;
        if let Some(pu) = parent.upper() {
            match open_path(pu.as_raw_fd(), name) {
                Ok(fd) => {
                    let st = stat_fd(fd.as_raw_fd())?;
                    if whiteout(fd.as_raw_fd(), &st) {
                        return Err(err(libc::ENOENT));
                    }
                    if !is_dir(&st) || !parent.has_lower() || opaque(fd.as_raw_fd()) {
                        search_lower = false;
                    }
                    top = Some(st);
                    upper = Some(fd);
                }
                Err(e) if e.code() == libc::ENOENT => {}
                Err(e) => return Err(e),
            }
        }
        let mut lowers = Vec::new();
        if search_lower {
            let pls = parent.lowers.read().clone();
            for pl in pls {
                let fd = match open_path(pl.as_raw_fd(), name) {
                    Ok(fd) => fd,
                    Err(e) if e.code() == libc::ENOENT || e.code() == libc::ENOTDIR => continue,
                    Err(e) => return Err(e),
                };
                let st = stat_fd(fd.as_raw_fd())?;
                if whiteout(fd.as_raw_fd(), &st) {
                    break;
                }
                match &top {
                    None => {
                        let dir = is_dir(&st);
                        let stop = !dir || opaque(fd.as_raw_fd());
                        top = Some(st);
                        lowers.push(fd);
                        if stop {
                            break;
                        }
                    }
                    Some(t) if is_dir(t) => {
                        if !is_dir(&st) {
                            break;
                        }
                        let stop = opaque(fd.as_raw_fd());
                        lowers.push(fd);
                        if stop {
                            break;
                        }
                    }
                    Some(_) => break,
                }
            }
        }
        match top {
            Some(st) => Ok((upper, lowers, st)),
            None => Err(err(libc::ENOENT)),
        }
    }

    /// Looks a name up and counts a lookup on the node it finds.
    fn lookup_at(&self, parent_ino: u64, parent: &Node, name: &CStr) -> Result<(u64, libc::stat), Errno> {
        let (upper, lowers, st) = self.resolve(parent, name)?;
        Ok((self.intern(parent_ino, name, upper, lowers, &st), st))
    }

    /// The node for what a name resolved to, made if there is none, with a
    /// lookup counted on it.
    fn intern(&self, parent_ino: u64, name: &CStr, upper: Option<OwnedFd>, lowers: Vec<OwnedFd>, st: &libc::stat) -> u64 {
        let key = (st.st_dev, st.st_ino);
        {
            let t = self.table.read();
            if let Some(&ino) = t.by_key.get(&key) {
                if let Some(n) = t.nodes.get(&ino) {
                    n.lookups.fetch_add(1, Ordering::Relaxed);
                    *n.place.lock() = (parent_ino, name.to_owned());
                    return ino;
                }
            }
        }
        let mut t = self.table.write();
        if let Some(&ino) = t.by_key.get(&key) {
            if let Some(n) = t.nodes.get(&ino) {
                n.lookups.fetch_add(1, Ordering::Relaxed);
                return ino;
            }
        }
        let ino = self.next_ino.fetch_add(1, Ordering::Relaxed);
        let node = Node {
            upper: RwLock::new(upper.map(Arc::new)),
            lowers: RwLock::new(lowers.into_iter().map(Arc::new).collect()),
            place: Mutex::new((parent_ino, name.to_owned())),
            is_dir: is_dir(st),
            is_symlink: st.st_mode & libc::S_IFMT == libc::S_IFLNK,
            key: Mutex::new(key),
            lookups: AtomicU64::new(1),
            caps: AtomicU64::new(0),
        };
        t.nodes.insert(ino, Arc::new(node));
        t.by_key.insert(key, ino);
        ino
    }

    fn entry(&self, parent_ino: u64, parent: &Node, name: &CStr, reply: ReplyEntry) {
        match self.lookup_at(parent_ino, parent, name) {
            Ok((ino, st)) => reply.entry(&self.ttl, &attr(ino, &st), Generation(0)),
            // A name that is not there is cached as such, as long as a name
            // that is: only the router adds names, and the kernel forgets the
            // absence itself when one is created through it.
            Err(e) if e.code() == libc::ENOENT => {
                let none: libc::stat = unsafe { std::mem::zeroed() };
                reply.entry(&self.ttl, &attr(0, &none), Generation(0))
            }
            Err(e) => reply.error(e),
        }
    }

    /// Whether a name exists in any lower layer under a directory, which
    /// removing or renaming it in the upper must cover with a whiteout.
    fn in_lower(&self, parent: &Node, name: &CStr) -> bool {
        for pl in parent.lowers.read().iter() {
            if let Ok(fd) = open_path(pl.as_raw_fd(), name) {
                if let Ok(st) = stat_fd(fd.as_raw_fd()) {
                    return !whiteout(fd.as_raw_fd(), &st);
                }
            }
        }
        false
    }

    /// Makes a whiteout at a name in an upper directory.
    fn make_whiteout(dir: RawFd, name: &CStr) -> Result<(), Errno> {
        if unsafe { libc::mknodat(dir, name.as_ptr(), libc::S_IFCHR, 0) } < 0 { Err(last()) } else { Ok(()) }
    }

    /// Removes a whiteout at a name in an upper directory, if there is one,
    /// returning whether there was.
    fn clear_whiteout(dir: RawFd, name: &CStr) -> Result<bool, Errno> {
        let Ok(fd) = open_path(dir, name) else { return Ok(false) };
        let st = stat_fd(fd.as_raw_fd())?;
        if !whiteout(fd.as_raw_fd(), &st) {
            return Ok(false);
        }
        if unsafe { libc::unlinkat(dir, name.as_ptr(), 0) } < 0 {
            return Err(last());
        }
        Ok(true)
    }

    /// Copies a node into the upper, parents first, if it is not there yet.
    fn copy_up(&self, ino: u64, node: &Node) -> Result<Arc<OwnedFd>, Errno> {
        if let Some(u) = node.upper() {
            return Ok(u);
        }
        let _g = self.copy_up.lock();
        if let Some(u) = node.upper() {
            return Ok(u);
        }
        self.copy_up_locked(ino, node)
    }

    fn copy_up_locked(&self, _ino: u64, node: &Node) -> Result<Arc<OwnedFd>, Errno> {
        if let Some(u) = node.upper() {
            return Ok(u);
        }
        let (pino, name) = node.place.lock().clone();
        let parent = self.node(INodeNo(pino))?;
        let pu = self.copy_up_locked(pino, &parent)?;
        let lower = node.lowers.read()[0].clone();
        let lfd = lower.as_raw_fd();
        let st = stat_fd(lfd)?;
        let dir = pu.as_raw_fd();
        match st.st_mode & libc::S_IFMT {
            libc::S_IFDIR => {
                if unsafe { libc::mkdirat(dir, name.as_ptr(), st.st_mode & 0o7777) } < 0 {
                    let e = last();
                    if e.code() != libc::EEXIST {
                        return Err(e);
                    }
                }
            }
            libc::S_IFREG => {
                let tmp = unsafe { libc::openat(dir, c".".as_ptr(), libc::O_TMPFILE | libc::O_WRONLY | libc::O_CLOEXEC, st.st_mode & 0o7777) };
                if tmp < 0 {
                    return Err(last());
                }
                let tmp = unsafe { File::from_raw_fd(tmp) };
                let src = unsafe { libc::open(proc_path(lfd).as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
                if src < 0 {
                    return Err(last());
                }
                let src = unsafe { File::from_raw_fd(src) };
                copy_data(&src, &tmp, st.st_size as usize)?;
                unsafe { libc::fchown(tmp.as_raw_fd(), st.st_uid, st.st_gid) };
                unsafe { libc::fchmod(tmp.as_raw_fd(), st.st_mode & 0o7777) };
                let times = [
                    libc::timespec { tv_sec: st.st_atime, tv_nsec: st.st_atime_nsec },
                    libc::timespec { tv_sec: st.st_mtime, tv_nsec: st.st_mtime_nsec },
                ];
                unsafe { libc::futimens(tmp.as_raw_fd(), times.as_ptr()) };
                copy_xattrs(lfd, tmp.as_raw_fd());
                let r = unsafe {
                    libc::linkat(libc::AT_FDCWD, proc_path(tmp.as_raw_fd()).as_ptr(), dir, name.as_ptr(), libc::AT_SYMLINK_FOLLOW)
                };
                if r < 0 {
                    return Err(last());
                }
            }
            libc::S_IFLNK => {
                let mut buf = vec![0u8; libc::PATH_MAX as usize];
                let n = unsafe { libc::readlinkat(lfd, EMPTY.as_ptr(), buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
                if n < 0 {
                    return Err(last());
                }
                buf.truncate(n as usize);
                let target = CString::new(buf).map_err(|_| err(libc::EINVAL))?;
                if unsafe { libc::symlinkat(target.as_ptr(), dir, name.as_ptr()) } < 0 {
                    return Err(last());
                }
            }
            _ => {
                if unsafe { libc::mknodat(dir, name.as_ptr(), st.st_mode, st.st_rdev) } < 0 {
                    return Err(last());
                }
            }
        }
        let up = open_path(dir, &name)?;
        let ufd = up.as_raw_fd();
        // A regular file was given its owner and attributes before it was
        // linked in. Changing the owner strips `security.capability`, so it
        // comes before the attributes are copied, never after.
        if st.st_mode & libc::S_IFMT != libc::S_IFREG {
            unsafe { libc::fchownat(ufd, EMPTY.as_ptr(), st.st_uid, st.st_gid, libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW) };
            copy_xattrs(lfd, ufd);
        }
        if st.st_mode & libc::S_IFMT == libc::S_IFDIR {
            let times = [
                libc::timespec { tv_sec: st.st_atime, tv_nsec: st.st_atime_nsec },
                libc::timespec { tv_sec: st.st_mtime, tv_nsec: st.st_mtime_nsec },
            ];
            unsafe { libc::utimensat(ufd, EMPTY.as_ptr(), times.as_ptr(), libc::AT_EMPTY_PATH) };
        }
        // Once copied up, the node is found by its upper file, so a hard
        // link made to it, or a later lookup, reaches this node.
        let ust = stat_fd(ufd)?;
        {
            let mut t = self.table.write();
            let mut key = node.key.lock();
            if let Some(ino) = t.by_key.remove(&*key) {
                *key = (ust.st_dev, ust.st_ino);
                t.by_key.insert(*key, ino);
            }
        }
        let up = Arc::new(up);
        *node.upper.write() = Some(up.clone());
        if !node.is_dir {
            // Once copied up, the upper hides the lower; a directory keeps
            // merging its lowers under it.
            node.lowers.write().clear();
        }
        Ok(up)
    }

    /// Opens the file a node shows, with the given flags, copying it up
    /// first if the open may change it.
    fn open_real(&self, ino: u64, node: &Node, flags: i32) -> Result<File, Errno> {
        let writes = flags & libc::O_ACCMODE != libc::O_RDONLY || flags & libc::O_TRUNC != 0;
        let real = if writes { self.copy_up(ino, node)? } else { node.real() };
        let flags = (flags & !(libc::O_CREAT | libc::O_EXCL | libc::O_NOCTTY | libc::O_NOFOLLOW)) | libc::O_CLOEXEC;
        let fd = unsafe { libc::open(proc_path(real.as_raw_fd()).as_ptr(), flags) };
        if fd < 0 { Err(last()) } else { Ok(unsafe { File::from_raw_fd(fd) }) }
    }

    /// The backing file for an inode's contents, registered with the kernel
    /// the first time it is opened. Its real file is opened read-write where
    /// it is in the upper, since every later open of the inode shares it.
    fn backing(&self, ino: u64, node: &Node, register: impl Fn(&File) -> io::Result<BackingId>) -> Result<Arc<BackingId>, Errno> {
        let mut b = self.backings.lock();
        if let Some(id) = b.get(&ino).and_then(Weak::upgrade) {
            return Ok(id);
        }
        let real = node.real();
        let path = proc_path(real.as_raw_fd());
        let mut fd = -1;
        if node.upper().is_some() {
            fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        }
        if fd < 0 {
            fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
        }
        if fd < 0 {
            return Err(last());
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let id = Arc::new(register(&file).map_err(errno)?);
        b.insert(ino, Arc::downgrade(&id));
        Ok(id)
    }

    /// Whether an inode has a backing file registered that is not its
    /// upper, which a write open, having copied it up, cannot replace.
    fn stale_backing(&self, ino: u64) -> bool {
        self.backings.lock().get(&ino).and_then(Weak::upgrade).is_some()
    }

    /// Makes what a request created belong to whoever asked, taking the
    /// directory's group where it has set-group-ID.
    fn chown_new(&self, req: &Request, dir: RawFd, name: &CStr) -> Result<(), Errno> {
        // The router is root, so what it makes is root's, with the
        // directory's group or its own: already right for root.
        if req.uid() == 0 && req.gid() == 0 {
            return Ok(());
        }
        let pst = stat_fd(dir)?;
        let gid = if pst.st_mode & libc::S_ISGID != 0 { pst.st_gid } else { req.gid() };
        let r = unsafe { libc::fchownat(dir, name.as_ptr(), req.uid(), gid, libc::AT_SYMLINK_NOFOLLOW) };
        if r < 0 { Err(last()) } else { Ok(()) }
    }

    /// Prepares an upper directory to receive a new name: the parent is
    /// copied up, and a whiteout there is removed, returning whether there
    /// was one (a directory made in its place must be opaque).
    fn prepare_create(&self, pino: u64, parent: &Node, name: &CStr) -> Result<(Arc<OwnedFd>, bool), Errno> {
        let pu = self.copy_up(pino, parent)?;
        // Only a directory with something beneath it can hold a whiteout.
        let had = if parent.has_lower() { Self::clear_whiteout(pu.as_raw_fd(), name)? } else { false };
        Ok((pu, had))
    }

    /// The merged listing of a directory: the upper's names, then each
    /// lower's not already seen, without whiteouts or what they cover.
    fn merged(&self, node: &Node) -> Result<Vec<(CString, FileType)>, Errno> {
        let mut seen: HashSet<CString> = HashSet::new();
        let mut out = Vec::new();
        if let Some(u) = node.upper() {
            for (name, d) in list(u.as_raw_fd())? {
                if d == libc::DT_CHR {
                    if let Ok(fd) = open_path(u.as_raw_fd(), &name) {
                        if let Ok(st) = stat_fd(fd.as_raw_fd()) {
                            if whiteout(fd.as_raw_fd(), &st) {
                                seen.insert(name);
                                continue;
                            }
                        }
                    }
                }
                seen.insert(name.clone());
                out.push((name, dtype(d)));
            }
        }
        let lowers = node.lowers.read().clone();
        for l in lowers {
            for (name, d) in list(l.as_raw_fd())? {
                if seen.contains(&name) {
                    continue;
                }
                if d == libc::DT_CHR {
                    if let Ok(fd) = open_path(l.as_raw_fd(), &name) {
                        if let Ok(st) = stat_fd(fd.as_raw_fd()) {
                            if whiteout(fd.as_raw_fd(), &st) {
                                seen.insert(name);
                                continue;
                            }
                        }
                    }
                }
                seen.insert(name.clone());
                out.push((name, dtype(d)));
            }
        }
        Ok(out)
    }

    /// Forgets that a name led to the node for a file, removed or replaced.
    /// It stays in the table while the kernel holds it, but nothing will
    /// copy it up by this name again.
    fn detach(&self, st: &libc::stat, parent: u64, name: &CStr) {
        let t = self.table.read();
        let Some(n) = t.by_key.get(&(st.st_dev, st.st_ino)).and_then(|ino| t.nodes.get(ino)) else { return };
        let mut p = n.place.lock();
        if p.0 == parent && p.1.as_c_str() == name {
            *p = (u64::MAX, CString::default());
        }
    }

    /// Closing a file sends nothing: the router keeps no state a flush
    /// would settle, and its locks are on the open's own descriptor, which
    /// goes with the release.
    fn open_flags(&self, passthrough: bool) -> FopenFlags {
        if passthrough { FopenFlags::FOPEN_NOFLUSH } else { FopenFlags::FOPEN_KEEP_CACHE | FopenFlags::FOPEN_NOFLUSH }
    }

    /// Clears set-user-ID and set-group-ID on a file an unprivileged caller
    /// truncated: with FUSE_HANDLE_KILLPRIV_V2 that is the router's to do,
    /// and the router, being root, is exempt from the kernel's own clearing.
    fn kill_suid(&self, req: &Request, fd: RawFd, st: &mut libc::stat) {
        if req.uid() == 0 || st.st_mode & libc::S_IFMT != libc::S_IFREG {
            return;
        }
        let mode = st.st_mode & 0o7777;
        let mut keep = mode & !libc::S_ISUID;
        // A group-executable file loses set-group-ID too; without group
        // execute it is a mandatory-locking mark, which stays.
        if mode & libc::S_IXGRP != 0 {
            keep &= !libc::S_ISGID;
        }
        if keep != mode && chmod_fd(fd, keep).is_ok() {
            st.st_mode = (st.st_mode & libc::S_IFMT) | keep;
        }
    }
}

/// Copies a file's contents into another. `copy_file_range` is tried first
/// (it can share extents within one filesystem); between filesystems, as from
/// the image's virtiofs to the upper's ext4, it refuses, and the data is read
/// and written.
fn copy_data(src: &File, dst: &File, len: usize) -> Result<(), Errno> {
    let mut left = len;
    while left > 0 {
        let n = unsafe {
            libc::copy_file_range(src.as_raw_fd(), std::ptr::null_mut(), dst.as_raw_fd(), std::ptr::null_mut(), left, 0)
        };
        if n < 0 {
            let e = io::Error::last_os_error();
            if matches!(e.raw_os_error(), Some(libc::EXDEV) | Some(libc::EOPNOTSUPP) | Some(libc::EINVAL) | Some(libc::ENOSYS)) {
                break;
            }
            return Err(errno(e));
        }
        if n == 0 {
            return Ok(());
        }
        left -= n as usize;
    }
    let mut buf = vec![0u8; 1 << 20];
    while left > 0 {
        let n = unsafe { libc::read(src.as_raw_fd(), buf.as_mut_ptr() as *mut libc::c_void, buf.len().min(left)) };
        if n < 0 {
            return Err(last());
        }
        if n == 0 {
            break;
        }
        let mut done = 0;
        while done < n as usize {
            let w = unsafe { libc::write(dst.as_raw_fd(), buf[done..].as_ptr() as *const libc::c_void, n as usize - done) };
            if w < 0 {
                return Err(last());
            }
            done += w as usize;
        }
        left -= n as usize;
    }
    Ok(())
}

/// Copies a file's attributes onto another, as a copy-up must.
fn copy_xattrs(from: RawFd, to: RawFd) {
    let fp = proc_path(from);
    let tp = proc_path(to);
    let mut names = vec![0u8; 65536];
    let n = unsafe { libc::listxattr(fp.as_ptr(), names.as_mut_ptr() as *mut libc::c_char, names.len()) };
    if n <= 0 {
        return;
    }
    for name in names[..n as usize].split(|&c| c == 0).filter(|s| !s.is_empty()) {
        let Ok(cname) = CString::new(name) else { continue };
        let mut val = vec![0u8; 65536];
        let m = unsafe { libc::getxattr(fp.as_ptr(), cname.as_ptr(), val.as_mut_ptr() as *mut libc::c_void, val.len()) };
        if m < 0 {
            continue;
        }
        unsafe { libc::setxattr(tp.as_ptr(), cname.as_ptr(), val.as_ptr() as *const libc::c_void, m as usize, 0) };
    }
}

impl Filesystem for Router {
    fn init(&mut self, _req: &Request, config: &mut KernelConfig) -> io::Result<()> {
        let mut want = InitFlags::FUSE_POSIX_LOCKS
            | InitFlags::FUSE_FLOCK_LOCKS
            | InitFlags::FUSE_DO_READDIRPLUS
            | InitFlags::FUSE_READDIRPLUS_AUTO
            | InitFlags::FUSE_PARALLEL_DIROPS
            | InitFlags::FUSE_CACHE_SYMLINKS
            | InitFlags::FUSE_ATOMIC_O_TRUNC
            | InitFlags::FUSE_EXPORT_SUPPORT
            | InitFlags::FUSE_MAX_PAGES
            | InitFlags::FUSE_ASYNC_READ
            | InitFlags::FUSE_BIG_WRITES
            | InitFlags::FUSE_NO_OPENDIR_SUPPORT
            // The router clears set-ID bits itself (kill_suid; writes go to
            // ext4, which clears them), so the kernel need not ask for every
            // file's security.capability before each write.
            | InitFlags::FUSE_HANDLE_KILLPRIV_V2;
        if self.uring {
            want |= InitFlags::FUSE_OVER_IO_URING;
        }
        if self.passthrough {
            want |= InitFlags::FUSE_PASSTHROUGH;
        } else {
            want |= InitFlags::FUSE_WRITEBACK_CACHE;
        }
        if let Err(missing) = config.add_capabilities(want) {
            log::warn!("the kernel does not offer {missing:?}");
            let _ = config.add_capabilities(want - missing);
        }
        if self.passthrough {
            // The router's files are ext4's and virtiofs's, neither stacked,
            // so one level is enough, and leaves room for an overlay (Docker's
            // overlay2) on top of the router.
            let _ = config.set_max_stack_depth(1);
        }
        let _ = config.set_max_write(1 << 20);
        Ok(())
    }

    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let (p, name) = match (self.node(parent), cstr(name)) {
            (Ok(p), Ok(n)) => (p, n),
            (Err(e), _) | (_, Err(e)) => return reply.error(e),
        };
        self.entry(parent.0, &p, &name, reply);
    }

    fn forget(&self, _req: &Request, ino: INodeNo, nlookup: u64) {
        let gone = {
            let t = self.table.read();
            match t.nodes.get(&ino.0) {
                Some(n) => n.lookups.fetch_sub(nlookup, Ordering::Relaxed) == nlookup,
                None => false,
            }
        };
        if gone && ino != INodeNo::ROOT {
            let mut t = self.table.write();
            if let Some(n) = t.nodes.get(&ino.0) {
                if n.lookups.load(Ordering::Relaxed) == 0 {
                    let key = *n.key.lock();
                    t.nodes.remove(&ino.0);
                    if t.by_key.get(&key) == Some(&ino.0) {
                        t.by_key.remove(&key);
                    }
                }
            }
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        match self.node(ino).and_then(|n| stat_fd(n.real().as_raw_fd())) {
            Ok(st) => reply.attr(&self.ttl, &attr(ino.0, &st)),
            Err(e) => reply.error(e),
        }
    }

    fn setattr(
        &self,
        req: &Request,
        ino: INodeNo,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        fh: Option<FileHandle>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<fuser::BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        let node = match self.node(ino) {
            Ok(n) => n,
            Err(e) => return reply.error(e),
        };
        let res = (|| -> Result<libc::stat, Errno> {
            let up = self.copy_up(ino.0, &node)?;
            let fd = up.as_raw_fd();
            if let Some(mode) = mode {
                chmod_fd(fd, mode & 0o7777)?;
            }
            if uid.is_some() || gid.is_some() {
                let u = uid.unwrap_or(u32::MAX);
                let g = gid.unwrap_or(u32::MAX);
                if unsafe { libc::fchownat(fd, EMPTY.as_ptr(), u, g, libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW) } < 0 {
                    return Err(last());
                }
            }
            if let Some(size) = size {
                let r = match fh.and_then(|fh| self.handle(fh).ok()) {
                    Some((_h, ffd)) => unsafe { libc::ftruncate(ffd, size as i64) },
                    None => unsafe { libc::truncate(proc_path(fd).as_ptr(), size as i64) },
                };
                if r < 0 {
                    return Err(last());
                }
            }
            if atime.is_some() || mtime.is_some() {
                let conv = |t: Option<TimeOrNow>| match t {
                    None => libc::timespec { tv_sec: 0, tv_nsec: libc::UTIME_OMIT },
                    Some(TimeOrNow::Now) => libc::timespec { tv_sec: 0, tv_nsec: libc::UTIME_NOW },
                    Some(TimeOrNow::SpecificTime(t)) => {
                        let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
                        libc::timespec { tv_sec: d.as_secs() as i64, tv_nsec: d.subsec_nanos() as i64 }
                    }
                };
                let times = [conv(atime), conv(mtime)];
                let r = unsafe { libc::utimensat(fd, EMPTY.as_ptr(), times.as_ptr(), libc::AT_EMPTY_PATH) };
                if r < 0 {
                    return Err(last());
                }
            }
            let mut st = stat_fd(fd)?;
            if size.is_some() {
                self.kill_suid(req, fd, &mut st);
            }
            Ok(st)
        })();
        match res {
            Ok(st) => reply.attr(&self.ttl, &attr(ino.0, &st)),
            Err(e) => reply.error(e),
        }
    }

    fn readlink(&self, _req: &Request, ino: INodeNo, reply: ReplyData) {
        let node = match self.node(ino) {
            Ok(n) => n,
            Err(e) => return reply.error(e),
        };
        let mut buf = vec![0u8; libc::PATH_MAX as usize];
        let n = unsafe {
            libc::readlinkat(node.real().as_raw_fd(), EMPTY.as_ptr(), buf.as_mut_ptr() as *mut libc::c_char, buf.len())
        };
        if n < 0 {
            return reply.error(last());
        }
        reply.data(&buf[..n as usize]);
    }

    fn mknod(&self, req: &Request, parent: INodeNo, name: &OsStr, mode: u32, umask: u32, rdev: u32, reply: ReplyEntry) {
        let res = (|| -> Result<Arc<Node>, Errno> {
            let p = self.node(parent)?;
            let name = cstr(name)?;
            let (pu, _) = self.prepare_create(parent.0, &p, &name)?;
            if unsafe { libc::mknodat(pu.as_raw_fd(), name.as_ptr(), mode & !umask, rdev as libc::dev_t) } < 0 {
                return Err(last());
            }
            if mode & libc::S_IFMT == libc::S_IFCHR && rdev == 0 {
                // A client's 0/0 device (Docker's overlay2 makes them as its
                // own whiteouts) is a file, not the router's whiteout.
                let fd = open_path(pu.as_raw_fd(), &name)?;
                set_xattr(fd.as_raw_fd(), REAL, b"")?;
            }
            self.chown_new(req, pu.as_raw_fd(), &name)?;
            Ok(p)
        })();
        match res {
            Ok(p) => self.entry(parent.0, &p, &cstr(name).unwrap(), reply),
            Err(e) => reply.error(e),
        }
    }

    fn mkdir(&self, req: &Request, parent: INodeNo, name: &OsStr, mode: u32, umask: u32, reply: ReplyEntry) {
        let res = (|| -> Result<Arc<Node>, Errno> {
            let p = self.node(parent)?;
            let name = cstr(name)?;
            let (pu, had_whiteout) = self.prepare_create(parent.0, &p, &name)?;
            if unsafe { libc::mkdirat(pu.as_raw_fd(), name.as_ptr(), mode & !umask) } < 0 {
                return Err(last());
            }
            if had_whiteout || self.in_lower(&p, &name) {
                // What the lower had at this name was removed: none of it
                // shows through the new directory.
                let fd = open_path(pu.as_raw_fd(), &name)?;
                set_xattr(fd.as_raw_fd(), OPAQUE, b"y")?;
            }
            self.chown_new(req, pu.as_raw_fd(), &name)?;
            Ok(p)
        })();
        match res {
            Ok(p) => self.entry(parent.0, &p, &cstr(name).unwrap(), reply),
            Err(e) => reply.error(e),
        }
    }

    fn unlink(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let res = (|| -> Result<(), Errno> {
            let p = self.node(parent)?;
            let name = cstr(name)?;
            // It must be there, in some layer, to be removed.
            let (up, _, st) = self.resolve(&p, &name)?;
            let pu = self.copy_up(parent.0, &p)?;
            let covered = self.in_lower(&p, &name);
            match up {
                Some(_) => {
                    if covered {
                        // Replace it with a whiteout in one step.
                        let tmp = CString::new(format!(".hangar-wh-{}", self.next_fh.fetch_add(1, Ordering::Relaxed))).unwrap();
                        Self::make_whiteout(pu.as_raw_fd(), &tmp)?;
                        if unsafe { libc::renameat(pu.as_raw_fd(), tmp.as_ptr(), pu.as_raw_fd(), name.as_ptr()) } < 0 {
                            let e = last();
                            unsafe { libc::unlinkat(pu.as_raw_fd(), tmp.as_ptr(), 0) };
                            return Err(e);
                        }
                    } else if unsafe { libc::unlinkat(pu.as_raw_fd(), name.as_ptr(), 0) } < 0 {
                        return Err(last());
                    }
                }
                None => Self::make_whiteout(pu.as_raw_fd(), &name)?,
            }
            self.detach(&st, parent.0, &name);
            Ok(())
        })();
        match res {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn rmdir(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let res = (|| -> Result<(), Errno> {
            let p = self.node(parent)?;
            let name = cstr(name)?;
            let (ino, cst) = self.lookup_at(parent.0, &p, &name)?;
            let child = self.node(INodeNo(ino))?;
            let r = (|| {
                if !child.is_dir {
                    return Err(err(libc::ENOTDIR));
                }
                if !self.merged(&child)?.is_empty() {
                    return Err(err(libc::ENOTEMPTY));
                }
                let pu = self.copy_up(parent.0, &p)?;
                if let Some(cu) = child.upper() {
                    // Only whiteouts are left in it; they go with it.
                    for (n, _) in list(cu.as_raw_fd())? {
                        unsafe { libc::unlinkat(cu.as_raw_fd(), n.as_ptr(), 0) };
                    }
                    if unsafe { libc::unlinkat(pu.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } < 0 {
                        return Err(last());
                    }
                }
                if self.in_lower(&p, &name) {
                    Self::make_whiteout(pu.as_raw_fd(), &name)?;
                }
                Ok(())
            })();
            self.forget(_req, INodeNo(ino), 1);
            r?;
            self.detach(&cst, parent.0, &name);
            Ok(())
        })();
        match res {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn symlink(&self, req: &Request, parent: INodeNo, link_name: &OsStr, target: &Path, reply: ReplyEntry) {
        let res = (|| -> Result<Arc<Node>, Errno> {
            let p = self.node(parent)?;
            let name = cstr(link_name)?;
            let target = cstr(target.as_os_str())?;
            let (pu, _) = self.prepare_create(parent.0, &p, &name)?;
            if unsafe { libc::symlinkat(target.as_ptr(), pu.as_raw_fd(), name.as_ptr()) } < 0 {
                return Err(last());
            }
            self.chown_new(req, pu.as_raw_fd(), &name)?;
            Ok(p)
        })();
        match res {
            Ok(p) => self.entry(parent.0, &p, &cstr(link_name).unwrap(), reply),
            Err(e) => reply.error(e),
        }
    }

    fn rename(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        newparent: INodeNo,
        newname: &OsStr,
        flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        let res = (|| -> Result<(), Errno> {
            let p = self.node(parent)?;
            let np = self.node(newparent)?;
            let (name, newname) = (cstr(name)?, cstr(newname)?);
            let (ino, st) = self.lookup_at(parent.0, &p, &name)?;
            let src = self.node(INodeNo(ino))?;
            let r = (|| {
                if is_dir(&st) && src.has_lower() {
                    // Its lower contents cannot move with it; mv copies.
                    return Err(err(libc::EXDEV));
                }
                let noreplace = flags.bits() & libc::RENAME_NOREPLACE != 0;
                if noreplace && self.resolve(&np, &newname).is_ok() {
                    return Err(err(libc::EEXIST));
                }
                self.copy_up(ino, &src)?;
                let pu = self.copy_up(parent.0, &p)?;
                let npu = self.copy_up(newparent.0, &np)?;
                Self::clear_whiteout(npu.as_raw_fd(), &newname)?;
                let exchange = flags.bits() & libc::RENAME_EXCHANGE != 0;
                if exchange {
                    if let Ok((tino, _)) = self.lookup_at(newparent.0, &np, &newname) {
                        let t = self.node(INodeNo(tino))?;
                        self.copy_up(tino, &t)?;
                        self.forget(_req, INodeNo(tino), 1);
                    }
                }
                let replaced = if exchange { None } else { self.resolve(&np, &newname).ok().map(|(_, _, st)| st) };
                let client_whiteout = flags.bits() & libc::RENAME_WHITEOUT != 0;
                let mut f = flags.bits() & !libc::RENAME_WHITEOUT;
                let covered = !exchange && self.in_lower(&p, &name);
                if covered || client_whiteout {
                    f |= libc::RENAME_WHITEOUT;
                }
                if unsafe { libc::renameat2(pu.as_raw_fd(), name.as_ptr(), npu.as_raw_fd(), newname.as_ptr(), f) } < 0 {
                    return Err(last());
                }
                if client_whiteout {
                    // The client's whiteout, for an overlay of its own.
                    if let Ok(fd) = open_path(pu.as_raw_fd(), &name) {
                        let _ = set_xattr(fd.as_raw_fd(), REAL, b"");
                    }
                }
                if is_dir(&st) && self.in_lower(&np, &newname) {
                    if let Ok(fd) = open_path(npu.as_raw_fd(), &newname) {
                        set_xattr(fd.as_raw_fd(), OPAQUE, b"y")?;
                    }
                }
                if let Some(rst) = replaced {
                    self.detach(&rst, newparent.0, &newname);
                }
                *src.place.lock() = (newparent.0, newname.clone());
                Ok(())
            })();
            self.forget(_req, INodeNo(ino), 1);
            r
        })();
        match res {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn link(&self, _req: &Request, ino: INodeNo, newparent: INodeNo, newname: &OsStr, reply: ReplyEntry) {
        let res = (|| -> Result<Arc<Node>, Errno> {
            let n = self.node(ino)?;
            let np = self.node(newparent)?;
            let newname = cstr(newname)?;
            let up = self.copy_up(ino.0, &n)?;
            let (npu, _) = self.prepare_create(newparent.0, &np, &newname)?;
            let r = unsafe {
                libc::linkat(up.as_raw_fd(), EMPTY.as_ptr(), npu.as_raw_fd(), newname.as_ptr(), libc::AT_EMPTY_PATH)
            };
            if r < 0 { Err(last()) } else { Ok(np) }
        })();
        match res {
            Ok(np) => self.entry(newparent.0, &np, &cstr(newname).unwrap(), reply),
            Err(e) => reply.error(e),
        }
    }

    fn open(&self, req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        let node = match self.node(ino) {
            Ok(n) => n,
            Err(e) => return reply.error(e),
        };
        let writes = flags.0 & libc::O_ACCMODE != libc::O_RDONLY || flags.0 & libc::O_TRUNC != 0;
        if writes && node.upper().is_none() && self.stale_backing(ino.0) {
            log::warn!("inode {} is open from a lower layer and cannot be copied up for writing", ino.0);
            return reply.error(err(libc::EBUSY));
        }
        let file = match self.open_real(ino.0, &node, flags.0) {
            Ok(f) => f,
            Err(e) => return reply.error(e),
        };
        if flags.0 & libc::O_TRUNC != 0 {
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            if unsafe { libc::fstat(file.as_raw_fd(), &mut st) } == 0 {
                self.kill_suid(req, file.as_raw_fd(), &mut st);
            }
        }
        if self.passthrough {
            match self.backing(ino.0, &node, |f| reply.open_backing(f)) {
                Ok(id) => {
                    // Recorded before the reply: the next request for it
                    // (a lock, say) can arrive as soon as the reply is sent.
                    let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);
                    self.handles.write().insert(fh, Arc::new(Handle::File { file, _backing: Some(id.clone()) }));
                    reply.opened_passthrough(FileHandle(fh), self.open_flags(true), &id);
                    return;
                }
                Err(e) => log::warn!("passthrough refused for inode {}: {e:?}", ino.0),
            }
        }
        let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);
        self.handles.write().insert(fh, Arc::new(Handle::File { file, _backing: None }));
        reply.opened(FileHandle(fh), self.open_flags(false));
    }

    fn create(&self, req: &Request, parent: INodeNo, name: &OsStr, mode: u32, umask: u32, flags: i32, reply: ReplyCreate) {
        let res = (|| -> Result<(u64, libc::stat, File), Errno> {
            let p = self.node(parent)?;
            let name = cstr(name)?;
            let (pu, _) = self.prepare_create(parent.0, &p, &name)?;
            // Opened read-write whatever the caller asked, so the one
            // descriptor serves this open and the inode's passthrough
            // backing, which later opens share.
            let oflags = ((flags & !libc::O_ACCMODE) | libc::O_RDWR | libc::O_CREAT | libc::O_CLOEXEC) & !libc::O_NOCTTY;
            let fd = unsafe { libc::openat(pu.as_raw_fd(), name.as_ptr(), oflags, mode & !umask) };
            if fd < 0 {
                return Err(last());
            }
            let file = unsafe { File::from_raw_fd(fd) };
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            if unsafe { libc::fstat(fd, &mut st) } < 0 {
                return Err(last());
            }
            // It is the caller's, with the directory's group if that is
            // set-group-ID (which the file has already taken from it), and
            // the caller's otherwise.
            let gid = if st.st_gid != 0 { st.st_gid } else { req.gid() };
            if (st.st_uid, st.st_gid) != (req.uid(), gid) && unsafe { libc::fchown(fd, req.uid(), gid) } < 0 {
                return Err(last());
            }
            st.st_uid = req.uid();
            st.st_gid = gid;
            let path = open_path(pu.as_raw_fd(), &name)?;
            let ino = self.intern(parent.0, &name, Some(path), Vec::new(), &st);
            if let Ok(n) = self.node(INodeNo(ino)) {
                n.caps.store(NO_CAPS, Ordering::Relaxed);
            }
            Ok((ino, st, file))
        })();
        let (ino, st, file) = match res {
            Ok(x) => x,
            Err(e) => return reply.error(e),
        };
        let a = attr(ino, &st);
        if self.passthrough {
            match reply.open_backing(&file) {
                Ok(id) => {
                    let id = Arc::new(id);
                    self.backings.lock().insert(ino, Arc::downgrade(&id));
                    let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);
                    self.handles.write().insert(fh, Arc::new(Handle::File { file, _backing: Some(id.clone()) }));
                    reply.created_passthrough(&self.ttl, &a, Generation(0), FileHandle(fh), self.open_flags(true), &id);
                    return;
                }
                Err(e) => log::warn!("passthrough refused for a new file: {e:?}"),
            }
        }
        let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);
        self.handles.write().insert(fh, Arc::new(Handle::File { file, _backing: None }));
        reply.created(&self.ttl, &a, Generation(0), FileHandle(fh), self.open_flags(false));
    }

    fn read(&self, _req: &Request, _ino: INodeNo, fh: FileHandle, offset: u64, size: u32, _flags: OpenFlags, _lock_owner: Option<LockOwner>, reply: ReplyData) {
        let (_h, fd) = match self.handle(fh) {
            Ok(x) => x,
            Err(e) => return reply.error(e),
        };
        let mut buf = vec![0u8; size as usize];
        let n = unsafe { libc::pread(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), offset as i64) };
        if n < 0 {
            return reply.error(last());
        }
        reply.data(&buf[..n as usize]);
    }

    fn write(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: WriteFlags,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        let (_h, fd) = match self.handle(fh) {
            Ok(x) => x,
            Err(e) => return reply.error(e),
        };
        let n = unsafe { libc::pwrite(fd, data.as_ptr() as *const libc::c_void, data.len(), offset as i64) };
        if n < 0 {
            return reply.error(last());
        }
        reply.written(n as u32);
    }

    fn flush(&self, _req: &Request, _ino: INodeNo, _fh: FileHandle, _lock_owner: LockOwner, reply: ReplyEmpty) {
        reply.ok();
    }

    fn release(&self, _req: &Request, _ino: INodeNo, fh: FileHandle, _flags: OpenFlags, _lock_owner: Option<LockOwner>, _flush: bool, reply: ReplyEmpty) {
        self.handles.write().remove(&fh.0);
        reply.ok();
    }

    fn fsync(&self, _req: &Request, _ino: INodeNo, fh: FileHandle, datasync: bool, reply: ReplyEmpty) {
        let (_h, fd) = match self.handle(fh) {
            Ok(x) => x,
            Err(e) => return reply.error(e),
        };
        let r = unsafe { if datasync { libc::fdatasync(fd) } else { libc::fsync(fd) } };
        if r < 0 { reply.error(last()) } else { reply.ok() }
    }

    /// Directories are not opened: with `FUSE_NO_OPENDIR_SUPPORT`, this
    /// answer tells the kernel to stop asking, and it then keeps each
    /// directory's listing in its own cache, reading it from the router only
    /// to fill that cache.
    fn opendir(&self, _req: &Request, _ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        reply.error(err(libc::ENOSYS));
    }

    fn readdir(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64, mut reply: ReplyDirectory) {
        let entries = match self.node(ino).and_then(|n| self.merged(&n)) {
            Ok(e) => e,
            Err(e) => return reply.error(e),
        };
        for (i, (name, t)) in entries.iter().enumerate().skip(offset as usize) {
            if reply.add(INodeNo(u64::MAX), (i + 1) as u64, *t, OsStr::from_bytes(name.as_bytes())) {
                break;
            }
        }
        reply.ok();
    }

    fn readdirplus(&self, req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64, mut reply: ReplyDirectoryPlus) {
        let dir = match self.node(ino) {
            Ok(d) => d,
            Err(e) => return reply.error(e),
        };
        let entries = match self.merged(&dir) {
            Ok(e) => e,
            Err(e) => return reply.error(e),
        };
        for (i, (name, _t)) in entries.iter().enumerate().skip(offset as usize) {
            let Ok((cino, st)) = self.lookup_at(ino.0, &dir, name) else { continue };
            if reply.add(INodeNo(cino), (i + 1) as u64, OsStr::from_bytes(name.as_bytes()), &self.ttl, &attr(cino, &st), Generation(0)) {
                // Not sent: the lookup it counted is given back.
                self.forget(req, INodeNo(cino), 1);
                break;
            }
        }
        reply.ok();
    }

    fn releasedir(&self, _req: &Request, _ino: INodeNo, _fh: FileHandle, _flags: OpenFlags, reply: ReplyEmpty) {
        reply.ok();
    }

    fn fsyncdir(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, _datasync: bool, reply: ReplyEmpty) {
        let res = (|| -> Result<(), Errno> {
            let n = self.node(ino)?;
            if let Some(u) = n.upper() {
                let fd = unsafe { libc::open(proc_path(u.as_raw_fd()).as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) };
                if fd < 0 {
                    return Err(last());
                }
                let f = unsafe { File::from_raw_fd(fd) };
                if unsafe { libc::fsync(f.as_raw_fd()) } < 0 {
                    return Err(last());
                }
            }
            Ok(())
        })();
        match res {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: ReplyStatfs) {
        let root = match self.node(INodeNo::ROOT) {
            Ok(n) => n,
            Err(e) => return reply.error(e),
        };
        let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstatvfs(root.real().as_raw_fd(), &mut s) } < 0 {
            return reply.error(last());
        }
        reply.statfs(s.f_blocks, s.f_bfree, s.f_bavail, s.f_files, s.f_ffree, s.f_bsize as u32, s.f_namemax as u32, s.f_frsize as u32);
    }

    fn setxattr(&self, _req: &Request, ino: INodeNo, name: &OsStr, value: &[u8], flags: i32, _position: u32, reply: ReplyEmpty) {
        let res = (|| -> Result<(), Errno> {
            let n = self.node(ino)?;
            let name = escape(&cstr(name)?)?;
            let up = self.copy_up(ino.0, &n)?;
            let path = proc_path(up.as_raw_fd());
            let r = unsafe { libc::setxattr(path.as_ptr(), name.as_ptr(), value.as_ptr() as *const libc::c_void, value.len(), flags) };
            if name.as_bytes() == CAPS_NAME {
                // A new count, without the mark: whatever was read before
                // this is no longer known.
                let _ = n.caps.fetch_update(Ordering::AcqRel, Ordering::Relaxed, |v| Some((v | NO_CAPS) + 1));
            }
            if r < 0 { Err(last()) } else { Ok(()) }
        })();
        match res {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn getxattr(&self, _req: &Request, ino: INodeNo, name: &OsStr, size: u32, reply: ReplyXattr) {
        let res = (|| -> Result<(isize, Vec<u8>), Errno> {
            let n = self.node(ino)?;
            let name = cstr(name)?;
            if name.as_bytes().starts_with(HANGAR_PREFIX) {
                return Err(err(libc::ENODATA));
            }
            let caps = name.as_bytes() == CAPS_NAME;
            let seen = n.caps.load(Ordering::Acquire);
            if caps && seen & NO_CAPS != 0 {
                return Err(err(libc::ENODATA));
            }
            let name = escape(&name)?;
            let path = proc_path(n.real().as_raw_fd());
            let mut buf = vec![0u8; size as usize];
            let r = unsafe { libc::getxattr(path.as_ptr(), name.as_ptr(), buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if r < 0 {
                let e = last();
                if caps && e.code() == libc::ENODATA {
                    // Unless the attribute changed since it was read.
                    let _ = n.caps.compare_exchange(seen, seen | NO_CAPS, Ordering::AcqRel, Ordering::Relaxed);
                }
                return Err(e);
            }
            Ok((r, buf))
        })();
        match res {
            Ok((r, buf)) => {
                if size == 0 { reply.size(r as u32) } else { reply.data(&buf[..r as usize]) }
            }
            Err(e) => reply.error(e),
        }
    }

    fn listxattr(&self, _req: &Request, ino: INodeNo, size: u32, reply: ReplyXattr) {
        let res = (|| -> Result<Vec<u8>, Errno> {
            let n = self.node(ino)?;
            let path = proc_path(n.real().as_raw_fd());
            let mut buf = vec![0u8; 65536];
            let r = unsafe { libc::listxattr(path.as_ptr(), buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
            if r < 0 {
                return Err(last());
            }
            let mut out = Vec::new();
            for name in buf[..r as usize].split(|&c| c == 0).filter(|s| !s.is_empty()) {
                if let Some(shown) = unescape(name) {
                    out.extend_from_slice(&shown);
                    out.push(0);
                }
            }
            Ok(out)
        })();
        match res {
            Ok(out) => {
                if size == 0 {
                    reply.size(out.len() as u32)
                } else if out.len() > size as usize {
                    reply.error(err(libc::ERANGE))
                } else {
                    reply.data(&out)
                }
            }
            Err(e) => reply.error(e),
        }
    }

    fn removexattr(&self, _req: &Request, ino: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let res = (|| -> Result<(), Errno> {
            let n = self.node(ino)?;
            let name = escape(&cstr(name)?)?;
            let up = self.copy_up(ino.0, &n)?;
            let path = proc_path(up.as_raw_fd());
            if unsafe { libc::removexattr(path.as_ptr(), name.as_ptr()) } < 0 { Err(last()) } else { Ok(()) }
        })();
        match res {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn access(&self, _req: &Request, _ino: INodeNo, _mask: AccessFlags, reply: ReplyEmpty) {
        // With default_permissions the kernel decides access itself.
        reply.ok();
    }

    fn getlk(&self, _req: &Request, _ino: INodeNo, fh: FileHandle, _lock_owner: LockOwner, start: u64, end: u64, typ: i32, pid: u32, reply: ReplyLock) {
        let (_h, fd) = match self.handle(fh) {
            Ok(x) => x,
            Err(e) => return reply.error(e),
        };
        let mut fl = flock_for(start, end, typ);
        if unsafe { libc::fcntl(fd, libc::F_OFD_GETLK, &mut fl) } < 0 {
            return reply.error(last());
        }
        if fl.l_type == libc::F_UNLCK as i16 {
            reply.locked(0, 0, libc::F_UNLCK, pid);
        } else {
            let end = if fl.l_len == 0 { i64::MAX as u64 } else { (fl.l_start + fl.l_len - 1) as u64 };
            reply.locked(fl.l_start as u64, end, fl.l_type as i32, fl.l_pid as u32);
        }
    }

    fn setlk(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _lock_owner: LockOwner,
        start: u64,
        end: u64,
        typ: i32,
        _pid: u32,
        sleep: bool,
        flock: bool,
        reply: ReplyEmpty,
    ) {
        let (_h, fd) = match self.handle(fh) {
            Ok(x) => x,
            Err(e) => return reply.error(e),
        };
        // Each open has its own descriptor, so a flock() lock on it, and an
        // OFD lock for a POSIX one, belong to that open as they would on a
        // local filesystem. flock() allows an exclusive lock on a file opened
        // read-only, which a POSIX write lock does not.
        let r = if flock {
            let op = match typ {
                libc::F_RDLCK => libc::LOCK_SH,
                libc::F_WRLCK => libc::LOCK_EX,
                _ => libc::LOCK_UN,
            };
            unsafe { libc::flock(fd, if sleep { op } else { op | libc::LOCK_NB }) }
        } else {
            let fl = flock_for(start, end, typ);
            let cmd = if sleep { libc::F_OFD_SETLKW } else { libc::F_OFD_SETLK };
            unsafe { libc::fcntl(fd, cmd, &fl) }
        };
        if r < 0 { reply.error(last()) } else { reply.ok() }
    }

    fn fallocate(&self, _req: &Request, _ino: INodeNo, fh: FileHandle, offset: u64, length: u64, mode: i32, reply: ReplyEmpty) {
        let (_h, fd) = match self.handle(fh) {
            Ok(x) => x,
            Err(e) => return reply.error(e),
        };
        if unsafe { libc::fallocate(fd, mode, offset as i64, length as i64) } < 0 { reply.error(last()) } else { reply.ok() }
    }

    fn lseek(&self, _req: &Request, _ino: INodeNo, fh: FileHandle, offset: i64, whence: i32, reply: ReplyLseek) {
        let (_h, fd) = match self.handle(fh) {
            Ok(x) => x,
            Err(e) => return reply.error(e),
        };
        let r = unsafe { libc::lseek(fd, offset, whence) };
        if r < 0 { reply.error(last()) } else { reply.offset(r) }
    }

    fn copy_file_range(
        &self,
        _req: &Request,
        _ino_in: INodeNo,
        fh_in: FileHandle,
        offset_in: u64,
        _ino_out: INodeNo,
        fh_out: FileHandle,
        offset_out: u64,
        len: u64,
        _flags: CopyFileRangeFlags,
        reply: ReplyWrite,
    ) {
        let res = (|| -> Result<isize, Errno> {
            let (_a, fin) = self.handle(fh_in)?;
            let (_b, fout) = self.handle(fh_out)?;
            let mut oin = offset_in as i64;
            let mut oout = offset_out as i64;
            let n = unsafe { libc::copy_file_range(fin, &mut oin, fout, &mut oout, len as usize, 0) };
            if n < 0 { Err(last()) } else { Ok(n) }
        })();
        match res {
            Ok(n) => reply.written(n as u32),
            Err(e) => reply.error(e),
        }
    }
}

/// The kernel sends a lock's range as first and last byte, with "to the end
/// of the file" as the largest offset (`OFFSET_MAX`); `struct flock` wants a
/// start and a length, where 0 means to the end.
fn flock_for(start: u64, end: u64, typ: i32) -> libc::flock {
    let mut fl: libc::flock = unsafe { std::mem::zeroed() };
    fl.l_type = typ as i16;
    fl.l_whence = libc::SEEK_SET as i16;
    fl.l_start = start as i64;
    fl.l_len = if end >= i64::MAX as u64 { 0 } else { (end - start + 1) as i64 };
    fl
}
