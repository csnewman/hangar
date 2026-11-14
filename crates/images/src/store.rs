//! The worker's local copies of the images its environments boot from.
//!
//! The store holds copies, each of what a reference named at one moment:
//! the manifest a registry's tag pointed to, or one build of a local source.
//! A reference whose image moves on gets a second copy beside the first, so
//! an environment made from the first keeps booting from it -- its writable
//! layer was made over that root filesystem, and would find another's
//! files changed beneath it. [`Store::current`] looks up what a reference
//! names, which is the copy a new environment is made from.
//!
//! A copy is fetched the first time it is asked for and kept until it is
//! removed. It is pulled from its registry, unless the worker's
//! configuration names a local source for the reference, which is copied.
//!
//! Each copy is a directory holding files naming its reference and digest,
//! and its root filesystem: for a pulled copy, its EROFS image, read and
//! never mounted; for a local build, a copy of it as rootfs/. A directory
//! being fetched is named with ".fetching" and renamed into place when
//! complete, so a copy interrupted half made is fetched again rather than
//! booted. The layout is fixed: workers' disks hold copies in it, and
//! each is read as it is.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::merge::EROFS_ALIGN;
use crate::reference::Reference;
use crate::registry::{Auth, Client, HangarRegistry, Progress, Stage};

/// Where a copy keeps its EROFS image, in the copy's directory, so it goes
/// when the copy does.
pub const EROFS_FILE: &str = "image.erofs";

/// In a copy's directory, says its root filesystem is its EROFS image, as a
/// pulled copy's is.
const IMAGE_MARK: &str = "rootfs-is-image";

/// Where an image says what it offers, inside its root filesystem.
pub const IMAGE_INFO_PATH: &str = "usr/share/hangar/image.json";

/// How often a caller waiting on a fetch is told how far it has got.
const WATCH_EVERY: Duration = Duration::from_millis(500);

/// One copy: what a reference named, at a digest.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ImageCopy {
    pub reference: String,
    pub digest: String,
}

/// A copy as an environment boots it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// Its root filesystem: none for a pulled copy, which has only its
    /// EROFS image.
    pub base: Option<PathBuf>,
    /// The store's directory for the copy, where its EROFS image is kept.
    pub copy: PathBuf,
}

/// A copy as the worker reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalImage {
    pub reference: String,
    pub digest: String,
    /// The space it takes on disk.
    pub size_bytes: u64,
    /// ready, or fetching.
    pub state: &'static str,
    /// Whether it is what its reference names.
    pub current: bool,
}

/// What an image offers beyond booting.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct ImageInfo {
    /// Whether the image has a desktop to run.
    #[serde(default)]
    pub desktop: bool,
    /// Identifies the build, so a worker copying a local build tells one
    /// from the next. `hangar build` sets it.
    #[serde(default)]
    pub build: String,
}

struct Held {
    dir: PathBuf,
    /// When the copy was taken, which orders copies of one reference.
    at: SystemTime,
    /// Whether its root filesystem is its EROFS image.
    image: bool,
}

/// A fetch under way, which every caller asking for its copy shares.
#[derive(Default)]
struct Fetch {
    state: Mutex<FetchState>,
    changed: Condvar,
}

#[derive(Default)]
struct FetchState {
    progress: Option<Progress>,
    /// Set once it is over: Ok, or what went wrong.
    done: Option<Result<(), String>>,
}

#[derive(Default)]
struct Inner {
    held: HashMap<ImageCopy, Held>,
    current: HashMap<String, String>,
    fetching: HashMap<ImageCopy, Arc<Fetch>>,
    hangar: Option<HangarRegistry>,
}

pub struct Store {
    dir: PathBuf,
    sources: HashMap<String, PathBuf>,
    auth: HashMap<String, Auth>,
    inner: Mutex<Inner>,
    /// Serialises building each copy's EROFS image, by its path.
    building: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
}

fn other(msg: String) -> io::Error {
    io::Error::other(msg)
}

