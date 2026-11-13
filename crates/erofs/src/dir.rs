//! Adding a directory tree as it is on disk.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::Path;

use crate::format::Kind;
use crate::writer::{Entry, Out, Writer};

impl<O: Out> Writer<O> {
    /// Adds the tree under root, which becomes the image's root, as it is
    /// on disk: owners, modes, times, hard links and extended attributes.
    /// Each directory comes before what it holds, which comes in order of
    /// name, so a tree's files' data is laid out the same each time.
    pub fn add_dir(&mut self, root: &Path) -> io::Result<()> {
        let mut seen = HashMap::new();
        self.walk(root, b".".to_vec(), &mut seen)
    }

    fn walk(
        &mut self,
        path: &Path,
        rel: Vec<u8>,
        seen: &mut HashMap<(u64, u64), Vec<u8>>,
    ) -> io::Result<()> {
        let with = |e: io::Error| io::Error::new(e.kind(), format!("{}: {e}", path.display()));
        let meta = fs::symlink_metadata(path).map_err(with)?;
        let mut e = Entry {
            path: rel.clone(),
            perm: meta.mode() & 0o7777,
            uid: meta.uid(),
            gid: meta.gid(),
            mtime: meta.mtime(),
            nsec: meta.mtime_nsec() as u32,
            xattrs: read_xattrs(path).map_err(with)?,
            ..Default::default()
        };
        let ft = meta.file_type();
        if !ft.is_dir() && meta.nlink() > 1 {
            let id = (meta.dev(), meta.ino());
            if let Some(first) = seen.get(&id) {
                return self.link(&rel, first);
            }
            seen.insert(id, rel.clone());
        }
        if ft.is_file() {
            e.kind = Some(Kind::Regular);
            e.size = meta.len();
            let mut f = fs::File::open(path).map_err(with)?;
            return self.add_file(&e, &mut f);
        }
        if ft.is_dir() {
            e.kind = Some(Kind::Dir);
            self.add(&e, &mut io::empty())?;
            let mut names: Vec<_> = fs::read_dir(path)
                .map_err(with)?
                .map(|d| d.map(|d| d.file_name()))
                .collect::<Result<_, _>>()
                .map_err(with)?;
            names.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
            for name in names {
                let child = if rel == b"." {
                    name.as_bytes().to_vec()
                } else {
                    [&rel[..], b"/", name.as_bytes()].concat()
                };
                self.walk(&path.join(&name), child, seen)?;
            }
            return Ok(());
        }
        if ft.is_symlink() {
            e.kind = Some(Kind::Symlink);
            e.target = fs::read_link(path)
                .map_err(with)?
                .as_os_str()
                .as_bytes()
                .to_vec();
        } else if ft.is_char_device() || ft.is_block_device() {
            e.kind = Some(if ft.is_char_device() {
                Kind::CharDevice
            } else {
                Kind::BlockDevice
            });
            (e.major, e.minor) = split_dev(meta.rdev());
        } else if ft.is_fifo() {
            e.kind = Some(Kind::Fifo);
        } else if ft.is_socket() {
            e.kind = Some(Kind::Socket);
        } else {
            return Err(with(io::Error::other("a file of a kind EROFS has none of")));
        }
        self.add(&e, &mut io::empty())
    }
}

/// A device number's major and minor, as this system packs them.
fn split_dev(dev: u64) -> (u32, u32) {
    #[cfg(target_os = "linux")]
    {
        let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff);
        let minor = (dev & 0xff) | ((dev >> 12) & !0xff);
        (major as u32, minor as u32)
    }
    #[cfg(not(target_os = "linux"))]
    {
        (((dev >> 24) & 0xff) as u32, (dev & 0xff_ffff) as u32)
    }
}

/// A file's extended attributes, not following a symlink.
#[cfg(target_os = "linux")]
fn read_xattrs(path: &Path) -> io::Result<BTreeMap<Vec<u8>, Vec<u8>>> {
    use std::ffi::CString;
    let mut out = BTreeMap::new();
    let c = CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: c is a valid NUL-terminated path; a null buffer of size 0
    // asks only for the list's size.
    let size = unsafe { libc::llistxattr(c.as_ptr(), std::ptr::null_mut(), 0) };
    if size < 0 {
        let e = io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::ENOTSUP) {
            return Ok(out);
        }
        return Err(e);
    }
    if size == 0 {
        return Ok(out);
    }
    let mut list = vec![0u8; size as usize];
    // SAFETY: list is a writable buffer of the given length.
    let size = unsafe { libc::llistxattr(c.as_ptr(), list.as_mut_ptr().cast(), list.len()) };
    if size < 0 {
        return Err(io::Error::last_os_error());
    }
    for name in list[..size as usize]
        .split(|b| *b == 0)
        .filter(|n| !n.is_empty())
    {
        let cname = CString::new(name)?;
        // SAFETY: as above, asking only for the value's size.
        let n = unsafe { libc::lgetxattr(c.as_ptr(), cname.as_ptr(), std::ptr::null_mut(), 0) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::ENODATA) {
                continue;
            }
            return Err(e);
        }
        let mut v = vec![0u8; n as usize];
        // SAFETY: v is a writable buffer of the given length.
        let n =
            unsafe { libc::lgetxattr(c.as_ptr(), cname.as_ptr(), v.as_mut_ptr().cast(), v.len()) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        v.truncate(n as usize);
        out.insert(name.to_vec(), v);
    }
    Ok(out)
}

/// Images are built from Linux trees, whose attributes only Linux reads as
/// they are.
#[cfg(not(target_os = "linux"))]
fn read_xattrs(_: &Path) -> io::Result<BTreeMap<Vec<u8>, Vec<u8>>> {
    Ok(BTreeMap::new())
}
