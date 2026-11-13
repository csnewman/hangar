//! Reads an ext4 filesystem image without mounting it: its tree, each
//! file's type, owner, mode and contents, and extended attributes. It reads
//! what is on the disk, as a read-only mount with noload does, without
//! replaying a journal a machine that did not shut down cleanly left
//! behind. Files are mapped by extents, as mkfs.ext4 makes them, or kept
//! inline; the older block maps are not read.

use std::collections::BTreeMap;
use std::io;

const SUPER_OFFSET: u64 = 1024;
const SUPER_MAGIC: u16 = 0xEF53;
const ROOT_INO: u32 = 2;

const INCOMPAT_FILETYPE: u32 = 0x0002;
const INCOMPAT_JOURNAL_DEV: u32 = 0x0008;
const INCOMPAT_META_BG: u32 = 0x0010;
const INCOMPAT_64BIT: u32 = 0x0080;
const INCOMPAT_ENCRYPT: u32 = 0x10000;
const INCOMPAT_CASEFOLD: u32 = 0x20000;
const INCOMPAT_DIRDATA: u32 = 0x1000;

const FLAG_EXTENTS: u32 = 0x80000;
const FLAG_INLINE_DATA: u32 = 0x1000_0000;

const EXTENT_MAGIC: u16 = 0xF30A;
const XATTR_MAGIC: u32 = 0xEA02_0000;
const INLINE_SIZE: usize = 60;

/// How many symlinks resolving one path may pass through.
const MAX_SYMLINKS: usize = 40;

/// Where a filesystem is read from: anything bytes can be read at an
/// offset.
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

impl<T: ReadAt + ?Sized> ReadAt for &T {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        (**self).read_at(buf, offset)
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

fn bad(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

fn le16(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// What a file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Regular,
    Dir,
    Symlink,
    CharDevice,
    BlockDevice,
    Fifo,
    Socket,
}

impl Kind {
    fn of_mode(mode: u16) -> Option<Kind> {
        Some(match mode & 0o170000 {
            0o100000 => Kind::Regular,
            0o040000 => Kind::Dir,
            0o120000 => Kind::Symlink,
            0o020000 => Kind::CharDevice,
            0o060000 => Kind::BlockDevice,
            0o010000 => Kind::Fifo,
            0o140000 => Kind::Socket,
            _ => return None,
        })
    }
}

/// An ext4 filesystem to read.
pub struct Fs<R: ReadAt> {
    r: R,
    block_size: u64,
    ipg: u32,
    inode_size: u64,
    desc_size: u64,
    gdt: u64,
    filetype: bool,
}

/// One file of the filesystem.
#[derive(Debug, Clone)]
pub struct Inode {
    pub ino: u32,
    pub kind: Kind,
    /// Its permission bits, with setuid, setgid and sticky.
    pub perm: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub nlink: u16,
    /// When it was last changed, in seconds since the epoch.
    pub mtime: i64,
    /// A device's number.
    pub major: u32,
    pub minor: u32,

    flags: u32,
    block: [u8; INLINE_SIZE],
    acl: u64,
    raw: Vec<u8>,
    extra: usize,
}

#[derive(Debug, Clone, Copy)]
struct Extent {
    logical: u64,
    physical: u64,
    length: u64,
    unwritten: bool,
}

/// One entry of a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: Vec<u8>,
    pub ino: u32,
}

impl<R: ReadAt> Fs<R> {
    /// Reads a filesystem's superblock.
    pub fn open(r: R) -> io::Result<Fs<R>> {
        let mut sb = [0u8; 1024];
        if r.read_at(&mut sb, SUPER_OFFSET)? < sb.len() {
            return Err(bad("reading the superblock: the image is too short".into()));
        }
        if le16(&sb[56..]) != SUPER_MAGIC {
            return Err(bad("not an ext4 filesystem".into()));
        }
        let log_block = le32(&sb[24..]);
        if log_block > 6 {
            return Err(bad(format!("a block size of 2^{}", 10 + log_block)));
        }
        let incompat = le32(&sb[96..]);
        for (bit, what) in [
            (INCOMPAT_JOURNAL_DEV, "an external journal"),
            (INCOMPAT_META_BG, "meta_bg"),
            (INCOMPAT_ENCRYPT, "encryption"),
            (INCOMPAT_CASEFOLD, "casefolding"),
            (INCOMPAT_DIRDATA, "dirdata"),
        ] {
            if incompat & bit != 0 {
                return Err(bad(format!(
                    "a filesystem with {what}, which this reader does not follow"
                )));
            }
        }
        let block_size = 1024u64 << log_block;
        let mut inode_size = 128;
        if le32(&sb[76..]) >= 1 {
            inode_size = le16(&sb[88..]) as u64;
        }
        let mut desc_size = 32;
        if incompat & INCOMPAT_64BIT != 0 {
            let d = le16(&sb[254..]) as u64;
            if d >= 64 {
                desc_size = d;
            }
        }
        let first_data = le32(&sb[20..]) as u64;
        let ipg = le32(&sb[40..]);
        if ipg == 0 || inode_size < 128 {
            return Err(bad("a bad superblock".into()));
        }
        Ok(Fs {
            r,
            block_size,
            ipg,
            inode_size,
            desc_size,
            gdt: (first_data + 1) * block_size,
            filetype: incompat & INCOMPAT_FILETYPE != 0,
        })
    }