impl Store {
    /// Opens the store in dir, taking in the copies already there. sources
    /// maps a reference to a local build copied in for it, and auth signs
    /// pulls in to registries, by host.
    pub fn open(
        dir: &Path,
        sources: HashMap<String, PathBuf>,
        auth: HashMap<String, Auth>,
    ) -> io::Result<Store> {
        fs::create_dir_all(dir)?;
        let mut held = HashMap::new();
        for e in fs::read_dir(dir)? {
            let e = e?;
            let path = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            // Anything left mid-fetch by an earlier run is started again
            // from scratch when next asked for.
            if name.ends_with(".fetching") {
                let _ = fs::remove_dir_all(&path);
                continue;
            }
            if !e.file_type()?.is_dir() {
                continue;
            }
            let Ok(reference) = fs::read_to_string(path.join("ref")) else {
                continue;
            };
            let Ok(at) = fs::metadata(path.join("ref")).and_then(|m| m.modified()) else {
                continue;
            };
            // A copy with no digest file is a copy of its own, digest "",
            // which no lookup of its reference names.
            let digest = fs::read_to_string(path.join("digest")).unwrap_or_default();
            let c = ImageCopy {
                reference,
                digest: digest.trim().to_string(),
            };
            let image = path.join(IMAGE_MARK).exists();
            held.insert(
                c,
                Held {
                    dir: path,
                    at,
                    image,
                },
            );
        }
        Ok(Store {
            dir: dir.to_path_buf(),
            sources,
            auth,
            inner: Mutex::new(Inner {
                held,
                ..Default::default()
            }),
            building: Mutex::default(),
        })
    }

    /// Says where Hangar's own registry is; none for no registry.
    pub fn use_registry(&self, hangar: Option<HangarRegistry>) {
        self.inner.lock().unwrap().hangar = hangar;
    }

    fn client(&self) -> Client {
        let hangar = self.inner.lock().unwrap().hangar.clone();
        Client::new(self.auth.clone(), hangar)
    }

    /// Where a copy lives in the store: its reference made safe for a file
    /// name, with a short hash of the reference and digest, so two copies
    /// of one reference, or two references that differ only in unsafe
    /// characters, do not collide.
    pub fn path(&self, c: &ImageCopy) -> PathBuf {
        let sum = Sha256::digest(format!("{}@{}", c.reference, c.digest).as_bytes());
        let mut name = String::new();
        let mut in_run = false;
        for ch in c.reference.chars() {
            if ch.is_ascii_alphanumeric() || "._-".contains(ch) {
                name.push(ch);
                in_run = false;
            } else if !in_run {
                name.push('_');
                in_run = true;
            }
        }
        let mut name = name.trim_matches('_').to_string();
        name.truncate(80);
        self.dir.join(format!("{name}-{}", hex::encode(&sum[..4])))
    }

    /// Looks up what a reference names: the digest of the manifest its
    /// registry has for it, or which build its local source holds. The
    /// store remembers the answer, as what [`Store::list`] calls current.
    pub fn current(&self, reference: &str) -> io::Result<String> {
        let digest = match self.sources.get(reference) {
            Some(src) => build_of(src)?,
            None => {
                let r = Reference::parse(reference)
                    .map_err(|e| other(format!("parsing the image reference: {e}")))?;
                self.client()
                    .resolve(&r)
                    .map_err(|e| other(format!("resolving {r}: {e}")))?
            }
        };
        self.inner
            .lock()
            .unwrap()
            .current
            .insert(reference.to_string(), digest.clone());
        Ok(digest)
    }

    /// The digest of the copy of a reference taken last, if the store holds
    /// one.
    pub fn newest(&self, reference: &str) -> Option<String> {
        self.pick(reference, |a, b| a > b)
    }

    /// The digest of the copy of a reference taken first, if the store
    /// holds one.
    pub fn oldest(&self, reference: &str) -> Option<String> {
        self.pick(reference, |a, b| a < b)
    }

