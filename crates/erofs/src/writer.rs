//! Building an image: files' data as they arrive, the tree at the end.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{self, Read};

use crate::format::*;

/// One thing in an image's tree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entry {
    /// Where it is, relative to the root, which is ".". A name is bytes, as
    /// Linux keeps it.
    pub path: Vec<u8>,
    pub kind: Option<Kind>,
    /// Its permission bits, with setuid, setgid and sticky (0o7777).
    pub perm: u32,
    pub uid: u32,
    pub gid: u32,
    /// When it was last changed: seconds and nanoseconds since the epoch.
    pub mtime: i64,
    pub nsec: u32,
    /// A regular file's length: what adding it reads of its data.
    pub size: u64,
    /// Where a symlink points.
    pub target: Vec<u8>,
    /// A device's number.
    pub major: u32,
    pub minor: u32,
    /// Its extended attributes, by their full names. Those under a prefix
    /// EROFS has no index for (anything in system. but the ACLs) are left
    /// out.
    pub xattrs: BTreeMap<Vec<u8>, Vec<u8>>,
}

/// Where an image is written: anywhere bytes can be put at an offset.
pub trait Out {
    fn write_at(&mut self, buf: &[u8], offset: u64) -> io::Result<()>;

    /// Copies len bytes of src to offset, if this can do it faster than
    /// reading and writing them; false if it cannot.
    fn copy_file(&mut self, _src: &mut File, _len: u64, _offset: u64) -> io::Result<bool> {
        Ok(false)
    }
}

impl Out for File {
    fn write_at(&mut self, buf: &[u8], offset: u64) -> io::Result<()> {
        #[cfg(unix)]
        {
            std::os::unix::fs::FileExt::write_all_at(self, buf, offset)
        }
        #[cfg(not(unix))]
        {
            use std::io::{Seek, SeekFrom, Write};
            self.seek(SeekFrom::Start(offset))?;
            self.write_all(buf)
        }
    }

    /// File to file: the kernel copies it (copy_file_range on Linux).
    fn copy_file(&mut self, src: &mut File, len: u64, offset: u64) -> io::Result<bool> {
        use std::io::{Seek, SeekFrom};
        self.seek(SeekFrom::Start(offset))?;
        let copied = io::copy(&mut src.take(len), self)?;
        if copied < len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "reading its data: the file is shorter than it said",
            ));
        }
        Ok(true)
    }
}

impl Out for Vec<u8> {
    fn write_at(&mut self, buf: &[u8], offset: u64) -> io::Result<()> {
        let end = offset as usize + buf.len();
        if self.len() < end {
            self.resize(end, 0);
        }
        self[offset as usize..end].copy_from_slice(buf);
        Ok(())
    }
}

fn err(msg: String) -> io::Error {
    io::Error::other(msg)
}

fn shown(p: &[u8]) -> String {
    String::from_utf8_lossy(p).into_owned()
}

struct Node {
    mode: u16,
    uid: u32,
    gid: u32,
    mtime: i64,
    nsec: u32,
    size: u64,
    rdev: u32,
    xattrs: Vec<u8>,
    icount: u16,

    layout: u8,
    start: u32,
    /// The data kept with the inode: the part of a file or symlink after its
    /// last whole block, in Writer::tails.
    tail_off: usize,
    tail_len: usize,

    /// A directory's entries by name; None for anything else.
    children: Option<HashMap<Vec<u8>, usize>>,
    parent: usize,
    implicit: bool,
    links: u32,

    queued: bool,
    nid: u64,
    ino: u32,

    /// A directory's entries, sorted, and where each of its blocks starts in
    /// them.
    dirents: Vec<(Vec<u8>, usize)>,
    dir_cuts: Vec<usize>,
    dir_tail: u64,
}

impl Node {
    fn new(mode: u16) -> Node {
        Node {
            mode,
            uid: 0,
            gid: 0,
            mtime: 0,
            nsec: 0,
            size: 0,
            rdev: 0,
            xattrs: vec![],
            icount: 0,
            layout: LAYOUT_PLAIN,
            start: NULL_ADDR,
            tail_off: 0,
            tail_len: 0,
            children: None,
            parent: 0,
            implicit: false,
            links: 0,
            queued: false,
            nid: 0,
            ino: 0,
            dirents: vec![],
            dir_cuts: vec![],
            dir_tail: 0,
        }
    }

