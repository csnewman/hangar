//! An image's layers merged into one EROFS image.
//!
//! An image's layers are tar archives, each recording what it changes over
//! the layers beneath it: files it adds or replaces, and with OCI whiteouts
//! (.wh.<name>, and .wh..wh..opq for a directory) what it removes. They are
//! merged here into the one tree they describe, as a single stream, top
//! layer first: a path's first entry is the one that stands, and a
//! whiteout, an opaque directory, or a non-directory replacing a directory
//! hides what the layers beneath had there. Nothing is unpacked: each entry
//! goes straight into the image, which accepts a file before its directory.

use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use hangar_erofs::{Entry, Kind, Out, Writer};

/// What an EROFS image's size is rounded up to: a persistent-memory region
/// is aligned to 2 MiB.
pub const EROFS_ALIGN: u64 = 2 << 20;

/// What the layers above have said, for the ones beneath.
#[derive(Default)]
pub struct Merge {
    seen: HashSet<Vec<u8>>,
    nondir: HashSet<Vec<u8>>,
    whiteouts: HashSet<Vec<u8>>,
    opaque: HashSet<Vec<u8>>,
}

/// A tar entry's path as the merge keys it: relative, without "./" or a
/// trailing slash, ".." worked out. The root is ".".
fn clean(name: &[u8]) -> Vec<u8> {
    let mut parts: Vec<&[u8]> = vec![];
    for part in name.split(|b| *b == b'/') {
        match part {
            b"" | b"." => {}
            b".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    if parts.is_empty() {
        b".".to_vec()
    } else {
        parts.join(&b'/')
    }
}

/// A path's directory and last name: ".", and the path, for a top-level
/// one.
fn split(p: &[u8]) -> (Vec<u8>, Vec<u8>) {
    match p.iter().rposition(|b| *b == b'/') {
        Some(i) => (p[..i].to_vec(), p[i + 1..].to_vec()),
        None => (b".".to_vec(), p.to_vec()),
    }
}

fn join(dir: &[u8], name: &[u8]) -> Vec<u8> {
    if dir == b"." {
        name.to_vec()
    } else {
        [dir, b"/", name].concat()
    }
}

impl Merge {
    /// Whether a layer above removed or replaced p.
    fn hidden(&self, p: &[u8]) -> bool {
        let mut q = p.to_vec();
        while q != b"." {
            if self.whiteouts.contains(&q) {
                return true;
            }
            if q != p && (self.opaque.contains(&q) || self.nondir.contains(&q)) {
                return true;
            }
            q = split(&q).0;
        }
        false
    }

    /// Adds one layer's entries, beneath those already added, to w.
    pub fn layer<O: Out>(&mut self, r: impl Read, w: &mut Writer<O>) -> io::Result<()> {
        let mut archive = tar::Archive::new(r);
        let mut whiteouts = vec![];
        let mut opaque = vec![];
        for entry in archive.entries()? {
            let mut entry = entry?;
            let kind = entry.header().entry_type();
            if kind.is_pax_global_extensions() {
                continue;
            }
            let p = clean(&entry.path_bytes());
            let (dir, base) = split(&p);
            if base == b".wh..wh..opq" {
                opaque.push(dir);
                continue;
            }
            if let Some(name) = base.strip_prefix(b".wh.") {
                whiteouts.push(join(&dir, name));
                continue;
            }
            // A path a layer above has decided is not this layer's to give.
            if self.seen.contains(&p) || self.hidden(&p) {
                continue;
            }
            self.seen.insert(p.clone());
            let is_dir =
                kind.is_dir() || (kind.as_byte() == 0 && entry.path_bytes().ends_with(b"/"));
            if !is_dir {
                self.nondir.insert(p.clone());
            }
            add_entry(&mut entry, p, is_dir, w)?;
        }
        // A layer's whiteouts and opaque directories hide only what is
        // beneath it, not its own entries.
        self.whiteouts.extend(whiteouts);
        self.opaque.extend(opaque);
        Ok(())
    }
}

/// Adds one tar entry at path, reading a regular file's data from it, as
/// Go's archive/tar reads a header: PAX records give its owner, its time to
/// the nanosecond, and its extended attributes (SCHILY.xattr.*).
fn add_entry<O: Out, R: Read>(
    entry: &mut tar::Entry<'_, R>,
    path: Vec<u8>,
    is_dir: bool,
    w: &mut Writer<O>,
) -> io::Result<()> {
    let h = entry.header();
    let kind = h.entry_type();
    let old = h.as_old();
    let mut e = Entry {
        path,
        perm: number(h.mode().map(u64::from), &old.mode)? as u32 & 0o7777,
        uid: number(h.uid(), &old.uid)? as u32,
        gid: number(h.gid(), &old.gid)? as u32,
        mtime: number(h.mtime(), &old.mtime)? as i64,
        major: device(h.device_major(), |u| &u.dev_major, |g| &g.dev_major, h)?,
        minor: device(h.device_minor(), |u| &u.dev_minor, |g| &g.dev_minor, h)?,
        ..Default::default()
    };
    let link = entry.link_name_bytes().map(|l| l.into_owned());
    let mut xattrs = BTreeMap::new();
    if let Some(pax) = entry.pax_extensions()? {
        for ext in pax {
            let ext = ext?;
            let key = ext.key_bytes();
            let value = ext.value_bytes();
            if let Some(name) = key.strip_prefix(b"SCHILY.xattr.") {
                xattrs.insert(name.to_vec(), value.to_vec());
                continue;
            }
            let text = String::from_utf8_lossy(value);
            match key {
                b"uid" => e.uid = text.parse().map_err(|_| bad_pax("uid"))?,
                b"gid" => e.gid = text.parse().map_err(|_| bad_pax("gid"))?,
                b"mtime" => (e.mtime, e.nsec) = pax_time(&text).ok_or_else(|| bad_pax("mtime"))?,
                _ => {}
            }
        }
    }
    e.xattrs = xattrs;
    let shown = || String::from_utf8_lossy(&e.path).into_owned();
    if !is_dir && (kind.is_file() || kind.is_gnu_sparse()) {
        e.kind = Some(Kind::Regular);
        e.size = entry.size();
        return w.add(&e, entry);
    }
    if kind.is_hard_link() {
        let target = clean(&link.unwrap_or_default());
        return w.link(&e.path, &target);
    }
    e.kind = Some(if is_dir {
        Kind::Dir
    } else if kind.is_symlink() {
        e.target = link.unwrap_or_default();
        Kind::Symlink
    } else if kind.is_character_special() {
        Kind::CharDevice
    } else if kind.is_block_special() {
        Kind::BlockDevice
    } else if kind.is_fifo() {
        Kind::Fifo
    } else {
        return Err(io::Error::other(format!(
            "{}: an entry of tar type {:?}",
            shown(),
            kind.as_byte() as char
        )));
    });
    w.add(&e, &mut io::empty())
}

/// A device number from a header, 0 where its format has none or leaves
/// it blank.
fn device(
    parsed: io::Result<Option<u32>>,
    ustar: fn(&tar::UstarHeader) -> &[u8; 8],
    gnu: fn(&tar::GnuHeader) -> &[u8; 8],
    h: &tar::Header,
) -> io::Result<u32> {
    let raw = h
        .as_ustar()
        .map(|u| &ustar(u)[..])
        .or(h.as_gnu().map(|g| &gnu(g)[..]))
        .unwrap_or(&[]);
    number(parsed.map(|d| d.unwrap_or(0) as u64), raw).map(|d| d as u32)
}

/// A header's number, or 0 for a field left blank (NULs or spaces), as Go's
/// archive/tar reads one.
fn number(parsed: io::Result<u64>, raw: &[u8]) -> io::Result<u64> {
    match parsed {
        Err(_) if raw.iter().all(|b| *b == 0 || *b == b' ') => Ok(0),
        r => r,
    }
}

fn bad_pax(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("a PAX {what} that is not a number"),
    )
}

