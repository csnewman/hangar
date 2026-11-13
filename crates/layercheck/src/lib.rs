//! Finds what an environment's writable layer would hide of an image
//! upgrade: the overlay's upper directory is kept across it, so a file the
//! layer replaced or deleted stays as the layer has it, whatever the newer
//! image changed there. It reads the layer and both images without mounting
//! them: the layer from its ext4 disk ([`Ext4Layer`]), the images from their
//! EROFS files.

use std::io;

use hangar_erofs::{Image, Inode, Kind, ReadAt};

/// An overlay's upper directory as the check reads it: its tree, in which a
/// whiteout is a character device numbered 0:0, and whether overlayfs takes
/// a directory as opaque, hiding everything beneath it.
pub trait Layer {
    /// A directory's entries, by name, each with whether it is a
    /// directory; rel is from the layer's root, empty for the root.
    fn read_dir(&self, rel: &[u8]) -> io::Result<Vec<(Vec<u8>, bool)>>;

    /// Whether overlayfs takes the directory at rel as opaque.
    fn opaque(&self, rel: &[u8]) -> io::Result<bool>;
}

/// Where package managers record what is installed. A copy in an
/// environment's writable layer describes the image it was made over plus
/// what was installed on it, and hides the newer image's own.
pub const PACKAGE_DATABASES: [&str; 4] = [
    "var/lib/dpkg/status",
    "var/lib/rpm/rpmdb.sqlite",
    "var/lib/rpm/Packages",
    "usr/lib/sysimage/rpm/rpmdb.sqlite",
];

/// Left out of the comparison: scratch space, logs and caches, which nothing
/// depends on, and the stamps systemd compares with /usr to decide whether
/// to redo its post-update work (ldconfig among it), which an older stamp
/// over a newer image makes it do.
const IGNORED: [&str; 6] = [
    "tmp",
    "var/tmp",
    "var/log",
    "var/cache",
    "etc/.updated",
    "var/.updated",
];

/// What a layer would hide.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Found {
    /// The paths, from /, the layer replaces or deletes that the images
    /// differ in, and the directories it makes opaque whose entries they
    /// differ in, which end in "/".
    pub conflicts: Vec<String>,
    /// Whether one of them is a package manager's database.
    pub packages: bool,
}

/// Lists what in layer would hide a change between two copies of the image
/// beneath it. What the layer adds that neither copy has, or changes that
/// both copies have alike, hides nothing new. The layer is walked in order
/// of name.
pub fn compare<L: Layer, A: ReadAt, B: ReadAt>(
    layer: &L,
    older: &Image<A>,
    newer: &Image<B>,
) -> io::Result<Found> {
    let mut found = Found::default();
    walk(layer, older, newer, &[], &mut found)?;
    Ok(found)
}

fn walk<L: Layer, A: ReadAt, B: ReadAt>(
    layer: &L,
    older: &Image<A>,
    newer: &Image<B>,
    dir: &[u8],
    found: &mut Found,
) -> io::Result<()> {
    let mut ents = layer.read_dir(dir)?;
    ents.sort();
    for (name, is_dir) in ents {
        let rel = if dir.is_empty() {
            name
        } else {
            [dir, b"/", &name].concat()
        };
        let shown = String::from_utf8_lossy(&rel).into_owned();
        let at = |e: io::Error| io::Error::new(e.kind(), format!("{shown}: {e}"));
        if IGNORED.contains(&shown.as_str()) {
            continue;
        }
        if is_dir {
            if layer.opaque(&rel).map_err(at)? && entries_differ(older, newer, &rel).map_err(at)? {
                found.conflicts.push(format!("/{shown}/"));
            }
            walk(layer, older, newer, &rel, found)?;
            continue;
        }
        // A whiteout, which deletes the file beneath, is compared like
        // anything else: what matters is whether the copies differ in the
        // path it hides.
        if paths_differ(older, newer, &rel).map_err(at)? {
            found.conflicts.push(format!("/{shown}"));
            if PACKAGE_DATABASES.contains(&shown.as_str()) {
                found.packages = true;
            }
        }
    }
    Ok(())
}