    fn pick(&self, reference: &str, better: fn(SystemTime, SystemTime) -> bool) -> Option<String> {
        let inner = self.inner.lock().unwrap();
        let mut best: Option<(&String, SystemTime)> = None;
        for (c, h) in &inner.held {
            if c.reference == reference && best.is_none_or(|(_, at)| better(h.at, at)) {
                best = Some((&c.digest, h.at));
            }
        }
        best.map(|(d, _)| d.clone())
    }

    /// The digest of the copy of a reference new environments are made
    /// over: what it named when last looked up, whether or not the store
    /// holds that copy yet, or, when it has not been looked up since the
    /// store opened, the newest copy held.
    pub fn latest(&self, reference: &str) -> Option<String> {
        let inner = self.inner.lock().unwrap();
        if let Some(d) = inner.current.get(reference) {
            return Some(d.clone());
        }
        inner
            .held
            .keys()
            .find(|c| c.reference == reference && is_current(&inner, c))
            .map(|c| c.digest.clone())
    }

    /// The store's copy c, fetched first if the store does not hold it.
    /// Callers asking for the same copy at once share one fetch, and each
    /// is told how far it has got through watch.
    pub fn get(&self, c: &ImageCopy, watch: &mut dyn FnMut(Progress)) -> io::Result<Image> {
        loop {
            let mut inner = self.inner.lock().unwrap();
            if let Some(h) = inner.held.get(c) {
                return Ok(image_of(h));
            }
            if let Some(f) = inner.fetching.get(c).cloned() {
                drop(inner);
                wait(&f, watch)?;
                continue;
            }
            let f = Arc::new(Fetch::default());
            inner.fetching.insert(c.clone(), f.clone());
            drop(inner);
            let dst = self.path(c);
            // This caller pulls, and hears how far it has got as the others
            // waiting on it do.
            let mut report = |p: Progress| {
                f.state.lock().unwrap().progress = Some(p);
                f.changed.notify_all();
                watch(p);
            };
            let got = self.fetch(c, &dst, &mut report);
            let mut inner = self.inner.lock().unwrap();
            inner.fetching.remove(c);
            if got.is_ok() {
                let image = dst.join(IMAGE_MARK).exists();
                inner.held.insert(
                    c.clone(),
                    Held {
                        dir: dst,
                        at: SystemTime::now(),
                        image,
                    },
                );
            }
            drop(inner);
            f.state.lock().unwrap().done =
                Some(got.as_ref().map(|_| ()).map_err(|e| e.to_string()));
            f.changed.notify_all();
            got?;
        }
    }