    fn read_exact_at(&self, buf: &mut [u8], offset: u64, what: &str) -> io::Result<()> {
        if self.r.read_at(buf, offset)? < buf.len() {
            return Err(bad(format!("reading {what}: the image ends")));
        }
        Ok(())
    }

    /// The root directory.
    pub fn root(&self) -> io::Result<Inode> {
        self.inode(ROOT_INO)
    }

    /// Reads inode number ino.
    pub fn inode(&self, ino: u32) -> io::Result<Inode> {
        if ino == 0 {
            return Err(bad("inode 0".into()));
        }
        let group = ((ino - 1) / self.ipg) as u64;
        let index = ((ino - 1) % self.ipg) as u64;
        let mut desc = vec![0u8; self.desc_size as usize];
        self.read_exact_at(
            &mut desc,
            self.gdt + group * self.desc_size,
            &format!("group {group}'s descriptor"),
        )?;
        let mut table = le32(&desc[8..]) as u64;
        if self.desc_size >= 64 {
            table |= (le32(&desc[0x28..]) as u64) << 32;
        }
        let mut raw = vec![0u8; self.inode_size as usize];
        self.read_exact_at(
            &mut raw,
            table * self.block_size + index * self.inode_size,
            &format!("inode {ino}"),
        )?;
        let mode = le16(&raw[0..]);
        let Some(kind) = Kind::of_mode(mode) else {
            return Err(bad(format!("inode {ino} is not in use")));
        };
        let mut block = [0u8; INLINE_SIZE];
        block.copy_from_slice(&raw[40..40 + INLINE_SIZE]);
        let mut inode = Inode {
            ino,
            kind,
            perm: (mode & 0o7777) as u32,
            uid: le16(&raw[2..]) as u32 | (le16(&raw[120..]) as u32) << 16,
            gid: le16(&raw[24..]) as u32 | (le16(&raw[122..]) as u32) << 16,
            size: le32(&raw[4..]) as u64 | (le32(&raw[108..]) as u64) << 32,
            nlink: le16(&raw[26..]),
            mtime: le32(&raw[16..]) as i32 as i64,
            major: 0,
            minor: 0,
            flags: le32(&raw[32..]),
            block,
            acl: le32(&raw[104..]) as u64 | (le16(&raw[118..]) as u64) << 32,
            extra: if self.inode_size > 128 {
                le16(&raw[128..]) as usize
            } else {
                0
            },
            raw,
        };
        if matches!(kind, Kind::CharDevice | Kind::BlockDevice) {
            // The old encoding in the first word, else the new in the
            // second.
            let old = le32(&inode.block[0..]);
            if old != 0 {
                (inode.major, inode.minor) = ((old >> 8) & 0xff, old & 0xff);
            } else {
                let dev = le32(&inode.block[4..]);
                inode.major = (dev & 0xfff00) >> 8;
                inode.minor = (dev & 0xff) | ((dev >> 12) & 0xfff00);
            }
        }
        Ok(inode)
    }

    /// Whether the inode keeps its data in itself: a fast symlink, or
    /// inline data.
    fn inline(inode: &Inode) -> bool {
        if inode.flags & FLAG_INLINE_DATA != 0 {
            return true;
        }
        inode.kind == Kind::Symlink
            && inode.flags & FLAG_EXTENTS == 0
            && inode.size < INLINE_SIZE as u64
    }

