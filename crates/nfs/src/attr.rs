//! File attributes, as GETATTR gives them and SETATTR and creates set them.

use std::ffi::CString;
use std::io;
use std::sync::LazyLock;
use std::time::Duration;

use crate::fs::{MAX_NAME, errno, fstatat, split};
use crate::proto::*;
use crate::server::{Server, Squash};
use crate::sys::User;
use crate::xdr::{Bitmap, Reader, Writer};

/// How long a client's state lasts without its renewing it: a SEQUENCE, at
/// least every lease, from a client that is there.
pub const LEASE_TIME: Duration = Duration::from_secs(30);

/// The root's file ID, which no file on disk has.
const ROOT_FILE_ID: u64 = (1 << 63) - 1;

fn bitmap_of(attrs: &[u32]) -> Bitmap {
    let mut b = Bitmap::default();
    for a in attrs {
        b.set(*a as usize);
    }
    b
}

/// The attributes this server gives.
pub static SUPPORTED: LazyLock<Bitmap> = LazyLock::new(|| {
    bitmap_of(&[
        ATTR_SUPPORTED_ATTRS,
        ATTR_TYPE,
        ATTR_FH_EXPIRE_TYPE,
        ATTR_CHANGE,
        ATTR_SIZE,
        ATTR_LINK_SUPPORT,
        ATTR_SYMLINK_SUPPORT,
        ATTR_NAMED_ATTR,
        ATTR_FSID,
        ATTR_UNIQUE_HANDLES,
        ATTR_LEASE_TIME,
        ATTR_RDATTR_ERROR,
        ATTR_ACL_SUPPORT,
        ATTR_CAN_SET_TIME,
        ATTR_CASE_INSENSITIVE,
        ATTR_CASE_PRESERVING,
        ATTR_CHOWN_RESTRICTED,
        ATTR_FILEHANDLE,
        ATTR_FILE_ID,
        ATTR_FILES_AVAIL,
        ATTR_FILES_FREE,
        ATTR_FILES_TOTAL,
        ATTR_HOMOGENEOUS,
        ATTR_MAX_FILE_SIZE,
        ATTR_MAX_LINK,
        ATTR_MAX_NAME,
        ATTR_MAX_READ,
        ATTR_MAX_WRITE,
        ATTR_MODE,
        ATTR_NO_TRUNC,
        ATTR_NUM_LINKS,
        ATTR_OWNER,
        ATTR_OWNER_GROUP,
        ATTR_RAW_DEV,
        ATTR_SPACE_AVAIL,
        ATTR_SPACE_FREE,
        ATTR_SPACE_TOTAL,
        ATTR_SPACE_USED,
        ATTR_TIME_ACCESS,
        ATTR_TIME_ACCESS_SET,
        ATTR_TIME_DELTA,
        ATTR_TIME_METADATA,
        ATTR_TIME_MODIFY,
        ATTR_TIME_MODIFY_SET,
        ATTR_MOUNTED_ON_FILE_ID,
        ATTR_FS_LAYOUT_TYPES,
        ATTR_LAYOUT_BLK_SIZE,
        ATTR_SUPP_ATTR_EXCL_CREAT,
    ])
});

/// The attributes a SETATTR, or a create, may give.
static SETTABLE: LazyLock<Bitmap> = LazyLock::new(|| {
    bitmap_of(&[
        ATTR_SIZE,
        ATTR_MODE,
        ATTR_OWNER,
        ATTR_OWNER_GROUP,
        ATTR_TIME_ACCESS_SET,
        ATTR_TIME_MODIFY_SET,
    ])
});

/// Those an EXCLUSIVE4_1 create may give: all but the times, in which some
/// servers keep the verifier.
pub static EXCL_CREATABLE: LazyLock<Bitmap> =
    LazyLock::new(|| bitmap_of(&[ATTR_SIZE, ATTR_MODE, ATTR_OWNER, ATTR_OWNER_GROUP]));

/// A file's attributes, or the root's.
pub struct Node {
    pub root: bool,
    pub st: libc::stat,
    pub fh: Vec<u8>,
}

pub fn ftype(mode: u32) -> u32 {
    match mode & libc::S_IFMT as u32 {
        m if m == libc::S_IFDIR as u32 => NF4_DIR,
        m if m == libc::S_IFLNK as u32 => NF4_LNK,
        m if m == libc::S_IFBLK as u32 => NF4_BLK,
        m if m == libc::S_IFCHR as u32 => NF4_CHR,
        m if m == libc::S_IFSOCK as u32 => NF4_SOCK,
        m if m == libc::S_IFIFO as u32 => NF4_FIFO,
        _ => NF4_REG,
    }
}