/// Finds rel in an image, resolving it as the guest would -- an absolute
/// symlink stays inside the image -- without following its last component.
/// None for a path that is not there.
fn lookup<R: ReadAt>(im: &Image<R>, rel: &[u8]) -> io::Result<Option<Inode>> {
    match im.lookup(rel, false) {
        Ok(i) => Ok(Some(i)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Whether the two images differ at rel: one has it and the other not, or
/// they have different kinds of file, modes, owners, targets or contents.
fn paths_differ<A: ReadAt, B: ReadAt>(
    older: &Image<A>,
    newer: &Image<B>,
    rel: &[u8],
) -> io::Result<bool> {
    let (a, b) = match (lookup(older, rel)?, lookup(newer, rel)?) {
        (Some(a), Some(b)) => (a, b),
        (a, b) => return Ok(a.is_some() != b.is_some()),
    };
    if a.kind != b.kind || a.perm != b.perm || a.uid != b.uid || a.gid != b.gid {
        return Ok(true);
    }
    Ok(match a.kind {
        Kind::Symlink => older.readlink(&a)? != newer.readlink(&b)?,
        Kind::Regular => a.size != b.size || contents_differ(older, &a, newer, &b)?,
        Kind::CharDevice | Kind::BlockDevice => a.major != b.major || a.minor != b.minor,
        _ => false,
    })
}

fn contents_differ<A: ReadAt, B: ReadAt>(
    older: &Image<A>,
    a: &Inode,
    newer: &Image<B>,
    b: &Inode,
) -> io::Result<bool> {
    let mut ba = vec![0u8; 64 << 10];
    let mut bb = vec![0u8; 64 << 10];
    let mut off = 0;
    while off < a.size {
        let na = older.read_at(a, &mut ba, off)?;
        let nb = newer.read_at(b, &mut bb, off)?;
        if na != nb || ba[..na] != bb[..nb] {
            return Ok(true);
        }
        off += na as u64;
    }
    Ok(false)
}

/// Whether the two images' directories at rel hold different names, or one
/// has the directory and the other not.
fn entries_differ<A: ReadAt, B: ReadAt>(
    older: &Image<A>,
    newer: &Image<B>,
    rel: &[u8],
) -> io::Result<bool> {
    fn names<R: ReadAt>(im: &Image<R>, rel: &[u8]) -> io::Result<Option<Vec<Vec<u8>>>> {
        match lookup(im, rel)? {
            Some(d) if d.kind == Kind::Dir => {
                let mut out: Vec<Vec<u8>> = im.read_dir(&d)?.into_iter().map(|e| e.name).collect();
                out.sort();
                Ok(Some(out))
            }
            _ => Ok(None),
        }
    }
    Ok(names(older, rel)? != names(newer, rel)?)
}

/// The upper directory at dir of an ext4 filesystem, the disk an
/// environment's writable layer is kept on.
pub struct Ext4Layer<'a, R: hangar_ext4::ReadAt> {
    fs: &'a hangar_ext4::Fs<R>,
    dir: Vec<u8>,
}

impl<'a, R: hangar_ext4::ReadAt> Ext4Layer<'a, R> {
    pub fn new(fs: &'a hangar_ext4::Fs<R>, dir: &[u8]) -> Self {
        Ext4Layer {
            fs,
            dir: dir.to_vec(),
        }
    }

    fn path(&self, rel: &[u8]) -> Vec<u8> {
        if rel.is_empty() {
            self.dir.clone()
        } else {
            [&self.dir[..], b"/", rel].concat()
        }
    }
}

impl<R: hangar_ext4::ReadAt> Layer for Ext4Layer<'_, R> {
    fn read_dir(&self, rel: &[u8]) -> io::Result<Vec<(Vec<u8>, bool)>> {
        let dir = self.fs.lookup(&self.path(rel), false)?;
        self.fs
            .read_dir(&dir)?
            .into_iter()
            .map(|e| {
                let kind = self.fs.inode(e.ino)?.kind;
                Ok((e.name, kind == hangar_ext4::Kind::Dir))
            })
            .collect()
    }

    /// Reads overlayfs's mark, trusted.overlay.opaque set to "y".
    fn opaque(&self, rel: &[u8]) -> io::Result<bool> {
        let inode = self.fs.lookup(&self.path(rel), false)?;
        let attrs = self.fs.xattrs(&inode)?;
        Ok(attrs.get(&b"trusted.overlay.opaque"[..]).map(Vec::as_slice) == Some(&b"y"[..]))
    }
}
