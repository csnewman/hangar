//! The files root, reached only beneath itself: every path is opened a
//! component at a time without following symbolic links, so a link an
//! environment leaves in a copy cannot take the server outside the root,
//! as Go's os.Root keeps it.

use std::io::{self, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::path::Path;

use rustix::fs::{AtFlags, FileType, Mode, OFlags};

/// Who owns what is written: the environments' user, which every image
/// shares, as every file environments make there is when squashed to one
/// owner.
pub const UID: u32 = 1000;
pub const GID: u32 = 1000;

/// What a file is, as its directory says.
pub struct Info {
    pub size: u64,
    /// Its permission bits.
    pub mode: u32,
    pub modified: chrono::DateTime<chrono::Utc>,
    pub regular: bool,
}

pub struct Root(OwnedFd);

fn dir_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

/// The components of a path beneath the root, refusing any that would
/// leave it.
fn parts(rel: &str) -> io::Result<Vec<&str>> {
    let out: Vec<&str> = rel
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    if out.is_empty() || out.contains(&"..") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{rel:?} is not beneath the root"),
        ));
    }
    Ok(out)
}

// A stat's fields are of different widths on different platforms, so the
// casts below are needed on some and not on others.
#[allow(clippy::unnecessary_cast)]
fn info(st: &rustix::fs::Stat) -> Info {
    Info {
        size: st.st_size as u64,
        mode: st.st_mode as u32 & 0o7777,
        modified: chrono::DateTime::from_timestamp(st.st_mtime as i64, st.st_mtime_nsec as u32)
            .unwrap_or_default(),
        regular: FileType::from_raw_mode(st.st_mode as _) == FileType::RegularFile,
    }
}