    /// Brings a copy into the store: copied from its local source if the
    /// configuration names one, and pulled from its registry by digest
    /// otherwise, straight into an EROFS image.
    fn fetch(&self, c: &ImageCopy, dst: &Path, report: &mut dyn FnMut(Progress)) -> io::Result<()> {
        let tmp = PathBuf::from(format!("{}.fetching", dst.display()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp)?;
        let made = (|| {
            if let Some(src) = self.sources.get(&c.reference) {
                // A local source holds one build, and only that build can be
                // copied from it.
                let build =
                    build_of(src).map_err(|e| other(format!("fetching {}: {e}", c.reference)))?;
                if build != c.digest {
                    return Err(other(format!(
                        "fetching {}: its local source holds {build}, not {}, and this worker holds no copy of that",
                        c.reference, c.digest
                    )));
                }
                report(Progress {
                    stage: Stage::Copy,
                    done: 0,
                    total: 0,
                    layer: 0,
                    layers: 0,
                });
                // Owners, modes, links and extended attributes are the
                // image, and reach the guest as they are, so the copy keeps
                // them all. Its blocks are shared with the source where the
                // filesystem can: reflinks on Linux, clones on APFS.
                let share = if cfg!(target_os = "macos") {
                    "-c"
                } else {
                    "--reflink=auto"
                };
                let out = Command::new("cp")
                    .args(["-a", share, "--"])
                    .arg(src)
                    .arg(tmp.join("rootfs"))
                    .output()?;
                if !out.status.success() {
                    return Err(other(format!(
                        "fetching {}: copying {}: {}",
                        c.reference,
                        src.display(),
                        String::from_utf8_lossy(&out.stderr).trim()
                    )));
                }
            } else {
                let r = Reference::parse(&c.reference)
                    .map_err(|e| other(format!("pulling {}: {e}", c.reference)))?;
                let blobs = tmp.join("blobs");
                self.client()
                    .pull(&r, &c.digest, &blobs, &tmp.join(EROFS_FILE), report)
                    .map_err(|e| other(format!("pulling {}: {e}", c.reference)))?;
                fs::remove_dir_all(&blobs)?;
                fs::write(tmp.join(IMAGE_MARK), b"")?;
            }
            fs::write(tmp.join("digest"), c.digest.as_bytes())?;
            fs::write(tmp.join("ref"), c.reference.as_bytes())
        })();
        if let Err(e) = made {
            let _ = fs::remove_dir_all(&tmp);
            return Err(e);
        }
        fs::rename(&tmp, dst)
    }

    /// Every copy in the store, and the ones being fetched, by reference
    /// and digest.
    pub fn list(&self) -> Vec<LocalImage> {
        let inner = self.inner.lock().unwrap();
        let mut entries: Vec<(ImageCopy, PathBuf, &'static str, bool)> = inner
            .held
            .iter()
            .map(|(c, h)| (c.clone(), h.dir.clone(), "ready", is_current(&inner, c)))
            .collect();
        for c in inner.fetching.keys() {
            let current = inner.current.get(&c.reference) == Some(&c.digest);
            let dir = PathBuf::from(format!("{}.fetching", self.path(c).display()));
            entries.push((c.clone(), dir, "fetching", current));
        }
        drop(inner);
        let mut out: Vec<LocalImage> = entries
            .into_iter()
            .map(|(c, dir, state, current)| LocalImage {
                reference: c.reference,
                digest: c.digest,
                size_bytes: allocated(&dir),
                state,
                current,
            })
            .collect();
        out.sort_by(|a, b| (&a.reference, &a.digest).cmp(&(&b.reference, &b.digest)));
        out
    }

    /// Deletes a copy. One being fetched is left to finish.
    pub fn remove(&self, c: &ImageCopy) -> io::Result<()> {
        let Some(h) = self.inner.lock().unwrap().held.remove(c) else {
            return Ok(());
        };
        match fs::remove_dir_all(&h.dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => {
                // Still there, and still held.
                self.inner.lock().unwrap().held.insert(c.clone(), h);
                Err(e)
            }
        }
    }

    /// Deletes every copy that is not current and that in_use says no
    /// environment uses: one a reference has moved on from, once the last
    /// environment made from it is gone. It reports the copies it deleted.
    pub fn prune(&self, in_use: impl Fn(&ImageCopy) -> bool) -> (Vec<ImageCopy>, Vec<io::Error>) {
        let stale: Vec<ImageCopy> = {
            let inner = self.inner.lock().unwrap();
            inner
                .held
                .keys()
                .filter(|c| !is_current(&inner, c) && !in_use(c))
                .cloned()
                .collect()
        };
        let mut removed = vec![];
        let mut errs = vec![];
        for c in stale {
            match self.remove(&c) {
                Ok(()) => removed.push(c),
                Err(e) => errs.push(e),
            }
        }
        (removed, errs)
    }

    /// An EROFS image of an image's root filesystem, built the first time:
    /// a pulled copy's is what it was pulled as.
    pub fn erofs(&self, img: &Image) -> io::Result<PathBuf> {
        let path = img.copy.join(EROFS_FILE);
        let lock = self
            .building
            .lock()
            .unwrap()
            .entry(path.clone())
            .or_default()
            .clone();
        let _held = lock.lock().unwrap();
        if path.exists() {
            return Ok(path);
        }
        let Some(base) = &img.base else {
            return Err(other(format!(
                "{}: a pulled copy without its image",
                img.copy.display()
            )));
        };
        let tmp = path.with_extension("erofs.building");
        let built = (|| {
            let mut w = hangar_erofs::Writer::new(fs::File::create(&tmp)?);
            w.add_dir(base)?;
            w.close()?;
            let f = w.into_inner();
            let size = f.metadata()?.len();
            f.set_len(size.div_ceil(EROFS_ALIGN) * EROFS_ALIGN)?;
            f.sync_all()?;
            fs::rename(&tmp, &path)
        })();
        if let Err(e) = built {
            let _ = fs::remove_file(&tmp);
            return Err(other(format!("building the image's EROFS image: {e}")));
        }
        Ok(path)
    }
}

fn image_of(h: &Held) -> Image {
    Image {
        base: (!h.image).then(|| h.dir.join("rootfs")),
        copy: h.dir.clone(),
    }
}

/// Waits for a fetch another caller started, telling watch how far it has
/// got.
fn wait(f: &Fetch, watch: &mut dyn FnMut(Progress)) -> io::Result<()> {
    let mut state = f.state.lock().unwrap();
    loop {
        if let Some(done) = &state.done {
            return done.clone().map_err(other);
        }
        if let Some(p) = state.progress {
            watch(p);
        }
        state = f.changed.wait_timeout(state, WATCH_EVERY).unwrap().0;
    }
}

/// Whether c is what its reference names, as last looked up, or the newest
/// copy of a reference not looked up since the store opened.
fn is_current(inner: &Inner, c: &ImageCopy) -> bool {
    if let Some(d) = inner.current.get(&c.reference) {
        return d == &c.digest;
    }
    let at = inner.held[c].at;
    !inner
        .held
        .iter()
        .any(|(o, h)| o.reference == c.reference && h.at > at)
}

/// Identifies the build a local source holds: the ID `hangar build` gives
/// each build, and for a root filesystem made some other way, the
/// directory as it is on disk, so replacing it counts as a new build.
pub fn build_of(rootfs: &Path) -> io::Result<String> {
    if let Some(info) = read_image_info(rootfs)?
        && !info.build.is_empty()
    {
        return Ok(format!("build:{}", info.build));
    }
    let fp = fingerprint(rootfs);
    if fp.is_empty() {
        return Err(other(format!(
            "local image {} is missing",
            rootfs.display()
        )));
    }
    Ok(format!(
        "local:{}",
        hex::encode(&Sha256::digest(fp.as_bytes())[..8])
    ))
}

/// What a file or directory is on disk: its path, size, time and identity,
/// which replacing it changes. Empty for one that is not there.
pub fn fingerprint(path: &Path) -> String {
    let Ok(m) = fs::metadata(path) else {
        return String::new();
    };
    let nanos = m.mtime() as i128 * 1_000_000_000 + m.mtime_nsec() as i128;
    format!(
        "{}:{}:{nanos}:{}:{}",
        path.display(),
        m.len(),
        m.dev(),
        m.ino()
    )
}

/// What the image whose root filesystem is rootfs offers; None for an
/// image that does not say.
pub fn read_image_info(rootfs: &Path) -> io::Result<Option<ImageInfo>> {
    match fs::read(rootfs.join(IMAGE_INFO_PATH)) {
        Ok(b) => parse_info(&b).map(Some),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// What an image offers, read from its EROFS image, as a pulled copy has
/// it.
pub fn read_image_info_erofs(image: &Path) -> io::Result<Option<ImageInfo>> {
    let img = hangar_erofs::Image::open(fs::File::open(image)?)?;
    match img.lookup(IMAGE_INFO_PATH.as_bytes(), true) {
        Ok(i) => parse_info(&img.read_all(&i)?).map(Some),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn parse_info(b: &[u8]) -> io::Result<ImageInfo> {
    serde_json::from_slice(b).map_err(|e| other(format!("{IMAGE_INFO_PATH}: {e}")))
}

/// The space a file or tree takes on disk, which for a sparse image is what
/// it holds rather than how big it claims to be.
pub fn allocated(path: &Path) -> u64 {
    let Ok(m) = fs::symlink_metadata(path) else {
        return 0;
    };
    if !m.is_dir() {
        return m.blocks() * 512;
    }
    fs::read_dir(path)
        .map(|rd| rd.flatten().map(|e| allocated(&e.path())).sum())
        .unwrap_or(0)
}
