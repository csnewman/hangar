//! Built images as an OCI image layout, to push: an image per tier, each
//! named in the index by its tier. The first tier is one layer; each after
//! it is the tier before plus a layer of what it changes, so a registry
//! stores the layers they share once and a worker that pulls both fetches
//! them once.
//!
//! A layer is a gzipped tar of what a tree adds or changes over the one
//! beneath it, with OCI whiteouts (`.wh.<name>`) for what it removes, as
//! containerd writes a diff: a file is the same when its owner, mode,
//! size and modification time are, its content compared only when the
//! times are whole seconds and so may have been truncated. The trees are
//! read as they are, owners and all, so it runs as root.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::{self, File, Metadata};
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The images built for each distribution, in build order: each is built
/// on the tree of the one before it.
pub const TIERS: [&str; 2] = ["minimal", "base"];

const LAYER: &str = "application/vnd.oci.image.layer.v1.tar+gzip";
const CONFIG: &str = "application/vnd.oci.image.config.v1+json";
const MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const INDEX: &str = "application/vnd.oci.image.index.v1+json";
const REF_NAME: &str = "org.opencontainers.image.ref.name";

/// A blob as a manifest or index names it.
struct Descriptor {
    media_type: &'static str,
    digest: String,
    size: u64,
}

impl Descriptor {
    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "mediaType": self.media_type,
            "digest": self.digest,
            "size": self.size,
        })
    }
}

/// Writes the layout at dir from each tier's root filesystem, in TIERS'
/// order, for arch (Go's naming: arm64, amd64), its configurations stamped
/// created (RFC 3339).
pub fn write(dir: &Path, rootfs: &[PathBuf], arch: &str, created: &str) -> io::Result<()> {
    let blobs = dir.join("blobs").join("sha256");
    fs::create_dir_all(&blobs)?;
    fs::write(
        dir.join("oci-layout"),
        serde_json::to_vec(&serde_json::json!({ "imageLayoutVersion": "1.0.0" }))?,
    )?;
    let platform = serde_json::json!({ "architecture": arch, "os": "linux" });
    let mut layers: Vec<Descriptor> = Vec::new();
    let mut diff_ids: Vec<String> = Vec::new();
    let mut manifests = Vec::new();
    let mut lower: Option<&Path> = None;
    for (tier, upper) in TIERS.iter().zip(rootfs) {
        let (layer, diff_id) = write_layer(&blobs, lower, upper)
            .map_err(|e| io::Error::new(e.kind(), format!("{tier}: {e}")))?;
        layers.push(layer);
        diff_ids.push(diff_id);
        lower = Some(upper);

        let config = write_blob(
            &blobs,
            CONFIG,
            &serde_json::json!({
                "created": created,
                "architecture": arch,
                "os": "linux",
                "config": {},
                "rootfs": { "type": "layers", "diff_ids": diff_ids },
            }),
        )?;
        let manifest = write_blob(
            &blobs,
            MANIFEST,
            &serde_json::json!({
                "schemaVersion": 2,
                "mediaType": MANIFEST,
                "config": config.json(),
                "layers": layers.iter().map(Descriptor::json).collect::<Vec<_>>(),
            }),
        )?;
        let mut m = manifest.json();
        m["platform"] = platform.clone();
        m["annotations"] = serde_json::json!({ REF_NAME: tier });
        manifests.push(m);
    }
    fs::write(
        dir.join("index.json"),
        serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2,
            "mediaType": INDEX,
            "manifests": manifests,
        }))?,
    )
}

