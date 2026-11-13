//! Reading an image without mounting it: for hosts that cannot mount EROFS,
//! and for reading one the worker made.

use std::collections::BTreeMap;
use std::io;

use crate::format::*;

/// Where an image is read from: anything bytes can be read at an offset.
pub trait ReadAt {
    /// Reads up to buf's length at offset; fewer only at the end.
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize>;
}

#[cfg(unix)]
impl ReadAt for std::fs::File {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        let mut n = 0;
        while n < buf.len() {
            match std::os::unix::fs::FileExt::read_at(self, &mut buf[n..], offset + n as u64)? {
                0 => break,
                k => n += k,
            }
        }
        Ok(n)
    }
}

impl ReadAt for [u8] {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        let start = (offset as usize).min(self.len());
        let n = buf.len().min(self.len() - start);
        buf[..n].copy_from_slice(&self[start..start + n]);
        Ok(n)
    }
}

impl ReadAt for Vec<u8> {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        self.as_slice().read_at(buf, offset)
    }
}

fn bad(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

fn not_found() -> io::Error {
    io::Error::from(io::ErrorKind::NotFound)
}

/// The incompatible features that move where things are, which this reader
/// does not follow.
const INCOMPAT_48BIT: u32 = 0x80;
const INCOMPAT_METABOX: u32 = 0x100;

/// How many symlinks resolving one path may pass through.
const MAX_SYMLINKS: usize = 40;

/// An EROFS image to read: uncompressed, as [`crate::Writer`] and
/// mkfs.erofs make by default.
pub struct Image<R: ReadAt> {
    r: R,
    block_bits: u8,
    meta_start: u64,
    xattr_start: u64,
    root_nid: u64,
    epoch: i64,
    fixed_nsec: u32,
}

/// One thing in the image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inode {
    pub nid: u64,
    pub kind: Kind,
    pub perm: u32,
    pub uid: u32,
    pub gid: u32,
    pub mtime: i64,
    pub nsec: u32,
    pub size: u64,
    pub nlink: u32,
    /// A device's number.
    pub major: u32,
    pub minor: u32,

    layout: u8,
    start: u32,
    loc: u64,
    isize: u64,
    xsize: u64,
}

/// One entry of a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: Vec<u8>,
    pub nid: u64,
    pub kind: Option<Kind>,
}

impl<R: ReadAt> Image<R> {
    /// Reads an image's superblock.
    pub fn open(r: R) -> io::Result<Image<R>> {
        let mut sb = [0u8; SUPER_SIZE];
        if r.read_at(&mut sb, SUPER_OFFSET)? < SUPER_SIZE {
            return Err(bad("reading the superblock: the image is too short".into()));
        }
        if le32(&sb[0..]) != SUPER_MAGIC {
            return Err(bad("not an EROFS image".into()));
        }
        let bits = sb[12];
        if !(9..=16).contains(&bits) {
            return Err(bad(format!("a block size of 2^{bits}")));
        }
        let f = le32(&sb[80..]) & (INCOMPAT_48BIT | INCOMPAT_METABOX);
        if f != 0 {
            return Err(bad(format!(
                "an image with features this reader does not follow ({f:#x})"
            )));
        }
        Ok(Image {
            block_bits: bits,
            root_nid: le16(&sb[14..]) as u64,
            epoch: le64(&sb[24..]) as i64,
            fixed_nsec: le32(&sb[32..]),
            meta_start: (le32(&sb[40..]) as u64) << bits,
            xattr_start: (le32(&sb[44..]) as u64) << bits,
            r,
        })
    }

    fn block_size(&self) -> u64 {
        1 << self.block_bits
    }

    /// The root directory.
    pub fn root(&self) -> io::Result<Inode> {
        self.inode(self.root_nid)
    }