    /// A directory made for what is put in it, until its own entry comes.
    fn implicit_dir(parent: usize) -> Node {
        let mut n = Node::new(Kind::Dir.mode() | 0o755);
        n.children = Some(HashMap::new());
        n.implicit = true;
        n.parent = parent;
        n
    }

    fn kind(&self) -> Option<Kind> {
        Kind::of_mode(self.mode)
    }

    /// The length of the data kept with the inode.
    fn inline_len(&self) -> u64 {
        if self.children.is_some() {
            self.dir_tail
        } else {
            self.tail_len as u64
        }
    }
}

/// Builds an image. Entries come in any order: a file may come before its
/// directory, which is made for it, owned by root with mode 0755, until the
/// directory's own entry says otherwise.
pub struct Writer<O: Out> {
    out: O,
    nodes: Vec<Node>,
    /// The next free block: file data is written from block 1, after the
    /// superblock's, as it arrives.
    next: u64,
    buf: Vec<u8>,
    tails: Vec<u8>,
    size: u64,
    done: bool,
}

const ROOT: usize = 0;

/// A path as the tree keys it: its names, without "." and "..", none for
/// the root. A path cannot leave the root.
fn clean_path(p: &[u8]) -> io::Result<Vec<Vec<u8>>> {
    let mut parts: Vec<Vec<u8>> = vec![];
    for part in p.split(|b| *b == b'/') {
        match part {
            b"" | b"." => {}
            b".." => {
                parts.pop();
            }
            name => {
                if name.len() > MAX_NAME {
                    return Err(err(format!(
                        "{}: a name longer than {MAX_NAME} bytes",
                        shown(p)
                    )));
                }
                parts.push(name.to_vec());
            }
        }
    }
    Ok(parts)
}

impl<O: Out> Writer<O> {
    /// Starts an image in out, which it writes from offset 0: each file's
    /// data as it is added, and the rest at close.
    pub fn new(out: O) -> Writer<O> {
        let mut root = Node::implicit_dir(ROOT);
        root.parent = ROOT;
        Writer {
            out,
            nodes: vec![root],
            next: 1,
            buf: vec![0; 1 << 20],
            tails: vec![],
            size: 0,
            done: false,
        }
    }

    fn lookup(&self, parts: &[Vec<u8>]) -> Option<usize> {
        let mut n = ROOT;
        for part in parts {
            n = *self.nodes[n].children.as_ref()?.get(part)?;
        }
        Some(n)
    }

    /// The directory a path goes in, made if it is not there yet.
    fn parent_of(&mut self, path: &[u8], parts: &[Vec<u8>]) -> io::Result<usize> {
        let mut n = ROOT;
        for part in &parts[..parts.len().saturating_sub(1)] {
            let found = self.nodes[n]
                .children
                .as_ref()
                .and_then(|c| c.get(part))
                .copied();
            n = match found {
                Some(c) if self.nodes[c].children.is_none() => {
                    return Err(err(format!(
                        "{}: {} is not a directory",
                        shown(path),
                        shown(part)
                    )));
                }
                Some(c) => c,
                None => {
                    let c = self.nodes.len();
                    self.nodes.push(Node::implicit_dir(n));
                    self.nodes[n]
                        .children
                        .as_mut()
                        .unwrap()
                        .insert(part.clone(), c);
                    c
                }
            };
        }
        Ok(n)
    }

    /// Puts e in the image, reading a regular file's e.size bytes from data.
    /// A directory added where one was made for what it holds keeps what it
    /// holds and takes e's owner, mode and attributes; anything else may be
    /// added once.
    pub fn add(&mut self, e: &Entry, data: &mut dyn Read) -> io::Result<()> {
        self.add_from(e, Source::Read(data))
    }

    /// As add, a regular file's data from a file, which the kernel copies
    /// where it can.
    pub fn add_file(&mut self, e: &Entry, data: &mut File) -> io::Result<()> {
        self.add_from(e, Source::File(data))
    }

