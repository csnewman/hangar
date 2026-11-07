use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use notify::{RecursiveMode, Watcher};
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc};

use super::error::{Error, Result, at, between};
use super::fileops::{self, Clash, Encoding, Packed, Separator, TextProps, Uploads};
use super::paths::{named, relative, resolve};

/// The largest file read for a preview.
const MAX_READ: u64 = 4 << 20;

/// The most of a file read as bytes at once: an image whole, or a hex
/// view's chunk of a larger file.
pub const MAX_BYTES: u64 = 32 << 20;

/// Directories whose contents are never announced as changing: they churn,
/// and nobody browses them.
const QUIET: [&str; 6] = [
    ".git",
    "node_modules",
    ".cache",
    "target",
    "dist",
    "__pycache__",
];

/// How long changes gather before they are announced together.
const SETTLE: Duration = Duration::from_millis(150);

/// An entry in a folder: whether it is one, its size, and of a file whether
/// it is a link, runs, and may be written. `ignored` is git's say, added by
/// the connection.
#[derive(Debug, Serialize)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub link: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub exec: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub readonly: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub ignored: bool,
}

/// What a file is, for choosing how to show it: text, or bytes that are
/// not, and how large; whether the user may write it, and its version.
#[derive(Serialize)]
pub struct Probe {
    size: u64,
    binary: bool,
    writable: bool,
    version: String,
}

#[derive(Serialize)]
pub struct Bytes {
    size: u64,
    offset: u64,
    data: String,
    version: String,
}

/// A run of bytes a patch writes at an offset.
pub struct Run {
    pub offset: f64,
    pub data: String,
}

/// Files lists and changes the files of one folder. Paths are absolute, or
/// relative to the folder.
pub struct Files {
    root: PathBuf,
    changed: broadcast::Sender<Vec<String>>,
    watch: Mutex<Watch>,
    uploads: Uploads,
}

#[derive(Serialize)]
pub struct Transferred {
    from: String,
    to: String,
}

#[derive(Serialize)]
pub struct TransferResult {
    clashes: Vec<String>,
    done: Vec<Transferred>,
}

#[derive(Serialize)]
pub struct Trashed {
    path: String,
    id: String,
}

/// Runs blocking file work on the blocking pool.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::msg(e.to_string()))?
}

#[derive(Default)]
struct Watch {
    listeners: usize,
    watcher: Option<notify::RecommendedWatcher>,
}

/// Held by each listener; the watch stops when the last one goes.
pub struct Listening {
    files: Arc<Files>,
}

impl Drop for Listening {
    fn drop(&mut self) {
        let mut w = self.files.watch.lock().unwrap();
        w.listeners -= 1;
        if w.listeners == 0 {
            w.watcher = None;
        }
    }
}

impl Files {
    pub fn new(root: &Path) -> Arc<Self> {
        Arc::new(Files {
            root: root.to_path_buf(),
            changed: broadcast::channel(64).0,
            watch: Mutex::new(Watch::default()),
            uploads: Uploads::default(),
        })
    }

    pub fn abs(&self, path: &str) -> PathBuf {
        resolve(&self.root, Path::new(path))
    }