    /// Reads the inode numbered nid.
    pub fn inode(&self, nid: u64) -> io::Result<Inode> {
        let loc = self.meta_start + (nid << SLOT_BITS);
        let mut b = [0u8; EXTENDED_SIZE];
        let n = self.r.read_at(&mut b, loc)?;
        if n < COMPACT_SIZE {
            return Err(bad(format!("reading inode {nid}: the image ends")));
        }
        let format = le16(&b[0..]);
        let layout = ((format >> 1) & 0x7) as u8;
        let mode = le16(&b[4..]);
        let Some(kind) = Kind::of_mode(mode) else {
            return Err(bad(format!("inode {nid}: a mode of {mode:o}")));
        };
        let mut inode = Inode {
            nid,
            kind,
            perm: (mode & 0o7777) as u32,
            uid: 0,
            gid: 0,
            mtime: 0,
            nsec: 0,
            size: 0,
            nlink: 0,
            major: 0,
            minor: 0,
            layout,
            start: 0,
            loc,
            isize: 0,
            xsize: xattr_body_size(le16(&b[2..])),
        };
        let iu;
        if format & 1 == VERSION_EXTENDED {
            if n < EXTENDED_SIZE {
                return Err(bad(format!("reading inode {nid}: the image ends")));
            }
            inode.isize = EXTENDED_SIZE as u64;
            inode.size = le64(&b[8..]);
            iu = le32(&b[16..]);
            inode.uid = le32(&b[24..]);
            inode.gid = le32(&b[28..]);
            inode.mtime = le64(&b[32..]) as i64;
            inode.nsec = le32(&b[40..]);
            inode.nlink = le32(&b[44..]);
        } else {
            inode.isize = COMPACT_SIZE as u64;
            inode.size = le32(&b[8..]) as u64;
            iu = le32(&b[16..]);
            inode.uid = le16(&b[24..]) as u32;
            inode.gid = le16(&b[26..]) as u32;
            inode.mtime = self.epoch + le32(&b[12..]) as i64;
            inode.nsec = self.fixed_nsec;
            inode.nlink = if kind != Kind::Dir && format & (1 << 4) != 0 {
                1
            } else {
                le16(&b[6..]) as u32
            };
        }
        match kind {
            Kind::CharDevice | Kind::BlockDevice => {
                (inode.major, inode.minor) = decode_dev(iu);
            }
            Kind::Regular | Kind::Dir | Kind::Symlink => {
                inode.start = iu;
                if layout != LAYOUT_PLAIN && layout != LAYOUT_INLINE {
                    return Err(bad(format!(
                        "inode {nid}: a data layout this reader does not follow ({layout})"
                    )));
                }
            }
            _ => {}
        }
        Ok(inode)
    }

    /// Reads up to buf's length of an inode's contents at offset: a file's
    /// data, a symlink's target, a directory's entries as they are stored.
    pub fn read_at(&self, inode: &Inode, mut buf: &mut [u8], mut offset: u64) -> io::Result<usize> {
        if offset >= inode.size {
            return Ok(0);
        }
        let rest = inode.size - offset;
        if buf.len() as u64 > rest {
            buf = &mut buf[..rest as usize];
        }
        let bs = self.block_size();
        // The whole blocks, from its start; with the inline layout, what
        // follows them is kept with the inode.
        let whole = if inode.layout == LAYOUT_INLINE {
            inode.size.div_ceil(bs) * bs - bs
        } else {
            inode.size
        };
        let mut n = 0;
        while !buf.is_empty() {
            let k = if offset < whole && inode.start == NULL_ADDR {
                let k = (buf.len() as u64).min(whole - offset) as usize;
                buf[..k].fill(0);
                k
            } else if offset < whole {
                let k = (buf.len() as u64).min(whole - offset) as usize;
                self.r.read_at(
                    &mut buf[..k],
                    ((inode.start as u64) << self.block_bits) + offset,
                )?
            } else {
                self.r
                    .read_at(buf, inode.loc + inode.isize + inode.xsize + offset - whole)?
            };
            if k == 0 {
                return Err(bad(format!(
                    "inode {}: its data runs past the image",
                    inode.nid
                )));
            }
            n += k;
            offset += k as u64;
            buf = &mut buf[k..];
        }
        Ok(n)
    }

    /// An inode's whole contents.
    pub fn read_all(&self, inode: &Inode) -> io::Result<Vec<u8>> {
        let mut out = vec![0u8; inode.size as usize];
        self.read_at(inode, &mut out, 0)?;
        Ok(out)
    }

    /// A symlink's target.
    pub fn readlink(&self, inode: &Inode) -> io::Result<Vec<u8>> {
        if inode.kind != Kind::Symlink {
            return Err(bad(format!("inode {} is not a symlink", inode.nid)));
        }
        self.read_all(inode)
    }

    /// A directory's entries, in the order they are stored -- by name --
    /// without "." and "..".
    pub fn read_dir(&self, inode: &Inode) -> io::Result<Vec<DirEntry>> {
        Ok(self
            .dirents(inode)?
            .into_iter()
            .filter(|e| e.name != b"." && e.name != b"..")
            .collect())
    }