    /// Reads an inode's contents: a file's data, a symlink's target, a
    /// directory's blocks. A hole, or an extent not yet written, reads as
    /// zeros.
    pub fn read_all(&self, inode: &Inode) -> io::Result<Vec<u8>> {
        let size = inode.size as usize;
        if Self::inline(inode) {
            let mut data = inode.block[..size.min(INLINE_SIZE)].to_vec();
            if inode.flags & FLAG_INLINE_DATA != 0 && size > INLINE_SIZE {
                let attrs = self.xattrs(inode)?;
                if let Some(more) = attrs.get(&b"system.data"[..]) {
                    data.extend_from_slice(more);
                }
            }
            if data.len() < size {
                return Err(bad(format!(
                    "inode {}: inline data short of its size",
                    inode.ino
                )));
            }
            data.truncate(size);
            return Ok(data);
        }
        if inode.flags & FLAG_EXTENTS == 0 {
            if size == 0 {
                return Ok(vec![]);
            }
            return Err(bad(format!(
                "inode {} maps its blocks without extents, which this reader does not follow",
                inode.ino
            )));
        }
        let extents = self
            .extents(&inode.block, 0)
            .map_err(|e| bad(format!("inode {}: {e}", inode.ino)))?;
        let mut out = vec![0u8; size];
        let bs = self.block_size;
        for x in extents.iter().filter(|x| !x.unwritten) {
            let start = x.logical * bs;
            if start >= inode.size {
                continue;
            }
            let end = ((x.logical + x.length) * bs).min(inode.size);
            self.read_exact_at(
                &mut out[start as usize..end as usize],
                x.physical * bs,
                &format!("inode {}'s data", inode.ino),
            )?;
        }
        Ok(out)
    }

    /// Walks an extent tree from node, a node's bytes.
    fn extents(&self, node: &[u8], depth: usize) -> io::Result<Vec<Extent>> {
        if depth > 5 {
            return Err(bad("an extent tree too deep".into()));
        }
        if node.len() < 12 || le16(&node[0..]) != EXTENT_MAGIC {
            return Err(bad("a bad extent header".into()));
        }
        let entries = le16(&node[2..]) as usize;
        let levels = le16(&node[6..]);
        if 12 + 12 * entries > node.len() {
            return Err(bad("an extent node past its end".into()));
        }
        let mut out = vec![];
        for i in 0..entries {
            let e = &node[12 + 12 * i..];
            if levels == 0 {
                let mut length = le16(&e[4..]) as u64;
                let unwritten = length > 32768;
                if unwritten {
                    length -= 32768;
                }
                out.push(Extent {
                    logical: le32(&e[0..]) as u64,
                    physical: (le16(&e[6..]) as u64) << 32 | le32(&e[8..]) as u64,
                    length,
                    unwritten,
                });
                continue;
            }
            let leaf = (le16(&e[8..]) as u64) << 32 | le32(&e[4..]) as u64;
            let mut child = vec![0u8; self.block_size as usize];
            self.read_exact_at(&mut child, leaf * self.block_size, "an extent node")?;
            out.extend(self.extents(&child, depth + 1)?);
        }
        Ok(out)
    }

    /// A symlink's target.
    pub fn readlink(&self, inode: &Inode) -> io::Result<Vec<u8>> {
        if inode.kind != Kind::Symlink {
            return Err(bad(format!("inode {} is not a symlink", inode.ino)));
        }
        self.read_all(inode)
    }