fn write_blob(
    blobs: &Path,
    media_type: &'static str,
    v: &serde_json::Value,
) -> io::Result<Descriptor> {
    let b = serde_json::to_vec(v)?;
    let digest = hex(&Sha256::digest(&b));
    fs::write(blobs.join(&digest), &b)?;
    Ok(Descriptor {
        media_type,
        digest: format!("sha256:{digest}"),
        size: b.len() as u64,
    })
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A writer that hashes and counts what passes through it.
struct Hashing<W> {
    inner: W,
    hash: Sha256,
    n: u64,
}

impl<W: Write> Hashing<W> {
    fn new(inner: W) -> Hashing<W> {
        Hashing {
            inner,
            hash: Sha256::new(),
            n: 0,
        }
    }

    fn finish(self) -> (W, String, u64) {
        (
            self.inner,
            format!("sha256:{}", hex(&self.hash.finalize())),
            self.n,
        )
    }
}

impl<W: Write> Write for Hashing<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hash.update(&buf[..n]);
        self.n += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Writes what upper changes over lower (nothing: an empty tree) as a
/// gzipped layer blob, giving its descriptor and the digest of its tar.
fn write_layer(
    blobs: &Path,
    lower: Option<&Path>,
    upper: &Path,
) -> io::Result<(Descriptor, String)> {
    let tmp = blobs.join(format!(".layer-{}", std::process::id()));
    let written = (|| {
        let compressed = Hashing::new(File::create(&tmp)?);
        let gz = flate2::write::GzEncoder::new(compressed, flate2::Compression::default());
        let mut d = Diff {
            tar: tar::Builder::new(Hashing::new(gz)),
            upper: upper.to_path_buf(),
            inodes: HashMap::new(),
            dirs: HashSet::new(),
        };
        d.dir(b"", lower, upper)?;
        let (gz, diff_id, _) = d.tar.into_inner()?.finish();
        let (f, digest, size) = gz.finish()?.finish();
        f.sync_all()?;
        f.set_permissions(fs::Permissions::from_mode(0o644))?;
        Ok::<_, io::Error>((digest, diff_id, size))
    })();
    let (digest, diff_id, size) = match written {
        Ok(w) => w,
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
    };
    fs::rename(&tmp, blobs.join(digest.trim_start_matches("sha256:")))?;
    Ok((
        Descriptor {
            media_type: LAYER,
            digest,
            size,
        },
        diff_id,
    ))
}

/// A layer being written: the tar, the tree it is of, the files written
/// so far by inode (for hard links), and the directories it has.
struct Diff<W: Write> {
    tar: tar::Builder<W>,
    upper: PathBuf,
    inodes: HashMap<(u64, u64), Vec<u8>>,
    dirs: HashSet<Vec<u8>>,
}

fn join(dir: &[u8], name: &[u8]) -> Vec<u8> {
    if dir.is_empty() {
        name.to_vec()
    } else {
        [dir, b"/", name].concat()
    }
}

fn os(p: &[u8]) -> &Path {
    Path::new(std::ffi::OsStr::from_bytes(p))
}

fn names(dir: &Path) -> io::Result<Vec<Vec<u8>>> {
    fs::read_dir(dir)?
        .map(|d| d.map(|d| d.file_name().as_bytes().to_vec()))
        .collect()
}

fn with(path: &Path) -> impl Fn(io::Error) -> io::Error + '_ {
    move |e| io::Error::new(e.kind(), format!("{}: {e}", path.display()))
}