    fn add_from(&mut self, e: &Entry, data: Source<'_>) -> io::Result<()> {
        if self.done {
            return Err(err("erofs: the image is closed".into()));
        }
        let parts = clean_path(&e.path)?;
        let Some(kind) = e.kind else {
            return Err(err(format!("{}: no type", shown(&e.path))));
        };
        let (xattrs, icount) =
            encode_xattrs(&e.xattrs).map_err(|m| err(format!("{}: {m}", shown(&e.path))))?;
        let mut n = Node::new(kind.mode() | (e.perm & 0o7777) as u16);
        n.uid = e.uid;
        n.gid = e.gid;
        n.xattrs = xattrs;
        n.icount = icount;
        n.mtime = e.mtime;
        n.nsec = e.nsec;

        let existing = self.lookup(&parts);
        if kind == Kind::Dir {
            if let Some(x) = existing {
                let x = &mut self.nodes[x];
                if x.children.is_none() || !x.implicit {
                    return Err(err(format!("{}: added twice", shown(&e.path))));
                }
                x.mode = n.mode;
                x.uid = n.uid;
                x.gid = n.gid;
                x.mtime = n.mtime;
                x.nsec = n.nsec;
                x.xattrs = n.xattrs;
                x.icount = n.icount;
                x.implicit = false;
                return Ok(());
            }
            n.children = Some(HashMap::new());
        } else if existing.is_some() {
            return Err(err(format!("{}: added twice", shown(&e.path))));
        }
        let Some(name) = parts.last() else {
            return Err(err(format!("the root is a directory, not a {kind}")));
        };
        let parent = self.parent_of(&e.path, &parts)?;

        match kind {
            Kind::Regular => self
                .write_data(&mut n, data, e.size)
                .map_err(|x| err(format!("{}: {x}", shown(&e.path))))?,
            Kind::Symlink => {
                if e.target.is_empty() {
                    return Err(err(format!("{}: a symlink to nothing", shown(&e.path))));
                }
                let target = e.target.clone();
                self.write_data(
                    &mut n,
                    Source::Read(&mut target.as_slice()),
                    target.len() as u64,
                )
                .map_err(|x| err(format!("{}: {x}", shown(&e.path))))?;
            }
            Kind::CharDevice | Kind::BlockDevice => n.rdev = encode_dev(e.major, e.minor),
            _ => {}
        }
        n.parent = parent;
        n.links = 1;
        let id = self.nodes.len();
        let name = name.clone();
        self.nodes.push(n);
        self.nodes[parent]
            .children
            .as_mut()
            .unwrap()
            .insert(name, id);
        Ok(())
    }

    /// Adds a hard link at path to what is at target, which must be there
    /// already and not be a directory.
    pub fn link(&mut self, path: &[u8], target: &[u8]) -> io::Result<()> {
        if self.done {
            return Err(err("erofs: the image is closed".into()));
        }
        let parts = clean_path(path)?;
        let tparts = clean_path(target)?;
        let n = match self.lookup(&tparts) {
            None => {
                return Err(err(format!(
                    "{}: a link to {}, which is not there",
                    shown(path),
                    shown(target)
                )));
            }
            Some(n) if self.nodes[n].children.is_some() => {
                return Err(err(format!(
                    "{}: a link to a directory, {}",
                    shown(path),
                    shown(target)
                )));
            }
            Some(n) => n,
        };
        if self.lookup(&parts).is_some() {
            return Err(err(format!("{}: added twice", shown(path))));
        }
        let Some(name) = parts.last().cloned() else {
            return Err(err(format!("{}: added twice", shown(path))));
        };
        let parent = self.parent_of(path, &parts)?;
        self.nodes[parent]
            .children
            .as_mut()
            .unwrap()
            .insert(name, n);
        self.nodes[n].links += 1;
        Ok(())
    }

    /// Writes a file's whole blocks where the data so far ends. The rest is
    /// kept, to go with its inode, if the two fit in one block; otherwise it
    /// gets a block of its own after the others.
    fn write_data(&mut self, n: &mut Node, data: Source<'_>, size: u64) -> io::Result<()> {
        n.size = size;
        n.layout = LAYOUT_PLAIN;
        let whole = size / BLOCK_SIZE;
        let rest = size % BLOCK_SIZE;
        let inline = rest > 0 && EXTENDED_SIZE as u64 + n.xattrs.len() as u64 + rest <= BLOCK_SIZE;
        if whole > 0 || (rest > 0 && !inline) {
            n.start = self.next as u32;
        }
        let mut left = whole * BLOCK_SIZE;
        let r: &mut dyn Read = match data {
            Source::File(f) => {
                if left > 0 && self.out.copy_file(f, left, self.next * BLOCK_SIZE)? {
                    self.next += whole;
                    left = 0;
                }
                f
            }
            Source::Read(r) => r,
        };
        while left > 0 {
            let k = (self.buf.len() as u64).min(left) as usize;
            r.read_exact(&mut self.buf[..k])
                .map_err(|e| err(format!("reading its data: {e}")))?;
            self.out.write_at(&self.buf[..k], self.next * BLOCK_SIZE)?;
            self.next += k as u64 / BLOCK_SIZE;
            left -= k as u64;
        }
        if rest == 0 {
            return self.check_blocks();
        }
        if inline {
            n.layout = LAYOUT_INLINE;
            n.tail_off = self.tails.len();
            self.tails.resize(n.tail_off + rest as usize, 0);
            r.read_exact(&mut self.tails[n.tail_off..])
                .map_err(|e| err(format!("reading its data: {e}")))?;
            n.tail_len = rest as usize;
            return Ok(());
        }
        let block = &mut self.buf[..BLOCK_SIZE as usize];
        block.fill(0);
        r.read_exact(&mut block[..rest as usize])
            .map_err(|e| err(format!("reading its data: {e}")))?;
        self.out
            .write_at(&self.buf[..BLOCK_SIZE as usize], self.next * BLOCK_SIZE)?;
        self.next += 1;
        self.check_blocks()
    }