    pub async fn list(&self, path: &str) -> Result<Vec<Entry>> {
        let dir = self.abs(path);
        let mut rd = tokio::fs::read_dir(&dir).await.map_err(at(&dir))?;
        let mut out = Vec::new();
        while let Some(e) = rd.next_entry().await.map_err(at(&dir))? {
            let ft = e.file_type().await.map_err(at(&e.path()))?;
            let mut entry = Entry {
                name: e.file_name().to_string_lossy().into_owned(),
                dir: ft.is_dir(),
                size: 0,
                link: ft.is_symlink(),
                exec: false,
                readonly: false,
                ignored: false,
            };
            if ft.is_symlink() || ft.is_file() {
                // A dangling link is listed as a file.
                if let Ok(m) = tokio::fs::metadata(e.path()).await {
                    entry.dir = m.is_dir();
                    if !entry.dir {
                        entry.size = m.len();
                        entry.exec = m.mode() & 0o111 != 0;
                        entry.readonly = m.mode() & 0o222 == 0;
                    }
                }
            }
            out.push(entry);
        }
        out.sort_by(|a, b| match (a.dir, b.dir) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => collate(&a.name, &b.name),
        });
        Ok(out)
    }

    /// A file's text, for a preview; None for a file too large to show or
    /// not text.
    pub async fn read(&self, path: &str) -> Result<Option<String>> {
        let p = self.abs(path);
        let m = tokio::fs::metadata(&p).await.map_err(at(&p))?;
        if !m.is_file() || m.len() > MAX_READ {
            return Ok(None);
        }
        let buf = tokio::fs::read(&p).await.map_err(at(&p))?;
        if buf[..buf.len().min(8000)].contains(&0) {
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
    }

    /// Whether a file is text: binary when its start holds a NUL, as git
    /// judges.
    pub async fn probe(&self, path: &str) -> Result<Probe> {
        let p = self.abs(path);
        let m = tokio::fs::metadata(&p).await.map_err(at(&p))?;
        if !m.is_file() {
            return Err(Error::msg(format!("{path} is not a file")));
        }
        let mut f = tokio::fs::File::open(&p).await.map_err(at(&p))?;
        let mut buf = vec![0u8; m.len().min(8192) as usize];
        let n = read_full(&mut f, &mut buf).await.map_err(at(&p))?;
        Ok(Probe {
            size: m.len(),
            binary: buf[..n].contains(&0),
            writable: writable(&p),
            version: version(&p).await?,
        })
    }

    /// Part of a file, base64: `length` bytes from `offset`, at most
    /// MAX_BYTES, fewer at its end.
    pub async fn bytes(&self, path: &str, offset: f64, length: f64) -> Result<Bytes> {
        let p = self.abs(path);
        let m = tokio::fs::metadata(&p).await.map_err(at(&p))?;
        if !m.is_file() {
            return Err(Error::msg(format!("{path} is not a file")));
        }
        let size = m.len();
        let from = clamp(offset.floor(), 0.0, size as f64) as u64;
        let n = clamp(length.floor(), 0.0, (MAX_BYTES.min(size - from)) as f64) as usize;
        let mut f = tokio::fs::File::open(&p).await.map_err(at(&p))?;
        f.seek(std::io::SeekFrom::Start(from))
            .await
            .map_err(at(&p))?;
        let mut buf = vec![0u8; n];
        let got = read_full(&mut f, &mut buf).await.map_err(at(&p))?;
        Ok(Bytes {
            size,
            offset: from,
            data: BASE64.encode(&buf[..got]),
            version: version(&p).await?,
        })
    }

    /// Writes bytes over a file's own, in place, each run at its offset, if
    /// the file is still the version they were made to; changed since, it
    /// refuses, so nobody's write is lost.
    pub async fn patch(&self, path: &str, at_version: &str, runs: Vec<Run>) -> Result<String> {
        let p = self.abs(path);
        if version(&p).await? != at_version {
            return Err(Error::msg(format!(
                "{path} changed on disk since it was read"
            )));
        }
        let size = tokio::fs::metadata(&p).await.map_err(at(&p))?.len() as f64;
        let mut bufs = Vec::with_capacity(runs.len());
        for r in runs {
            let data = BASE64
                .decode(r.data.as_bytes())
                .map_err(|e| Error::msg(e.to_string()))?;
            bufs.push((r.offset.floor(), data));
        }
        if bufs.iter().map(|(_, b)| b.len() as u64).sum::<u64>() > MAX_BYTES {
            return Err(Error::msg("too many bytes changed at once"));
        }
        if bufs
            .iter()
            .any(|(o, b)| *o < 0.0 || o + b.len() as f64 > size)
        {
            return Err(Error::msg("a change lies outside the file"));
        }
        let mut f = tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&p)
            .await
            .map_err(at(&p))?;
        for (o, b) in bufs {
            f.seek(std::io::SeekFrom::Start(o as u64))
                .await
                .map_err(at(&p))?;
            f.write_all(&b).await.map_err(at(&p))?;
        }
        f.flush().await.map_err(at(&p))?;
        drop(f);
        version(&p).await
    }

    pub async fn create(&self, path: &str) -> Result<()> {
        let p = self.abs(path);
        tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&p)
            .await
            .map_err(at(&p))?;
        Ok(())
    }

    pub async fn mkdir(&self, path: &str) -> Result<()> {
        let p = self.abs(path);
        tokio::fs::create_dir_all(&p).await.map_err(at(&p))
    }

    pub async fn rename(&self, from: &str, to: &str) -> Result<()> {
        let (from, to) = (self.abs(from), self.abs(to));
        tokio::fs::rename(&from, &to)
            .await
            .map_err(between(&from, &to))
    }

    pub async fn remove(&self, path: &str) -> Result<()> {
        let p = self.abs(path);
        if p == self.root || p == Path::new("/") {
            return Err(Error::msg("refusing to delete the root"));
        }
        let m = tokio::fs::symlink_metadata(&p).await.map_err(at(&p))?;
        if m.is_dir() {
            tokio::fs::remove_dir_all(&p).await.map_err(at(&p))
        } else {
            tokio::fs::remove_file(&p).await.map_err(at(&p))
        }
    }

    /// A path to change wholesale -- move, copy, trash -- which the root, and
    /// /, are not.
    fn guarded(&self, path: &str) -> Result<PathBuf> {
        let p = self.abs(path);
        if p == self.root || p == Path::new("/") {
            return Err(Error::msg("refusing to move the root"));
        }
        Ok(p)
    }

    /// Copies or moves paths into a folder, the paths it made named as the
    /// browser named the ones it was given.
    pub async fn transfer(
        &self,
        move_: bool,
        from: &[String],
        to: &str,
        clash: Clash,
        name: Option<String>,
    ) -> Result<TransferResult> {
        let srcs = from
            .iter()
            .map(|f| self.guarded(f))
            .collect::<Result<Vec<_>>>()?;
        let to = self.abs(to);
        let r =
            blocking(move || fileops::transfer(move_, &srcs, &to, clash, name.as_deref())).await?;
        Ok(TransferResult {
            clashes: r.clashes,
            done: r
                .done
                .iter()
                .map(|(f, t)| Transferred {
                    from: named(&self.root, f),
                    to: named(&self.root, t),
                })
                .collect(),
        })
    }

    /// Puts paths in the desktop's trash.
    pub async fn trash(&self, paths: &[String]) -> Result<Vec<Trashed>> {
        let ps = paths
            .iter()
            .map(|p| self.guarded(p))
            .collect::<Result<Vec<_>>>()?;
        let out = blocking(move || fileops::trash(&ps)).await?;
        Ok(out
            .into_iter()
            .map(|(p, id)| Trashed {
                path: named(&self.root, &p),
                id,
            })
            .collect())
    }

    /// Puts back what trash put there.
    pub async fn restore(&self, items: Vec<(String, String)>) -> Result<()> {
        let items: Vec<(PathBuf, String)> = items
            .into_iter()
            .map(|(p, id)| (self.abs(&p), id))
            .collect();
        blocking(move || fileops::restore(&items)).await
    }

    pub fn upload_start(&self, path: &str) -> Result<String> {
        self.uploads.start(&self.abs(path))
    }

    pub fn upload_write(&self, token: &str, data: &str) -> Result<()> {
        let bytes = BASE64
            .decode(data.as_bytes())
            .map_err(|e| Error::msg(e.to_string()))?;
        self.uploads.write(token, &bytes)
    }

    pub fn upload_end(&self, token: &str, replace: bool) -> Result<String> {
        Ok(named(&self.root, &self.uploads.end(token, replace)?))
    }

    pub fn upload_abort(&self, token: &str) {
        self.uploads.abort(token)
    }

    /// Packs paths for downloading, into a file the browser reads with bytes
    /// and then removes with unzip.
    pub async fn zip(&self, paths: &[String]) -> Result<Packed> {
        let ps: Vec<PathBuf> = paths.iter().map(|p| self.abs(p)).collect();
        blocking(move || fileops::zip(&ps)).await
    }

    pub async fn unzip(&self, path: &str) {
        let p = PathBuf::from(path);
        if fileops::is_packed(&p) {
            let _ = tokio::fs::remove_file(p).await;
        }
    }

    pub async fn set_mode(
        &self,
        path: &str,
        exec: Option<bool>,
        writable: Option<bool>,
    ) -> Result<u32> {
        let p = self.abs(path);
        blocking(move || fileops::set_mode(&p, exec, writable)).await
    }

    pub async fn text_props(&self, path: &str) -> Result<TextProps> {
        let p = self.abs(path);
        blocking(move || fileops::text_props(&p)).await
    }

    pub async fn convert(
        &self,
        path: &str,
        separator: Option<Separator>,
        encoding: Option<Encoding>,
        bom: Option<bool>,
    ) -> Result<TextProps> {
        let p = self.abs(path);
        blocking(move || fileops::convert(&p, separator, encoding, bom)).await
    }

    /// The directories whose contents changed, a few at a time, from when
    /// the first listener starts listening until the last stops.
    pub fn listen(self: &Arc<Self>) -> (broadcast::Receiver<Vec<String>>, Listening) {
        let rx = self.changed.subscribe();
        let mut w = self.watch.lock().unwrap();
        w.listeners += 1;
        if w.watcher.is_none() {
            // The root may not exist yet; the browser lists on demand anyway.
            w.watcher = self.start().ok();
        }
        drop(w);
        (
            rx,
            Listening {
                files: self.clone(),
            },
        )
    }

    fn start(&self) -> notify::Result<notify::RecommendedWatcher> {
        let (tx, mut rx) = mpsc::unbounded_channel::<PathBuf>();
        let root = self.root.clone();
        let mut watcher = notify::recommended_watcher(move |ev: notify::Result<notify::Event>| {
            if let Ok(ev) = ev
                && changes(&ev)
            {
                for p in ev.paths {
                    let _ = tx.send(p);
                }
            }
        })?;
        watcher.watch(&self.root, RecursiveMode::Recursive)?;
        let changed = self.changed.clone();
        tokio::spawn(async move {
            // Ends when the watcher, and its sender with it, is dropped.
            while let Some(first) = rx.recv().await {
                let mut dirs = BTreeSet::new();
                note(&root, &first, &mut dirs);
                loop {
                    match tokio::time::timeout(SETTLE, rx.recv()).await {
                        Ok(Some(p)) => note(&root, &p, &mut dirs),
                        Ok(None) => return,
                        Err(_) => break,
                    }
                }
                if !dirs.is_empty() {
                    let _ = changed.send(dirs.into_iter().collect());
                }
            }
        });
        Ok(watcher)
    }
}