impl<W: Write> Diff<W> {
    /// Writes what the directory upper, at rel, changes over lower, which
    /// may be missing or not a directory.
    fn dir(&mut self, rel: &[u8], lower: Option<&Path>, upper: &Path) -> io::Result<()> {
        let mut all: BTreeSet<Vec<u8>> = names(upper).map_err(with(upper))?.into_iter().collect();
        let lower = lower.filter(|l| fs::symlink_metadata(l).is_ok_and(|m| m.is_dir()));
        if let Some(l) = lower {
            all.extend(names(l).map_err(with(l))?);
        }
        for name in all {
            let child = join(rel, &name);
            let u = upper.join(os(&name));
            let um = fs::symlink_metadata(&u).ok();
            let l = lower.map(|l| l.join(os(&name)));
            let lm = l.as_ref().and_then(|l| fs::symlink_metadata(l).ok());
            match (lm, um) {
                (None, None) => {}
                (Some(_), None) => self.whiteout(rel, &name)?,
                (None, Some(um)) => self.add(&child, &u, &um)?,
                (Some(lm), Some(um)) => {
                    let l = l.expect("a lower path");
                    if lm.is_dir() && um.is_dir() {
                        if !same_meta(&l, &lm, &u, &um)? {
                            self.entry(&child, &u, &um)?;
                        }
                        self.dir(&child, Some(&l), &u)?;
                    } else if !same_file(&l, &lm, &u, &um)? {
                        self.add(&child, &u, &um)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Writes path, at rel, and if it is a directory everything in it.
    fn add(&mut self, rel: &[u8], path: &Path, meta: &Metadata) -> io::Result<()> {
        self.entry(rel, path, meta)?;
        if meta.is_dir() {
            self.dir(rel, None, path)?;
        }
        Ok(())
    }

    /// Writes the directories above rel the layer does not have yet, as
    /// the tree has them.
    fn parents(&mut self, rel: &[u8]) -> io::Result<()> {
        let mut at = 0;
        while let Some(i) = rel[at..].iter().position(|&c| c == b'/') {
            let dir = rel[..at + i].to_vec();
            if !self.dirs.contains(&dir) {
                let path = self.upper.join(os(&dir));
                let meta = fs::symlink_metadata(&path).map_err(with(&path))?;
                self.header(&dir, &path, &meta)?;
            }
            at += i + 1;
        }
        Ok(())
    }

    fn whiteout(&mut self, dir: &[u8], name: &[u8]) -> io::Result<()> {
        let rel = join(dir, &[b".wh.", name].concat());
        self.parents(&rel)?;
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Regular);
        h.set_mode(0);
        h.set_uid(0);
        h.set_gid(0);
        h.set_size(0);
        h.set_mtime(0);
        self.tar.append_data(&mut h, os(&rel), io::empty())
    }

    fn entry(&mut self, rel: &[u8], path: &Path, meta: &Metadata) -> io::Result<()> {
        if meta.file_type().is_socket() || self.dirs.contains(rel) {
            return Ok(());
        }
        self.parents(rel)?;
        self.header(rel, path, meta)
    }

    /// Writes one thing's header, and a regular file's data.
    fn header(&mut self, rel: &[u8], path: &Path, meta: &Metadata) -> io::Result<()> {
        let ft = meta.file_type();
        let mut h = tar::Header::new_gnu();
        h.set_mode(meta.mode() & 0o7777);
        h.set_uid(meta.uid() as u64);
        h.set_gid(meta.gid() as u64);
        h.set_mtime(meta.mtime().max(0) as u64);
        h.set_size(0);
        let mut pax: Vec<(&str, Vec<u8>)> = Vec::new();
        if meta.mtime_nsec() != 0 {
            pax.push((
                "mtime",
                format!("{}.{:09}", meta.mtime(), meta.mtime_nsec()).into_bytes(),
            ));
        }
        if let Some(cap) = capability(path)? {
            pax.push(("SCHILY.xattr.security.capability", cap));
        }
        let name = os(rel);
        if ft.is_file() && meta.nlink() > 1 {
            let id = (meta.dev(), meta.ino());
            if let Some(first) = self.inodes.get(&id).cloned() {
                h.set_entry_type(tar::EntryType::Link);
                self.pax(&pax)?;
                return self.tar.append_link(&mut h, name, os(&first));
            }
            self.inodes.insert(id, rel.to_vec());
        }
        if ft.is_dir() {
            h.set_entry_type(tar::EntryType::Directory);
            self.dirs.insert(rel.to_vec());
            self.pax(&pax)?;
            let dir = [rel, b"/"].concat();
            return self.tar.append_data(&mut h, os(&dir), io::empty());
        }
        if ft.is_file() {
            h.set_entry_type(tar::EntryType::Regular);
            h.set_size(meta.len());
            let f = File::open(path).map_err(with(path))?;
            self.pax(&pax)?;
            return self.tar.append_data(&mut h, name, f.take(meta.len()));
        }
        if ft.is_symlink() {
            h.set_entry_type(tar::EntryType::Symlink);
            let target = fs::read_link(path).map_err(with(path))?;
            self.pax(&pax)?;
            return self.tar.append_link(&mut h, name, target);
        }
        if ft.is_char_device() || ft.is_block_device() {
            h.set_entry_type(if ft.is_char_device() {
                tar::EntryType::Char
            } else {
                tar::EntryType::Block
            });
            let (major, minor) = split_dev(meta.rdev());
            h.set_device_major(major)?;
            h.set_device_minor(minor)?;
        } else if ft.is_fifo() {
            h.set_entry_type(tar::EntryType::Fifo);
        } else {
            return Err(io::Error::other(format!(
                "{}: a file of a kind a layer has none of",
                path.display()
            )));
        }
        self.pax(&pax)?;
        self.tar.append_data(&mut h, name, io::empty())
    }

    /// Writes PAX records for the entry that follows.
    fn pax(&mut self, records: &[(&str, Vec<u8>)]) -> io::Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        let mut data = Vec::new();
        for (k, v) in records {
            // A record's length counts its own digits.
            let rest = k.len() + v.len() + 3;
            let mut n = rest + rest.to_string().len();
            if n.to_string().len() != rest.to_string().len() {
                n += 1;
            }
            data.extend_from_slice(format!("{n} {k}=").as_bytes());
            data.extend_from_slice(v);
            data.push(b'\n');
        }
        let mut h = tar::Header::new_ustar();
        h.set_entry_type(tar::EntryType::XHeader);
        h.set_path("PaxHeader")?;
        h.set_mode(0o644);
        h.set_size(data.len() as u64);
        h.set_cksum();
        self.tar.append(&h, &data[..])
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

/// A file's capabilities (security.capability), the one extended
/// attribute a layer carries, as Docker's and containerd's do.
#[cfg(target_os = "linux")]
fn capability(path: &Path) -> io::Result<Option<Vec<u8>>> {
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)?;
    let name = c"security.capability";
    let mut buf = vec![0u8; 256];
    let n = unsafe {
        libc::lgetxattr(
            c.as_ptr(),
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    if n < 0 {
        let e = io::Error::last_os_error();
        return match e.raw_os_error() {
            Some(libc::ENODATA) | Some(libc::ENOTSUP) => Ok(None),
            _ => Err(with(path)(e)),
        };
    }
    buf.truncate(n as usize);
    Ok(Some(buf))
}

#[cfg(not(target_os = "linux"))]
fn capability(_: &Path) -> io::Result<Option<Vec<u8>>> {
    Ok(None)
}

/// Whether two directories are the same as a layer sees them: their type,
/// mode, owner and capabilities.
fn same_meta(l: &Path, lm: &Metadata, u: &Path, um: &Metadata) -> io::Result<bool> {
    Ok(lm.mode() == um.mode()
        && lm.uid() == um.uid()
        && lm.gid() == um.gid()
        && lm.rdev() == um.rdev()
        && capability(l)? == capability(u)?)
}

/// Whether two things that are not both directories are the same, as
/// containerd tells: the same file, or the same metadata, size and time;
/// times of whole seconds may have been truncated, so then the content
/// (or a symlink's target) decides.
fn same_file(l: &Path, lm: &Metadata, u: &Path, um: &Metadata) -> io::Result<bool> {
    if lm.dev() == um.dev() && lm.ino() == um.ino() {
        return Ok(true);
    }
    if !same_meta(l, lm, u, um)? || lm.is_dir() || um.is_dir() {
        return Ok(false);
    }
    if lm.len() != um.len() || lm.mtime() != um.mtime() {
        return Ok(false);
    }
    if lm.mtime_nsec() != 0 || um.mtime_nsec() != 0 {
        return Ok(lm.mtime_nsec() == um.mtime_nsec());
    }
    if lm.file_type().is_symlink() {
        return Ok(fs::read_link(l)? == fs::read_link(u)?);
    }
    if !lm.is_file() || lm.len() == 0 {
        return Ok(true);
    }
    same_content(l, u)
}

fn same_content(a: &Path, b: &Path) -> io::Result<bool> {
    let (mut fa, mut fb) = (File::open(a)?, File::open(b)?);
    let (mut ba, mut bb) = (vec![0u8; 64 << 10], vec![0u8; 64 << 10]);
    loop {
        let n = read_full(&mut fa, &mut ba)?;
        let m = read_full(&mut fb, &mut bb)?;
        if n != m || ba[..n] != bb[..m] {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
    }
}

fn read_full(f: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}