/// A stat's times: access, change and modify, each seconds and
/// nanoseconds.
pub fn times(st: &libc::stat) -> [(i64, u32); 3] {
    #[cfg(target_os = "linux")]
    {
        [
            (st.st_atime, st.st_atime_nsec as u32),
            (st.st_ctime, st.st_ctime_nsec as u32),
            (st.st_mtime, st.st_mtime_nsec as u32),
        ]
    }
    #[cfg(target_os = "macos")]
    {
        [
            (st.st_atime, st.st_atime_nsec as u32),
            (st.st_ctime, st.st_ctime_nsec as u32),
            (st.st_mtime, st.st_mtime_nsec as u32),
        ]
    }
}

/// A file's change attribute: its ctime, which every change to it moves
/// on.
pub fn change(st: &libc::stat) -> u64 {
    let (sec, nsec) = times(st)[1];
    (sec as u64) << 32 | nsec as u64
}

/// A device number's major and minor, as this system packs them.
fn split_dev(dev: u64) -> (u32, u32) {
    #[cfg(target_os = "linux")]
    {
        (
            (((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff)) as u32,
            ((dev & 0xff) | ((dev >> 12) & !0xff)) as u32,
        )
    }
    #[cfg(target_os = "macos")]
    {
        (((dev >> 24) & 0xff) as u32, (dev & 0xff_ffff) as u32)
    }
}

/// A time a SETATTR gives: the server's own clock's, or the one given.
#[derive(Debug, Clone, Copy)]
pub struct SetTime {
    pub server: bool,
    pub sec: i64,
    pub nsec: u32,
}

/// What a SETATTR, or a create, gives.
#[derive(Debug, Clone, Default)]
pub struct SetAttrs {
    pub mask: Bitmap,
    pub size: Option<u64>,
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub atime: Option<SetTime>,
    pub mtime: Option<SetTime>,
}

/// Reads a fattr4 to set. An attribute that cannot be set, or is not
/// known, is refused: what follows it could not be read.
pub fn decode_attrs(r: &mut Reader) -> Result<SetAttrs, u32> {
    let mut a = SetAttrs {
        mask: r.bitmap(),
        ..Default::default()
    };
    let mut vals = Reader::new(r.opaque(usize::MAX >> 1));
    if r.failed {
        return Err(ERR_BAD_XDR);
    }
    for n in 0..a.mask.0.len() * 32 {
        if !a.mask.has(n) {
            continue;
        }
        if !SETTABLE.has(n) {
            return Err(if SUPPORTED.has(n) {
                ERR_INVAL
            } else {
                ERR_ATTR_NOT_SUPP
            });
        }
        match n as u32 {
            ATTR_SIZE => a.size = Some(vals.u64()),
            ATTR_MODE => a.mode = Some(vals.u32() & 0o7777),
            ATTR_OWNER | ATTR_OWNER_GROUP => {
                let id = parse_id(&vals.string(1024)).ok_or(ERR_BAD_OWNER)?;
                if n as u32 == ATTR_OWNER {
                    a.uid = Some(id);
                } else {
                    a.gid = Some(id);
                }
            }
            ATTR_TIME_ACCESS_SET | ATTR_TIME_MODIFY_SET => {
                let mut t = SetTime {
                    server: vals.u32() == 0,
                    sec: 0,
                    nsec: 0,
                };
                if !t.server {
                    t.sec = vals.i64();
                    t.nsec = vals.u32();
                }
                if n as u32 == ATTR_TIME_ACCESS_SET {
                    a.atime = Some(t);
                } else {
                    a.mtime = Some(t);
                }
            }
            _ => {}
        }
    }
    if vals.failed {
        return Err(ERR_BAD_XDR);
    }
    Ok(a)
}

/// An owner or group as AUTH_SYS clients give them: the number, with a
/// domain after an @ when one is configured.
fn parse_id(s: &[u8]) -> Option<u32> {
    let s = std::str::from_utf8(s).ok()?;
    let s = s.split_once('@').map_or(s, |(n, _)| n);
    s.parse().ok()
}

impl Server {
    /// The requested attributes of n that this server has.
    pub fn encode_attrs(&self, req: &Bitmap, n: &Node) -> (Bitmap, Vec<u8>) {
        let mut mask = req.and(&SUPPORTED);
        let mut w = Writer::default();
        let need_fs = [
            ATTR_FILES_AVAIL,
            ATTR_FILES_FREE,
            ATTR_FILES_TOTAL,
            ATTR_SPACE_AVAIL,
            ATTR_SPACE_FREE,
            ATTR_SPACE_TOTAL,
        ]
        .iter()
        .any(|a| mask.has(*a as usize));
        // SAFETY: fs is written by fstatfs, or left zero.
        let mut fs: libc::statfs = unsafe { std::mem::zeroed() };
        if need_fs {
            unsafe { libc::fstatfs(self.root.fd, &mut fs) };
        }
        let st = if n.root { self.root_stat() } else { n.st };
        let [atime, ctime, mtime] = times(&st);
        for a in 0..mask.0.len() * 32 {
            if !mask.has(a) {
                continue;
            }
            match a as u32 {
                ATTR_SUPPORTED_ATTRS => w.bitmap(&SUPPORTED),
                ATTR_TYPE => w.u32(ftype(st.st_mode as u32)),
                // Persistent, while the server runs.
                ATTR_FH_EXPIRE_TYPE => w.u32(0),
                ATTR_CHANGE => w.u64(change(&st)),
                ATTR_SIZE => w.u64(st.st_size as u64),
                ATTR_LINK_SUPPORT
                | ATTR_SYMLINK_SUPPORT
                | ATTR_CAN_SET_TIME
                | ATTR_CASE_PRESERVING
                | ATTR_CHOWN_RESTRICTED
                | ATTR_HOMOGENEOUS
                | ATTR_NO_TRUNC => w.bool(true),
                ATTR_NAMED_ATTR | ATTR_UNIQUE_HANDLES | ATTR_CASE_INSENSITIVE => w.bool(false),
                ATTR_FSID => {
                    w.u64(0x48414e47); // "HANG"
                    w.u64(0);
                }
                ATTR_LEASE_TIME => w.u32(LEASE_TIME.as_secs() as u32),
                ATTR_RDATTR_ERROR => w.u32(NFS_OK),
                ATTR_ACL_SUPPORT => w.u32(0),
                ATTR_FILEHANDLE => w.opaque(&n.fh),
                ATTR_FILE_ID | ATTR_MOUNTED_ON_FILE_ID => w.u64(st.st_ino),
                ATTR_FILES_AVAIL | ATTR_FILES_FREE => w.u64(fs.f_ffree),
                ATTR_FILES_TOTAL => w.u64(fs.f_files),
                ATTR_MAX_FILE_SIZE => w.u64((1 << 63) - 1),
                ATTR_MAX_LINK => w.u32(65000),
                ATTR_MAX_NAME => w.u32(MAX_NAME as u32),
                ATTR_MAX_READ | ATTR_MAX_WRITE => w.u64(MAX_IO as u64),
                ATTR_MODE => w.u32(st.st_mode as u32 & !(libc::S_IFMT as u32)),
                ATTR_NUM_LINKS => w.u32(st.st_nlink as u32),
                ATTR_OWNER => w.opaque(self.owner(st.st_uid, st.st_gid).uid.to_string().as_bytes()),
                ATTR_OWNER_GROUP => {
                    w.opaque(self.owner(st.st_uid, st.st_gid).gid.to_string().as_bytes())
                }
                ATTR_RAW_DEV => {
                    let (major, minor) = split_dev(st.st_rdev as u64);
                    w.u32(major);
                    w.u32(minor);
                }
                ATTR_SPACE_AVAIL => w.u64(fs.f_bavail * fs.f_bsize as u64),
                ATTR_SPACE_FREE => w.u64(fs.f_bfree * fs.f_bsize as u64),
                ATTR_SPACE_TOTAL => w.u64(fs.f_blocks * fs.f_bsize as u64),
                ATTR_SPACE_USED => w.u64(st.st_blocks as u64 * 512),
                ATTR_TIME_ACCESS => {
                    w.i64(atime.0);
                    w.u32(atime.1);
                }
                ATTR_TIME_METADATA => {
                    w.i64(ctime.0);
                    w.u32(ctime.1);
                }
                ATTR_TIME_MODIFY => {
                    w.i64(mtime.0);
                    w.u32(mtime.1);
                }
                ATTR_TIME_DELTA => {
                    w.i64(0);
                    w.u32(1);
                }
                ATTR_FS_LAYOUT_TYPES => w.u32(0),
                ATTR_LAYOUT_BLK_SIZE => w.u32(MAX_IO),
                ATTR_SUPP_ATTR_EXCL_CREAT => w.bitmap(&EXCL_CREATABLE),
                // Write-only: a GETATTR asking gets nothing for them.
                ATTR_TIME_ACCESS_SET | ATTR_TIME_MODIFY_SET => mask.0[a / 32] &= !(1 << (a % 32)),
                _ => {}
            }
        }
        mask.trim();
        (mask, w.b)
    }

    /// The root's attributes: a directory of the sets, made when the server
    /// started.
    fn root_stat(&self) -> libc::stat {
        // SAFETY: a zeroed stat is a valid one.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        st.st_mode = (libc::S_IFDIR | 0o555) as _;
        st.st_nlink = 2;
        st.st_ino = ROOT_FILE_ID;
        let (sec, nsec) = self.started;
        st.st_atime = sec;
        st.st_atime_nsec = nsec as _;
        st.st_ctime = sec;
        st.st_ctime_nsec = nsec as _;
        st.st_mtime = sec;
        st.st_mtime_nsec = nsec as _;
        st
    }

    /// Sets a's attributes on the file at p, giving which it set.
    pub fn apply(&self, p: &[u8], a: &SetAttrs) -> (Bitmap, u32) {
        let mut set = Bitmap::default();
        let (fd, name) = match self.root.at(p) {
            Ok(x) => x,
            Err(e) => return (set, errno(&e)),
        };
        let st = match fstatat(fd.0, name) {
            Ok(st) => st,
            Err(e) => return (set, errno(&e)),
        };
        let cname = CString::new(name).unwrap_or_default();
        let fmt = st.st_mode as u32 & libc::S_IFMT as u32;
        let link = fmt == libc::S_IFLNK as u32;
        let fail = |set: Bitmap| (set, errno(&io::Error::last_os_error()));
        if let Some(size) = a.size {
            if fmt != libc::S_IFREG as u32 {
                return (set, ERR_INVAL);
            }
            // SAFETY: cname is a valid NUL-terminated name.
            let f = unsafe {
                libc::openat(
                    fd.0,
                    cname.as_ptr(),
                    libc::O_WRONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if f < 0 {
                return fail(set);
            }
            let r = unsafe { libc::ftruncate(f, size as libc::off_t) };
            let truncated = if r < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            };
            unsafe { libc::close(f) };
            if let Err(e) = truncated {
                return (set, errno(&e));
            }
            set.set(ATTR_SIZE as usize);
        }
        // A set squashing every caller takes an owner as given and changes
        // nothing; another changes it as the caller may.
        if (a.uid.is_some() || a.gid.is_some()) && self.squash(split(p).0) != Squash::All {
            let uid = a.uid.unwrap_or(u32::MAX);
            let gid = a.gid.unwrap_or(u32::MAX);
            // SAFETY: cname is a valid NUL-terminated name.
            if unsafe { libc::fchownat(fd.0, cname.as_ptr(), uid, gid, libc::AT_SYMLINK_NOFOLLOW) }
                < 0
            {
                return fail(set);
            }
        }
        if a.uid.is_some() {
            set.set(ATTR_OWNER as usize);
        }
        if a.gid.is_some() {
            set.set(ATTR_OWNER_GROUP as usize);
        }
        if let Some(mode) = a.mode
            && !link
        {
            // SAFETY: cname is a valid NUL-terminated name.
            if unsafe { libc::fchmodat(fd.0, cname.as_ptr(), mode as libc::mode_t, 0) } < 0 {
                return fail(set);
            }
            set.set(ATTR_MODE as usize);
        }
        if a.atime.is_some() || a.mtime.is_some() {
            let spec = |t: Option<SetTime>| match t {
                None => libc::timespec {
                    tv_sec: 0,
                    tv_nsec: libc::UTIME_OMIT as _,
                },
                Some(t) if t.server => libc::timespec {
                    tv_sec: 0,
                    tv_nsec: libc::UTIME_NOW as _,
                },
                Some(t) => libc::timespec {
                    tv_sec: t.sec as _,
                    tv_nsec: t.nsec as _,
                },
            };
            let ts = [spec(a.atime), spec(a.mtime)];
            // SAFETY: ts holds the two times utimensat reads.
            if unsafe {
                libc::utimensat(fd.0, cname.as_ptr(), ts.as_ptr(), libc::AT_SYMLINK_NOFOLLOW)
            } < 0
            {
                return fail(set);
            }
            if a.atime.is_some() {
                set.set(ATTR_TIME_ACCESS_SET as usize);
            }
            if a.mtime.is_some() {
                set.set(ATTR_TIME_MODIFY_SET as usize);
            }
        }
        (set, NFS_OK)
    }

    /// Who a file belongs to, as clients are told: what the server owns when
    /// it cannot act as anyone else is the squashed user's.
    pub fn owner(&self, uid: u32, gid: u32) -> User {
        match &self.self_user {
            Some(me) if uid == me.uid => User {
                uid: self.uid,
                gid: self.gid,
                groups: vec![],
            },
            _ => User {
                uid,
                gid,
                groups: vec![],
            },
        }
    }
}