/// Whether an event is a change: on Linux a file being opened or closed
/// without writing is reported too, which reading it does.
pub fn changes(ev: &notify::Event) -> bool {
    use notify::event::{AccessKind, AccessMode, EventKind};
    match ev.kind {
        EventKind::Access(AccessKind::Close(AccessMode::Write)) => true,
        EventKind::Access(_) => false,
        _ => true,
    }
}

/// Adds the directory a changed path is in, unless it is under a quiet one.
fn note(root: &Path, p: &Path, dirs: &mut BTreeSet<String>) {
    let rel = relative(root, p);
    if rel
        .components()
        .any(|c| QUIET.iter().any(|q| c.as_os_str() == *q))
    {
        return;
    }
    if let Some(dir) = p.parent() {
        dirs.insert(dir.display().to_string());
    }
}

/// A file's version is its modification time to the nanosecond and its
/// size: a write by anyone changes it.
async fn version(p: &Path) -> Result<String> {
    let m = tokio::fs::metadata(p).await.map_err(at(p))?;
    let ns = m.mtime() as i128 * 1_000_000_000 + m.mtime_nsec() as i128;
    Ok(format!("{ns}:{}", m.len()))
}

/// Whether the user may write the file.
pub fn writable(p: &Path) -> bool {
    let Ok(c) = CString::new(p.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: c is a NUL-terminated path.
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

async fn read_full(f: &mut tokio::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        let got = f.read(&mut buf[n..]).await?;
        if got == 0 {
            break;
        }
        n += got;
    }
    Ok(n)
}

fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    v.max(lo).min(hi)
}

/// Names in the order a person expects: letters together whatever their
/// case, lowercase first when two differ only in case. Node's localeCompare
/// orders by Unicode collation too, which this approximates.
pub fn collate(a: &str, b: &str) -> Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| b.cmp(a))
}