    fn check_blocks(&self) -> io::Result<()> {
        if self.next >= NULL_ADDR as u64 {
            return Err(err(
                "the image is larger than EROFS's 32-bit block numbers reach".into(),
            ));
        }
        Ok(())
    }

    /// The image's length, once it is closed: a whole number of blocks.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// What the image was written to.
    pub fn into_inner(self) -> O {
        self.out
    }

    /// Lays out and writes what describes the tree -- inodes, directories,
    /// and the data kept with them -- after the files' data, and the
    /// superblock before it.
    pub fn close(&mut self) -> io::Result<()> {
        if self.done {
            return Ok(());
        }
        self.done = true;

        // Inodes in breadth-first order, so a directory's are near each
        // other.
        let mut order = vec![ROOT];
        self.nodes[ROOT].queued = true;
        let mut i = 0;
        while i < order.len() {
            let n = order[i];
            i += 1;
            if self.nodes[n].children.is_none() {
                continue;
            }
            self.layout_dir(n);
            let ents: Vec<usize> = self.nodes[n].dirents.iter().map(|(_, c)| *c).collect();
            for c in ents {
                if !self.nodes[c].queued {
                    self.nodes[c].queued = true;
                    order.push(c);
                }
            }
        }

        // Inodes go in the metadata area, each with its attributes and any
        // data kept with it in one block, which the kernel requires of the
        // data.
        let meta_start = self.next;
        let mut alloc = MetaAlloc::default();
        for (i, &n) in order.iter().enumerate() {
            let node = &self.nodes[n];
            let mut rec = EXTENDED_SIZE as u64 + node.xattrs.len() as u64;
            if node.layout == LAYOUT_INLINE {
                rec += node.inline_len();
            }
            let off = alloc.alloc(align(rec, SLOT_SIZE));
            let node = &mut self.nodes[n];
            node.nid = off >> SLOT_BITS;
            node.ino = (i + 1) as u32;
        }
        if self.nodes[ROOT].nid > 0xFFFF {
            return Err(err(
                "the root's inode is beyond where the superblock can point".into(),
            ));
        }

        // Directories' blocks follow the metadata area.
        let mut next = meta_start + alloc.blocks;
        for &n in &order {
            let node = &mut self.nodes[n];
            if node.children.is_none() {
                continue;
            }
            let mut blocks = node.size / BLOCK_SIZE;
            if node.layout == LAYOUT_PLAIN && !node.size.is_multiple_of(BLOCK_SIZE) {
                blocks += 1;
            }
            if blocks > 0 {
                node.start = next as u32;
                next += blocks;
            }
        }
        if next >= NULL_ADDR as u64 {
            return Err(err(
                "the image is larger than EROFS's 32-bit block numbers reach".into(),
            ));
        }

        let mut meta = vec![0u8; (alloc.blocks * BLOCK_SIZE) as usize];
        for &n in &order {
            let node = &self.nodes[n];
            let mut off = (node.nid << SLOT_BITS) as usize;
            self.encode_inode(n, &mut meta[off..]);
            off += EXTENDED_SIZE;
            meta[off..off + node.xattrs.len()].copy_from_slice(&node.xattrs);
            off += node.xattrs.len();
            if node.children.is_none() {
                if node.layout == LAYOUT_INLINE {
                    meta[off..off + node.tail_len]
                        .copy_from_slice(&self.tails[node.tail_off..node.tail_off + node.tail_len]);
                }
                continue;
            }
            let mut data = self.render_dir(n);
            let whole = (node.size / BLOCK_SIZE * BLOCK_SIZE) as usize;
            if node.layout == LAYOUT_INLINE {
                let tail = &data[whole..];
                meta[off..off + tail.len()].copy_from_slice(tail);
                data.truncate(whole);
            }
            if !data.is_empty() {
                self.out.write_at(&data, node.start as u64 * BLOCK_SIZE)?;
            }
        }
        self.out.write_at(&meta, meta_start * BLOCK_SIZE)?;

        let mut sb = [0u8; SUPER_SIZE];
        sb[0..4].copy_from_slice(&SUPER_MAGIC.to_le_bytes());
        sb[12] = BLOCK_BITS;
        sb[14..16].copy_from_slice(&(self.nodes[ROOT].nid as u16).to_le_bytes());
        sb[16..24].copy_from_slice(&(order.len() as u64).to_le_bytes());
        sb[36..40].copy_from_slice(&(next as u32).to_le_bytes());
        sb[40..44].copy_from_slice(&(meta_start as u32).to_le_bytes());
        self.out.write_at(&sb, SUPER_OFFSET)?;
        self.size = next * BLOCK_SIZE;
        Ok(())
    }