    /// A directory's entries, without "." and "..", in the order they are
    /// stored.
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
            return Err(bad(format!("inode {} is not a directory", inode.ino)));
        }
        let data = self.read_all(inode)?;
        let at = |e: io::Error| bad(format!("directory {}: {e}", inode.ino));
        let mut out = vec![];
        if inode.flags & FLAG_INLINE_DATA != 0 {
            // An inline directory starts with its parent's inode, and keeps
            // no "." or "..".
            if data.len() < 4 {
                return Err(bad(format!("directory {}: short inline data", inode.ino)));
            }
            out.push(DirEntry {
                name: b".".to_vec(),
                ino: inode.ino,
            });
            out.push(DirEntry {
                name: b"..".to_vec(),
                ino: le32(&data[0..]),
            });
            out.extend(
                self.parse_dirents(&data[4..INLINE_SIZE.min(data.len())])
                    .map_err(at)?,
            );
            if data.len() > INLINE_SIZE {
                out.extend(self.parse_dirents(&data[INLINE_SIZE..]).map_err(at)?);
            }
            return Ok(out);
        }
        let bs = self.block_size as usize;
        for chunk in data.chunks(bs) {
            out.extend(self.parse_dirents(chunk).map_err(at)?);
        }
        Ok(out)
    }

    /// Reads one block of linear directory entries. A hashed directory's
    /// index blocks read as one empty entry each, and a checksum's tail as
    /// another, both with inode 0, so they are skipped like a deleted
    /// entry.
    fn parse_dirents(&self, b: &[u8]) -> io::Result<Vec<DirEntry>> {
        let mut out = vec![];
        let mut off = 0;
        while off + 8 <= b.len() {
            let ino = le32(&b[off..]);
            let rec_len = le16(&b[off + 4..]) as usize;
            let mut name_len = b[off + 6] as usize;
            if !self.filetype {
                name_len |= (b[off + 7] as usize) << 8;
            }
            if rec_len < 8 || off + rec_len > b.len() || 8 + name_len > rec_len {
                return Err(bad("a bad directory entry".into()));
            }
            if ino != 0 && name_len > 0 {
                out.push(DirEntry {
                    name: b[off + 8..off + 8 + name_len].to_vec(),
                    ino,
                });
            }
            off += rec_len;
        }
        Ok(out)
    }

    /// An inode's extended attributes, by their full names: those in the
    /// inode and those in its attribute block.
    pub fn xattrs(&self, inode: &Inode) -> io::Result<BTreeMap<Vec<u8>, Vec<u8>>> {
        let mut out = BTreeMap::new();
        let start = 128 + inode.extra;
        if start + 4 <= inode.raw.len() && le32(&inode.raw[start..]) == XATTR_MAGIC {
            let body = &inode.raw[start + 4..];
            parse_xattrs(body, body, &mut out)
                .map_err(|e| bad(format!("inode {}: {e}", inode.ino)))?;
        }
        if inode.acl != 0 {
            let mut blk = vec![0u8; self.block_size as usize];
            self.read_exact_at(
                &mut blk,
                inode.acl * self.block_size,
                &format!("inode {}'s attribute block", inode.ino),
            )?;
            if le32(&blk[0..]) != XATTR_MAGIC {
                return Err(bad(format!("inode {}: a bad attribute block", inode.ino)));
            }
            parse_xattrs(&blk[32..], &blk, &mut out)
                .map_err(|e| bad(format!("inode {}: {e}", inode.ino)))?;
        }
        Ok(out)
    }

    /// Finds what is at path, slash-separated from the root, following
    /// symlinks on the way as the filesystem's own root resolves them, and
    /// the last one too if follow. A path that is not there, or passes
    /// through something that is not a directory, is NotFound.
    pub fn lookup(&self, path: &[u8], follow: bool) -> io::Result<Inode> {
        let root = self.root()?;
        let mut parts = split_path(path);
        parts.reverse();
        let mut cur = root.clone();
        let mut links = 0;
        while let Some(part) = parts.pop() {
            if cur.kind != Kind::Dir {
                return Err(io::ErrorKind::NotFound.into());
            }
            let ino = self
                .dirents(&cur)?
                .into_iter()
                .find(|e| e.name == part)
                .map(|e| e.ino)
                .ok_or(io::ErrorKind::NotFound)?;
            let next = self.inode(ino)?;
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

/// The prefix an extended attribute's name is stored by, by its index.
fn xattr_prefix(index: u8) -> &'static str {
    match index {
        1 => "user.",
        2 => "system.posix_acl_access",
        3 => "system.posix_acl_default",
        4 => "trusted.",
        6 => "security.",
        7 => "system.",
        8 => "system.richacl",
        _ => "",
    }
}

/// Reads entries from entries, whose values are at offsets in values, until
/// the four zero bytes that end them.
fn parse_xattrs(
    entries: &[u8],
    values: &[u8],
    out: &mut BTreeMap<Vec<u8>, Vec<u8>>,
) -> Result<(), String> {
    let mut off = 0;
    while off + 16 <= entries.len() {
        if le32(&entries[off..]) == 0 {
            return Ok(());
        }
        let name_len = entries[off] as usize;
        let index = entries[off + 1];
        let val_off = le16(&entries[off + 2..]) as usize;
        let val_inum = le32(&entries[off + 4..]);
        let val_size = le32(&entries[off + 8..]) as usize;
        if off + 16 + name_len > entries.len() {
            return Err("an attribute past its end".into());
        }
        let mut name = xattr_prefix(index).as_bytes().to_vec();
        name.extend_from_slice(&entries[off + 16..off + 16 + name_len]);
        if val_inum == 0 {
            if val_off + val_size > values.len() {
                return Err("an attribute's value past its end".into());
            }
            out.insert(name, values[val_off..val_off + val_size].to_vec());
        }
        off += (16 + name_len + 3) & !3;
    }
    Ok(())
}