/// Gives what was made to the environments' user; a server not run as
/// root cannot, and leaves it.
fn own(fd: impl AsFd) -> io::Result<()> {
    match rustix::fs::fchown(
        fd,
        Some(rustix::fs::Uid::from_raw(UID)),
        Some(rustix::fs::Gid::from_raw(GID)),
    ) {
        Ok(()) | Err(rustix::io::Errno::PERM) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

impl Root {
    /// Opens the root at path, made if it is not there.
    pub fn open(path: &Path) -> io::Result<Root> {
        std::fs::create_dir_all(path)?;
        Ok(Root(rustix::fs::open(path, dir_flags(), Mode::empty())?))
    }

    /// The directory holding rel's last component, made where it is not
    /// there when make, and that component's name.
    fn parent<'a>(&self, rel: &'a str, make: bool) -> io::Result<(OwnedFd, &'a str)> {
        let parts = parts(rel)?;
        let (leaf, dirs) = parts.split_last().expect("a component");
        let mut dir = self.0.try_clone()?;
        for d in dirs {
            dir = match rustix::fs::openat(&dir, *d, dir_flags(), Mode::empty()) {
                Ok(fd) => fd,
                Err(rustix::io::Errno::NOENT) if make => {
                    match rustix::fs::mkdirat(&dir, *d, Mode::from_raw_mode(0o755)) {
                        Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                        Err(e) => return Err(e.into()),
                    }
                    let fd = rustix::fs::openat(&dir, *d, dir_flags(), Mode::empty())?;
                    own(&fd)?;
                    fd
                }
                Err(e) => return Err(e.into()),
            };
        }
        Ok((dir, leaf))
    }

    /// What rel is, without following a link.
    pub fn lstat(&self, rel: &str) -> io::Result<Info> {
        let (dir, leaf) = self.parent(rel, false)?;
        Ok(info(&rustix::fs::statat(
            &dir,
            leaf,
            AtFlags::SYMLINK_NOFOLLOW,
        )?))
    }

    /// The regular file at rel, opened for reading.
    fn open_file(&self, rel: &str, flags: OFlags) -> io::Result<std::fs::File> {
        let (dir, leaf) = self.parent(rel, false)?;
        let fd = rustix::fs::openat(
            &dir,
            leaf,
            flags | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )?;
        let st = rustix::fs::fstat(&fd)?;
        if !info(&st).regular {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "not a regular file",
            ));
        }
        Ok(std::fs::File::from(fd))
    }

    /// The contents of the regular file at rel.
    pub fn read(&self, rel: &str) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        self.open_file(rel, OFlags::RDONLY)?.read_to_end(&mut out)?;
        Ok(out)
    }

    /// The regular file at rel, open for reading.
    pub fn open_read(&self, rel: &str) -> io::Result<std::fs::File> {
        self.open_file(rel, OFlags::RDONLY)
    }

    /// Makes a file at rel that is not there, open for reading and
    /// writing, private to the server; the directories it is in are made
    /// as needed.
    pub fn create(&self, rel: &str) -> io::Result<std::fs::File> {
        let (dir, leaf) = self.parent(rel, true)?;
        let fd = rustix::fs::openat(
            &dir,
            leaf,
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )?;
        Ok(std::fs::File::from(fd))
    }

    /// Puts the file at from in rel's place, as rel replaces would: given
    /// mode, owned by the environments' user, synced, then renamed over
    /// rel, so a reader sees it whole. Both are beneath the root, so the
    /// rename moves nothing.
    pub fn install(&self, from: &str, rel: &str, mode: u32) -> io::Result<()> {
        let f = self.open_file(from, OFlags::RDONLY)?;
        rustix::fs::fchmod(&f, Mode::from_raw_mode(mode as _))?;
        own(&f)?;
        f.sync_all()?;
        let (from_dir, from_leaf) = self.parent(from, false)?;
        let (dir, leaf) = self.parent(rel, true)?;
        Ok(rustix::fs::renameat(&from_dir, from_leaf, &dir, leaf)?)
    }

    /// Copies the regular file at from to rel, as replace writes, a
    /// buffer at a time.
    pub fn copy(&self, from: &str, rel: &str, mode: u32, staging: &str) -> io::Result<()> {
        let mut src = self.open_file(from, OFlags::RDONLY)?;
        let (dir, leaf) = self.parent(rel, true)?;
        let fd = rustix::fs::openat(
            &dir,
            staging,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(mode as _),
        )?;
        let written = (|| {
            rustix::fs::fchmod(&fd, Mode::from_raw_mode(mode as _))?;
            own(&fd)?;
            let mut f = std::fs::File::from(fd);
            io::copy(&mut src, &mut f)?;
            f.sync_all()?;
            rustix::fs::renameat(&dir, staging, &dir, leaf)?;
            Ok::<_, io::Error>(())
        })();
        if written.is_err() {
            let _ = rustix::fs::unlinkat(&dir, staging, AtFlags::empty());
        }
        written
    }

    /// Writes data beside rel and renames it over rel, so a reader sees it
    /// whole, with mode, owned by the environments' user; the directories
    /// it is in are made as needed.
    pub fn replace(&self, rel: &str, data: &[u8], mode: u32, staging: &str) -> io::Result<()> {
        let (dir, leaf) = self.parent(rel, true)?;
        let fd = rustix::fs::openat(
            &dir,
            staging,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(mode as _),
        )?;
        let written = (|| {
            rustix::fs::fchmod(&fd, Mode::from_raw_mode(mode as _))?;
            own(&fd)?;
            let mut f = std::fs::File::from(fd);
            f.write_all(data)?;
            f.sync_all()?;
            rustix::fs::renameat(&dir, staging, &dir, leaf)?;
            Ok::<_, io::Error>(())
        })();
        if written.is_err() {
            let _ = rustix::fs::unlinkat(&dir, staging, AtFlags::empty());
        }
        written
    }

    /// Changes the regular file at rel's mode.
    pub fn chmod(&self, rel: &str, mode: u32) -> io::Result<()> {
        let f = self.open_file(rel, OFlags::RDONLY)?;
        Ok(rustix::fs::fchmod(&f, Mode::from_raw_mode(mode as _))?)
    }

    /// Removes the file at rel; one not there is no error.
    pub fn remove(&self, rel: &str) -> io::Result<()> {
        let (dir, leaf) = match self.parent(rel, false) {
            Ok(x) => x,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        match rustix::fs::unlinkat(&dir, leaf, AtFlags::empty()) {
            Ok(()) | Err(rustix::io::Errno::NOENT) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Removes rel and everything under it; one not there is no error.
    pub fn remove_all(&self, rel: &str) -> io::Result<()> {
        let (dir, leaf) = match self.parent(rel, false) {
            Ok(x) => x,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        remove_tree(&dir, leaf)
    }

    /// The regular files under rel, each by its path beneath rel, in no
    /// order; none when rel is not there.
    pub fn files(&self, rel: &str) -> io::Result<Vec<(String, Info)>> {
        let (dir, leaf) = match self.parent(rel, false) {
            Ok(x) => x,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let top = match rustix::fs::openat(&dir, leaf, dir_flags(), Mode::empty()) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT) | Err(rustix::io::Errno::NOTDIR) => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        walk(&top, String::new(), &mut out)?;
        Ok(out)
    }
}

fn entries(dir: &OwnedFd) -> io::Result<Vec<(String, FileType)>> {
    let mut out = Vec::new();
    for e in rustix::fs::Dir::read_from(dir)? {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        if name != "." && name != ".." {
            out.push((name, e.file_type()));
        }
    }
    Ok(out)
}

fn walk(dir: &OwnedFd, prefix: String, out: &mut Vec<(String, Info)>) -> io::Result<()> {
    for (name, kind) in entries(dir)? {
        let path = format!("{prefix}{name}");
        match kind {
            FileType::Directory => {
                if let Ok(sub) = rustix::fs::openat(dir, name.as_str(), dir_flags(), Mode::empty())
                {
                    walk(&sub, format!("{path}/"), out)?;
                }
            }
            FileType::RegularFile | FileType::Unknown => {
                if let Ok(st) = rustix::fs::statat(dir, name.as_str(), AtFlags::SYMLINK_NOFOLLOW) {
                    let i = info(&st);
                    if i.regular {
                        out.push((path, i));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn remove_tree(dir: &OwnedFd, name: &str) -> io::Result<()> {
    match rustix::fs::openat(dir, name, dir_flags(), Mode::empty()) {
        Ok(sub) => {
            for (child, _) in entries(&sub)? {
                remove_tree(&sub, &child)?;
            }
            match rustix::fs::unlinkat(dir, name, AtFlags::REMOVEDIR) {
                Ok(()) | Err(rustix::io::Errno::NOENT) => Ok(()),
                Err(e) => Err(e.into()),
            }
        }
        Err(rustix::io::Errno::NOTDIR) | Err(rustix::io::Errno::LOOP) => {
            match rustix::fs::unlinkat(dir, name, AtFlags::empty()) {
                Ok(()) | Err(rustix::io::Errno::NOENT) => Ok(()),
                Err(e) => Err(e.into()),
            }
        }
        Err(rustix::io::Errno::NOENT) => Ok(()),
        Err(e) => Err(e.into()),
    }
}