/// A PAX time: seconds, and a fraction to the nanosecond, as Go's
/// archive/tar reads it ("1700000000.5", "-1.25").
fn pax_time(s: &str) -> Option<(i64, u32)> {
    let (secs, frac) = match s.split_once('.') {
        Some((a, b)) => (a, b),
        None => (s, ""),
    };
    let negative = secs.starts_with('-');
    let secs: i64 = secs.parse().ok()?;
    if !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let digits: String = frac.chars().chain(std::iter::repeat('0')).take(9).collect();
    let mut nsec: i64 = digits.parse().ok()?;
    let mut secs = secs;
    if negative && nsec != 0 {
        secs -= 1;
        nsec = 1_000_000_000 - nsec;
    }
    Some((secs, nsec as u32))
}

/// Writes the tree an image's layers describe to out as an EROFS image,
/// padded to a multiple of 2 MiB for persistent memory. next gives the
/// layers' uncompressed tar streams top first, and None after the last;
/// an error from it, or from a layer, stops the build and removes out. An
/// image whose layers never say what / is gets the root Docker gives it:
/// root's, and 0755.
pub fn build_erofs<'a>(
    out: &Path,
    mut next: impl FnMut() -> io::Result<Option<Box<dyn Read + 'a>>>,
) -> io::Result<()> {
    let built = (|| {
        let mut w = Writer::new(File::create(out)?);
        let mut m = Merge::default();
        while let Some(layer) = next()? {
            m.layer(layer, &mut w)?;
        }
        w.close()?;
        let f = w.into_inner();
        let size = f.metadata()?.len();
        f.set_len(size.div_ceil(EROFS_ALIGN) * EROFS_ALIGN)?;
        f.sync_all()
    })();
    if built.is_err() {
        let _ = std::fs::remove_file(out);
    }
    built
}