    /// Sorts a directory's entries, "." and ".." among them, as the kernel's
    /// binary search expects, and packs them into blocks: each block its
    /// entries' 12-byte records, then their names.
    fn layout_dir(&mut self, n: usize) {
        let node = &self.nodes[n];
        let mut dirents: Vec<(Vec<u8>, usize)> =
            vec![(b".".to_vec(), n), (b"..".to_vec(), node.parent)];
        for (name, &c) in node.children.as_ref().unwrap() {
            dirents.push((name.clone(), c));
        }
        dirents.sort_by(|a, b| a.0.cmp(&b.0));
        let mut cuts = vec![0];
        let mut used = 0u64;
        for (i, (name, _)) in dirents.iter().enumerate() {
            let need = (DIRENT_SIZE + name.len()) as u64;
            if used + need > BLOCK_SIZE {
                cuts.push(i);
                used = 0;
            }
            used += need;
        }
        let node = &mut self.nodes[n];
        node.size = (cuts.len() as u64 - 1) * BLOCK_SIZE + used;
        node.dir_tail = node.size % BLOCK_SIZE;
        node.start = NULL_ADDR;
        node.layout = LAYOUT_PLAIN;
        if node.dir_tail > 0
            && EXTENDED_SIZE as u64 + node.xattrs.len() as u64 + node.dir_tail <= BLOCK_SIZE
        {
            node.layout = LAYOUT_INLINE;
        }
        node.dirents = dirents;
        node.dir_cuts = cuts;
    }

    /// A directory's contents, every block but the last whole.
    fn render_dir(&self, n: usize) -> Vec<u8> {
        let node = &self.nodes[n];
        let mut out = vec![0u8; align(node.size, BLOCK_SIZE) as usize];
        let mut cuts = node.dir_cuts.clone();
        cuts.push(node.dirents.len());
        for b in 0..cuts.len() - 1 {
            let block = &mut out[b * BLOCK_SIZE as usize..];
            let ents = &node.dirents[cuts[b]..cuts[b + 1]];
            let mut name_off = ents.len() * DIRENT_SIZE;
            for (i, (name, c)) in ents.iter().enumerate() {
                let child = &self.nodes[*c];
                let r = &mut block[i * DIRENT_SIZE..];
                r[0..8].copy_from_slice(&child.nid.to_le_bytes());
                r[8..10].copy_from_slice(&(name_off as u16).to_le_bytes());
                r[10] = child.kind().map(Kind::dirent).unwrap_or(0);
                block[name_off..name_off + name.len()].copy_from_slice(name);
                name_off += name.len();
            }
        }
        out.truncate(node.size as usize);
        out
    }

    /// How many directory entries name it: a directory's own, its parent's,
    /// and each of its subdirectories' "..".
    fn nlink(&self, n: usize) -> u32 {
        let node = &self.nodes[n];
        match &node.children {
            None => node.links,
            Some(children) => {
                2 + children
                    .values()
                    .filter(|c| self.nodes[**c].children.is_some())
                    .count() as u32
            }
        }
    }