    /// Every entry of a directory, "." and ".." among them.
    fn dirents(&self, inode: &Inode) -> io::Result<Vec<DirEntry>> {
        if inode.kind != Kind::Dir {
            return Err(bad(format!("inode {} is not a directory", inode.nid)));
        }
        let data = self.read_all(inode)?;
        let bs = self.block_size() as usize;
        let mut out = vec![];
        let mut start = 0;
        while start < data.len() {
            let block = &data[start..(start + bs).min(data.len())];
            start += bs;
            if block.len() < DIRENT_SIZE {
                return Err(bad(format!("directory {}: a short block", inode.nid)));
            }
            let count = le16(&block[8..]) as usize / DIRENT_SIZE;
            if count == 0 || count * DIRENT_SIZE > block.len() {
                return Err(bad(format!(
                    "directory {}: a bad first name offset",
                    inode.nid
                )));
            }
            for i in 0..count {
                let r = &block[i * DIRENT_SIZE..];
                let off = le16(&r[8..]) as usize;
                let end = if i + 1 < count {
                    le16(&block[(i + 1) * DIRENT_SIZE + 8..]) as usize
                } else {
                    block.len()
                };
                if off > end || end > block.len() {
                    return Err(bad(format!("directory {}: a bad name offset", inode.nid)));
                }
                let mut name = &block[off..end];
                if i + 1 == count
                    && let Some(z) = name.iter().position(|b| *b == 0)
                {
                    name = &name[..z];
                }
                out.push(DirEntry {
                    name: name.to_vec(),
                    nid: le64(r),
                    kind: Kind::of_dirent(r[10]),
                });
            }
        }
        Ok(out)
    }

    /// An inode's extended attributes, by their full names.
    pub fn xattrs(&self, inode: &Inode) -> io::Result<BTreeMap<Vec<u8>, Vec<u8>>> {
        let mut out = BTreeMap::new();
        if inode.xsize == 0 {
            return Ok(out);
        }
        let mut body = vec![0u8; inode.xsize as usize];
        if self.r.read_at(&mut body, inode.loc + inode.isize)? < body.len() {
            return Err(bad(format!(
                "reading inode {}'s attributes: the image ends",
                inode.nid
            )));
        }
        let shared = body[4] as usize;
        let mut pos = XATTR_HEADER_SIZE + 4 * shared;
        if pos > body.len() {
            return Err(bad(format!("inode {}: a bad attribute header", inode.nid)));
        }
        for i in 0..shared {
            let id = le32(&body[XATTR_HEADER_SIZE + 4 * i..]);
            let at = self.xattr_start + id as u64 * 4;
            let mut head = [0u8; 4];
            self.r.read_at(&mut head, at)?;
            let mut e = vec![0u8; 4 + head[0] as usize + le16(&head[2..]) as usize];
            self.r.read_at(&mut e, at)?;
            add_xattr(&mut out, &e).map_err(|m| bad(format!("inode {}: {m}", inode.nid)))?;
        }
        while pos + 4 <= body.len() {
            let size = 4 + body[pos] as usize + le16(&body[pos + 2..]) as usize;
            if pos + size > body.len() {
                return Err(bad(format!(
                    "inode {}: an attribute past its end",
                    inode.nid
                )));
            }
            add_xattr(&mut out, &body[pos..pos + size])
                .map_err(|m| bad(format!("inode {}: {m}", inode.nid)))?;
            pos += align(size as u64, XATTR_ENTRY_SIZE as u64) as usize;
        }
        Ok(out)
    }

    /// The inode a directory names name, ".." included.
    fn child(&self, dir: &Inode, name: &[u8]) -> io::Result<Inode> {
        for e in self.dirents(dir)? {
            if e.name == name {
                return self.inode(e.nid);
            }
        }
        Err(not_found())
    }

    /// Finds what is at path, slash-separated from the root, following
    /// symlinks on the way, as the image's own root resolves them, and the
    /// last one too if follow.
    pub fn lookup(&self, path: &[u8], follow: bool) -> io::Result<Inode> {
        let root = self.root()?;
        let mut parts = split_path(path);
        parts.reverse();
        let mut cur = root.clone();
        let mut links = 0;
        while let Some(part) = parts.pop() {
            if cur.kind != Kind::Dir {
                return Err(not_found());
            }
            let next = self.child(&cur, &part)?;
            if part == b".." {
                cur = next;
                continue;
            }
            if next.kind == Kind::Symlink && (!parts.is_empty() || follow) {
                links += 1;
                if links > MAX_SYMLINKS {
                    return Err(bad("too many levels of symbolic links".into()));
                }
                let target = self.readlink(&next)?;
                if target.starts_with(b"/") {
                    cur = root.clone();
                }
                let mut more = split_path(&target);
                more.reverse();
                parts.extend(more);
                continue;
            }
            cur = next;
        }
        Ok(cur)
    }
}

fn split_path(p: &[u8]) -> Vec<Vec<u8>> {
    p.split(|b| *b == b'/')
        .filter(|s| !s.is_empty() && *s != b".")
        .map(<[u8]>::to_vec)
        .collect()
}

fn add_xattr(out: &mut BTreeMap<Vec<u8>, Vec<u8>>, e: &[u8]) -> Result<(), String> {
    let name_len = e[0] as usize;
    let Some(prefix) = xattr_prefix(e[1]) else {
        return Err(format!("an attribute with name index {}", e[1]));
    };
    let mut name = prefix.as_bytes().to_vec();
    name.extend_from_slice(&e[4..4 + name_len]);
    out.insert(name, e[4 + name_len..].to_vec());
    Ok(())
}