    fn encode_inode(&self, n: usize, b: &mut [u8]) {
        let node = &self.nodes[n];
        b[0..2].copy_from_slice(&(VERSION_EXTENDED | (node.layout as u16) << 1).to_le_bytes());
        b[2..4].copy_from_slice(&node.icount.to_le_bytes());
        b[4..6].copy_from_slice(&node.mode.to_le_bytes());
        b[8..16].copy_from_slice(&node.size.to_le_bytes());
        match node.kind() {
            Some(Kind::CharDevice | Kind::BlockDevice) => {
                b[16..20].copy_from_slice(&node.rdev.to_le_bytes())
            }
            Some(Kind::Regular | Kind::Dir | Kind::Symlink) => {
                b[16..20].copy_from_slice(&node.start.to_le_bytes())
            }
            _ => {}
        }
        b[20..24].copy_from_slice(&node.ino.to_le_bytes());
        b[24..28].copy_from_slice(&node.uid.to_le_bytes());
        b[28..32].copy_from_slice(&node.gid.to_le_bytes());
        b[32..40].copy_from_slice(&(node.mtime as u64).to_le_bytes());
        b[40..44].copy_from_slice(&node.nsec.to_le_bytes());
        b[44..48].copy_from_slice(&self.nlink(n).to_le_bytes());
    }
}

enum Source<'a> {
    Read(&'a mut dyn Read),
    File(&'a mut File),
}

/// An inode's inline attribute body, and its i_xattr_icount; none for no
/// attributes.
fn encode_xattrs(attrs: &BTreeMap<Vec<u8>, Vec<u8>>) -> Result<(Vec<u8>, u16), String> {
    if attrs.is_empty() {
        return Ok((vec![], 0));
    }
    let mut body = vec![0u8; XATTR_HEADER_SIZE];
    for (name, value) in attrs {
        let Some((index, suffix)) = split_xattr(name) else {
            continue;
        };
        if suffix.len() > MAX_NAME {
            return Err(format!("the attribute name {:?} is too long", shown(name)));
        }
        if value.len() > MAX_XATTR_VALUE {
            return Err(format!(
                "the attribute {} is larger than {MAX_XATTR_VALUE} bytes",
                shown(name)
            ));
        }
        let mut e = vec![
            0u8;
            align(
                (4 + suffix.len() + value.len()) as u64,
                XATTR_ENTRY_SIZE as u64
            ) as usize
        ];
        e[0] = suffix.len() as u8;
        e[1] = index;
        e[2..4].copy_from_slice(&(value.len() as u16).to_le_bytes());
        e[4..4 + suffix.len()].copy_from_slice(suffix);
        e[4 + suffix.len()..4 + suffix.len() + value.len()].copy_from_slice(value);
        body.extend_from_slice(&e);
    }
    if body.len() == XATTR_HEADER_SIZE {
        return Ok((vec![], 0));
    }
    let icount = (body.len() - XATTR_HEADER_SIZE) / XATTR_ENTRY_SIZE + 1;
    if icount > 0xFFFF {
        return Err("too many extended attributes".into());
    }
    Ok((body, icount as u16))
}

fn split_xattr(name: &[u8]) -> Option<(u8, &[u8])> {
    for (index, prefix) in XATTR_PREFIXES {
        if index == 2 || index == 3 {
            if name == prefix.as_bytes() {
                return Some((index, &[]));
            }
            continue;
        }
        if let Some(suffix) = name.strip_prefix(prefix.as_bytes())
            && !suffix.is_empty()
        {
            return Some((index, suffix));
        }
    }
    None
}

/// Places records in the metadata area, each within one block where it fits
/// in one: in the fullest block with room for it, so few slots go spare.
struct MetaAlloc {
    blocks: u64,
    /// Blocks by how many 32-byte slots they have left.
    free: Vec<Vec<u64>>,
}

impl Default for MetaAlloc {
    fn default() -> Self {
        MetaAlloc {
            blocks: 0,
            free: vec![vec![]; (BLOCK_SIZE / SLOT_SIZE) as usize + 1],
        }
    }
}

impl MetaAlloc {
    fn alloc(&mut self, size: u64) -> u64 {
        let slots = BLOCK_SIZE / SLOT_SIZE;
        if size > BLOCK_SIZE {
            let start = self.blocks;
            self.blocks += align(size, BLOCK_SIZE) / BLOCK_SIZE;
            return start * BLOCK_SIZE;
        }
        let need = size / SLOT_SIZE;
        for s in need..=slots {
            if let Some(b) = self.free[s as usize].pop() {
                let off = b * BLOCK_SIZE + (slots - s) * SLOT_SIZE;
                if s > need {
                    self.free[(s - need) as usize].push(b);
                }
                return off;
            }
        }
        let b = self.blocks;
        self.blocks += 1;
        if need < slots {
            self.free[(slots - need) as usize].push(b);
        }
        b * BLOCK_SIZE
    }
}
