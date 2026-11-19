//! A linked person's copies' files, kept the same in every place they link
//! (docs/places.md §4.4). The copies linked are their own copy of the
//! built-in profile and of each of their own packs, and the shared copies
//! they own of those. A copy's ID differs from place to place, so each is
//! known to every place by a name of its own (`Linked::key`): `profile`,
//! `pack:<pack>` for a person's own copy of their pack, `copy:<copy>` for a
//! shared one, whose ID every place keeps (`linksync`'s copy records).
//!
//! Each file of such a copy has a version (`FileVersion`): a version
//! vector, the time of the latest change each place made to it, which says
//! whether one version follows from another or was made without having
//! seen it; the place whose change it is; and its content's SHA-256 and
//! mode. A version that follows from another replaces it. Of two made
//! apart, the later change wins, and the other, where it was held, goes
//! into the copy's history (`Replaced`), from which it can be restored.
//! A change is timed by the file's modification time, so of two places'
//! files found when they are first linked the one changed last wins.
//!
//! The versions are kept in the files root beside the copies, in
//! `.links/<copy>/` (`files.json`, and `history.json` with `history/`),
//! which no environment is served. A place notices its own changes by
//! scanning a copy: whenever its workers say an environment changed it,
//! the Files page writes to it, and before two places compare versions.
//!
//! No file is held in memory whole: one is hashed, cut and sent a window
//! at a time from the file itself, and one coming in is written to
//! `.links/incoming/` as its pieces arrive, then renamed into its copy.
//! One that stops coming part way is kept a while, and a place that wants
//! it again says how much it holds (`Held`), for the rest alone to come.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use uuid::Uuid;

use crate::chunks::At;
pub use crate::chunks::{Base, Delta};
use crate::fail::{self, Fail};
use crate::http::App;
use crate::root::Root;
use crate::store::{Error as StoreError, Result as StoreResult, Tx};
use crate::users::Principal;

/// The largest file linked: a larger one stays in its place.
pub const MAX_FILE: u64 = 4 << 30;

/// The largest file a place speaking link level 1 takes, which holds
/// each file in memory whole.
pub const LEVEL1_MAX_FILE: u64 = 64 << 20;

/// Where files coming in are written as their pieces arrive.
const INCOMING: &str = ".links/incoming";

/// How long a file that stopped coming part way is kept for the rest.
const PARTIAL_KEPT: Duration = Duration::from_secs(24 * 3600);

/// How much of a file one message carries.
pub const PIECE: usize = 1 << 20;

/// How many replaced versions of a file are kept, and for how long.
const HISTORY_KEPT: usize = 5;
const HISTORY_DAYS: i64 = 7;

/// How long a deleted file's version is kept, so a place that still has
/// the file does not bring it back.
const DELETED_DAYS: i64 = 30;

/// Where the versions and history are kept in the files root.
const META: &str = ".links";

/// A file's place-to-place version clock: the time of the latest change
/// each place made to it, in milliseconds.
pub type Vector = BTreeMap<Uuid, i64>;

/// A version of one of a copy's files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileVersion {
    pub vv: Vector,
    /// The place whose change this is.
    pub origin: Uuid,
    /// The content's SHA-256; empty when the file is deleted.
    #[serde(default)]
    pub digest: String,
    #[serde(default)]
    pub mode: u32,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub deleted: bool,
}

impl FileVersion {
    /// When the change it is was made: its place's time in the vector.
    pub fn time(&self) -> i64 {
        self.vv.get(&self.origin).copied().unwrap_or(0)
    }

    fn same_content(&self, o: &FileVersion) -> bool {
        self.deleted == o.deleted
            && (self.deleted || (self.digest == o.digest && self.mode == o.mode))
    }
}

/// Whether every change b counts, a counts too.
fn covers(a: &Vector, b: &Vector) -> bool {
    b.iter().all(|(k, v)| a.get(k).is_some_and(|x| x >= v))
}

fn merge(a: &Vector, b: &Vector) -> Vector {
    let mut out = a.clone();
    for (k, v) in b {
        let e = out.entry(*k).or_insert(*v);
        *e = (*e).max(*v);
    }
    out
}

/// How a version another place has stands to this place's.
#[derive(Debug, PartialEq)]
enum Order {
    Same,
    /// Theirs replaces this place's; apart when this place's was made
    /// without having seen it.
    Theirs {
        apart: bool,
    },
    /// This place's follows from theirs, or wins over it.
    Mine,
}

fn order(mine: Option<&FileVersion>, theirs: &FileVersion) -> Order {
    let Some(m) = mine else {
        return Order::Theirs { apart: false };
    };
    if m.vv == theirs.vv {
        Order::Same
    } else if covers(&theirs.vv, &m.vv) {
        Order::Theirs { apart: false }
    } else if covers(&m.vv, &theirs.vv) {
        Order::Mine
    } else if (theirs.time(), theirs.origin) > (m.time(), m.origin) {
        Order::Theirs { apart: true }
    } else {
        Order::Mine
    }
}

/// A file's version by its path in the copy, as one place lists them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Listed {
    pub path: String,
    #[serde(flatten)]
    pub version: FileVersion,
}

/// Every version a place has of a person's linked files, by copy: every
/// copy it can hold is listed, those it has no files in empty.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct FileVersions {
    pub copies: BTreeMap<String, Vec<Listed>>,
}

/// The files a place wants sent, by copy and path, and of each it has an
/// older copy of, what it has (`Base`), for the place sending to send
/// what it lacks; and of each it holds the start of from a send that
/// stopped, how much (`Held`), for the rest alone to come. A place of an
/// older build lists neither, and is sent every file whole.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct FileWant {
    pub want: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bases: Vec<Base>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub held: Vec<Held>,
}

/// The start of a file a place holds, of the version with the digest
/// given: its first `have` bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Held {
    pub copy: String,
    pub path: String,
    pub digest: String,
    pub have: u64,
}

impl FileWant {
    pub fn is_empty(&self) -> bool {
        self.want.is_empty()
    }
}

/// One file's version and a piece of its content, from offset, the whole
/// being the version's size; without content, the version alone, for a
/// place that has its content already, for a deleted file, or for the
/// place to ask for what it lacks of a large file. With a delta, the
/// pieces are the bytes it sends, which with the receiving place's older
/// copy make the file; sent only to a place that listed that copy. The
/// delta is in the first piece alone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilePiece {
    pub copy: String,
    pub path: String,
    pub version: FileVersion,
    #[serde(default)]
    pub offset: u64,
    #[serde(default)]
    pub content: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<Delta>,
}

impl FilePiece {
    /// How many bytes its pieces carry together.
    pub fn total(&self) -> u64 {
        self.delta.as_ref().map_or(self.version.size, |d| d.sent)
    }
}

/// Files at least this large go first as their version alone, and then as
/// what the other place lacks of them.
const DELTA_MIN: u64 = 128 << 10;

/// What of a file to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Carry {
    /// Its version alone.
    Version,
    /// Its content: a file smaller than DELTA_MIN whole, a larger one's
    /// version alone, which the other place answers with what it has.
    Content,
    /// Its content, whole.
    Whole,
    /// What a place that has this of it lacks.
    Delta(Arc<Base>),
    /// Its content from an offset on, to a place holding what comes
    /// before of the version with this digest; whole if it has another.
    After { digest: String, from: u64 },
}

impl Carry {
    fn rank(&self) -> u8 {
        match self {
            Carry::Version => 0,
            Carry::Content => 1,
            Carry::Delta(_) => 2,
            Carry::Whole | Carry::After { .. } => 3,
        }
    }

    /// What to send of a file asked for again before it went: the more of
    /// the two, the later of two alike.
    pub fn and(&self, later: Carry) -> Carry {
        if later.rank() >= self.rank() {
            later
        } else {
            self.clone()
        }
    }

    fn content(deleted: bool) -> Carry {
        if deleted {
            Carry::Version
        } else {
            Carry::Content
        }
    }
}

/// A file to send, by copy and path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub copy: String,
    pub path: String,
    pub send: Carry,
}

/// What this place keeps of a file: its version, how the file was when
/// last looked at, so an unchanged file is not read again, and when this
/// place last made a version of it (milliseconds), so a file written in
/// bursts is not sent with every write.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Kept {
    #[serde(flatten)]
    v: FileVersion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seen: Option<Seen>,
    #[serde(default, skip_serializing_if = "is_zero")]
    made: i64,
}

fn is_zero(n: &i64) -> bool {
    *n == 0
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
struct Seen {
    size: u64,
    /// Nanoseconds.
    mtime: i64,
    mode: u32,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    files: BTreeMap<String, Kept>,
}

/// A version of a file replaced by another place's change made apart
/// from it, kept for a while.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Replaced {
    pub id: Uuid,
    pub path: String,
    pub size: u64,
    pub mode: u32,
    /// When the change kept was made, in milliseconds.
    pub time: i64,
    pub replaced_at: DateTime<Utc>,
    /// The place whose change replaced it.
    pub by: Uuid,
    /// Replaced by merging two packs: the other's file of its path was
    /// changed later.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub merged: bool,
}

/// The files a place is linking: copies changed and waiting to be looked
/// at, and a lock for each person, held while their files are read,
/// compared or written.
#[derive(Default)]
pub struct Files {
    /// Files coming in, until each is whole.
    partials: Mutex<HashMap<PartKey, Partial>>,
    dirty: Mutex<HashSet<Uuid>>,
    /// Copies with a file changed too soon after its last version to be
    /// sent, and when to look at them again (milliseconds).
    later: Mutex<HashMap<Uuid, i64>>,
    wake: Notify,
    locks: Mutex<HashMap<Uuid, Arc<tokio::sync::Mutex<()>>>>,
}

impl Files {
    /// Has the copies looked at for changes, soon.
    pub fn mark(&self, copies: impl IntoIterator<Item = Uuid>) {
        let mut d = self.dirty.lock().unwrap();
        let before = d.len();
        d.extend(copies);
        if d.len() > before {
            self.wake.notify_one();
        }
    }

    /// Has a copy looked at again at the time given, or sooner if it was
    /// to be already.
    fn later(&self, copy: Uuid, at: Option<i64>) {
        let Some(at) = at else { return };
        let mut l = self.later.lock().unwrap();
        let e = l.entry(copy).or_insert(at);
        if at <= *e {
            *e = at;
            self.wake.notify_one();
        }
    }

    async fn lock(&self, user: Uuid) -> tokio::sync::OwnedMutexGuard<()> {
        let m = self.locks.lock().unwrap().entry(user).or_default().clone();
        m.lock_owned().await
    }
}

/// A file coming in: the person here, the place it comes from, its copy
/// and its path.
type PartKey = (Uuid, Uuid, String, String);

/// A file coming in, written to `.links/incoming/` as its pieces arrive.
struct Partial {
    /// Its first piece: its version, and how it is made.
    first: FilePiece,
    rel: String,
    file: std::fs::File,
    got: u64,
    /// The SHA-256 of what came.
    hash: ring::digest::Context,
    touched: Instant,
}

/// A file that came whole: its first piece, and where in the files root
/// what its pieces carried is. Its content matches its digest, but for a
/// delta, which is what makes it (`rebuild`).
pub struct Came {
    pub piece: FilePiece,
    pub rel: String,
}

/// A new file's place in `.links/incoming/`.
fn incoming() -> String {
    format!("{INCOMING}/{}", Uuid::new_v4())
}

impl Files {
    /// Writes a piece of a file a place sends; the file, once its last
    /// piece has come. A first piece starts the file afresh; a piece that
    /// does not follow the last one taken lets the file go.
    pub fn receive(
        &self,
        root: &Root,
        user: Uuid,
        from: Uuid,
        p: FilePiece,
        bytes: &[u8],
    ) -> Result<Option<Came>, String> {
        let key = (user, from, p.copy.clone(), p.path.clone());
        let found = {
            let mut parts = self.partials.lock().unwrap();
            parts.retain(|_, q| {
                let keep = q.touched.elapsed() < PARTIAL_KEPT;
                if !keep {
                    let _ = root.remove(&q.rel);
                }
                keep
            });
            parts.remove(&key)
        };
        let mut part = match found {
            Some(q) if p.offset > 0 && q.first.version == p.version && q.got == p.offset => q,
            other => {
                if let Some(q) = other {
                    let _ = root.remove(&q.rel);
                }
                if p.offset > 0 {
                    return Ok(None);
                }
                if once_swept() {
                    sweep(root);
                }
                let rel = incoming();
                let file = root.create(&rel).map_err(io)?;
                Partial {
                    first: p,
                    rel,
                    file,
                    got: 0,
                    hash: ring::digest::Context::new(&ring::digest::SHA256),
                    touched: Instant::now(),
                }
            }
        };
        let total = part.first.total();
        if part.got + bytes.len() as u64 > total {
            let _ = root.remove(&part.rel);
            return Err(format!("{} is longer than it says", part.first.path));
        }
        std::os::unix::fs::FileExt::write_all_at(&part.file, bytes, part.got).map_err(io)?;
        part.hash.update(bytes);
        part.got += bytes.len() as u64;
        part.touched = Instant::now();
        if part.got < total {
            self.partials.lock().unwrap().insert(key, part);
            return Ok(None);
        }
        part.file.sync_all().map_err(io)?;
        if part.first.delta.is_none()
            && hex(part.hash.finish().as_ref()) != part.first.version.digest
        {
            let _ = root.remove(&part.rel);
            return Err(format!(
                "{}'s content does not match its digest",
                part.first.path
            ));
        }
        Ok(Some(Came {
            piece: part.first,
            rel: part.rel,
        }))
    }

    /// How much this place holds of a file coming whole from a place, of
    /// the version given, so far.
    fn held(&self, user: Uuid, from: Uuid, copy: &str, path: &str, v: &FileVersion) -> Option<u64> {
        let parts = self.partials.lock().unwrap();
        let q = parts.get(&(user, from, copy.to_string(), path.to_string()))?;
        (q.first.delta.is_none() && q.first.version == *v && q.got > 0).then_some(q.got)
    }
}

/// Whether this is the first file to come in since the server started,
/// when what earlier runs left is swept.
fn once_swept() -> bool {
    static SWEPT: OnceLock<()> = OnceLock::new();
    SWEPT.set(()).is_ok()
}

/// Removes the files coming in that no piece has reached for a while:
/// what an earlier run, or another server sharing the files root, left.
fn sweep(root: &Root) {
    let Ok(files) = root.files(INCOMING) else {
        return;
    };
    let cutoff = Utc::now() - chrono::Duration::from_std(PARTIAL_KEPT).unwrap_or_default();
    for (rel, info) in files {
        if info.modified < cutoff {
            let _ = root.remove(&format!("{INCOMING}/{rel}"));
        }
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect()
}

/// The SHA-256 of the regular file at rel, and its size, read a window
/// at a time.
fn file_digest(root: &Root, rel: &str) -> std::io::Result<(String, u64)> {
    let f = root.open_read(rel)?;
    let len = f.metadata()?.len();
    Ok((crate::chunks::sha256(&f, len)?, len))
}

fn staging() -> String {
    let mut name = [0u8; 6];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut name);
    format!("{}link-{}", crate::packs::STAGING, hex(&name))
}

/// Whether a path another place names is one a copy can hold: beneath the
/// copy, and not a file written beside another to replace it.
fn valid(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && path
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != "..")
        && !path
            .rsplit('/')
            .next()
            .unwrap_or(path)
            .starts_with(crate::packs::STAGING)
}

fn io(e: std::io::Error) -> String {
    format!("the files root: {e}")
}

fn load<T: Default + serde::de::DeserializeOwned>(root: &Root, rel: &str) -> Result<T, String> {
    match root.read(rel) {
        Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("{rel}: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(io(e)),
    }
}

fn save<T: Serialize>(root: &Root, rel: &str, v: &T) -> Result<(), String> {
    let b = serde_json::to_vec(v).map_err(|e| e.to_string())?;
    root.replace(rel, &b, 0o600, &staging()).map_err(io)
}

fn state_path(copy: Uuid) -> String {
    format!("{META}/{copy}/files.json")
}

fn history_path(copy: Uuid) -> String {
    format!("{META}/{copy}/history.json")
}

fn kept_path(copy: Uuid, id: Uuid) -> String {
    format!("{META}/{copy}/history/{id}")
}

fn file_path(copy: Uuid, path: &str) -> String {
    format!("{copy}/{path}")
}

/// Removes what is kept beside a copy, once it is gone.
pub fn forget(root: &Root, copy: Uuid) -> std::io::Result<()> {
    root.remove_all(&format!("{META}/{copy}"))
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// How long this place waits between versions it makes of a file, in
/// milliseconds, by its path in the copy; with none, every change found
/// is a version at once.
type Wait<'a> = Option<&'a dyn Fn(&str) -> i64>;

/// Each file's wait as its pack's paths say.
fn waits(paths: &[crate::packs::Path]) -> impl Fn(&str) -> i64 + '_ {
    move |rel| i64::from(crate::packs::send_every(paths, rel)) * 1000
}

/// What a scan found: the paths changed, and when to look again for the
/// changes it left waiting, if any.
#[derive(Default)]
struct Scanned {
    changed: Vec<String>,
    due: Option<i64>,
}

/// Looks for the changes made here to a copy's files since it was last
/// looked at, each a new version from this place, but a file's change
/// made within its wait of its last version: that waits, and is read when
/// the wait ends, so a file written in bursts is sent at most once a wait,
/// as it is at the end of it.
fn scan(
    root: &Root,
    place: Uuid,
    copy: Uuid,
    st: &mut State,
    wait: Wait,
) -> Result<Scanned, String> {
    let files = root.files(&copy.to_string()).map_err(io)?;
    let mut present = HashSet::new();
    let mut changed = vec![];
    let mut due: Option<i64> = None;
    let now = now_ms();
    for (rel, info) in files {
        if !valid(&rel) {
            continue;
        }
        present.insert(rel.clone());
        if info.size > MAX_FILE {
            continue;
        }
        let seen = Seen {
            size: info.size,
            mtime: info.modified.timestamp_nanos_opt().unwrap_or_default(),
            mode: info.mode & 0o777,
        };
        let known = st.files.get(&rel).cloned();
        if known
            .as_ref()
            .is_some_and(|k| !k.v.deleted && k.seen == Some(seen))
        {
            continue;
        }
        if let (Some(wait), Some(k)) = (wait, known.as_ref().filter(|k| !k.v.deleted)) {
            let ms = wait(&rel);
            if ms > 0 && now < k.made + ms {
                due = Some(due.map_or(k.made + ms, |d| d.min(k.made + ms)));
                continue;
            }
        }
        let (digest, size) = match file_digest(root, &file_path(copy, &rel)) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                present.remove(&rel);
                continue;
            }
            Err(e) => return Err(io(e)),
        };
        if let Some(k) = st.files.get_mut(&rel)
            && !k.v.deleted
            && k.v.digest == digest
            && k.v.mode == seen.mode
        {
            k.seen = Some(seen);
            continue;
        }
        let mut vv = known.map(|k| k.v.vv).unwrap_or_default();
        let before = vv.get(&place).copied().unwrap_or(0);
        vv.insert(place, info.modified.timestamp_millis().max(before + 1));
        st.files.insert(
            rel.clone(),
            Kept {
                v: FileVersion {
                    vv,
                    origin: place,
                    digest,
                    mode: seen.mode,
                    size,
                    deleted: false,
                },
                seen: Some(seen),
                made: now,
            },
        );
        changed.push(rel);
    }
    for (rel, k) in st.files.iter_mut() {
        if k.v.deleted || present.contains(rel) {
            continue;
        }
        let before = k.v.vv.get(&place).copied().unwrap_or(0);
        k.v.vv.insert(place, now.max(before + 1));
        k.v.origin = place;
        k.v.digest = String::new();
        k.v.size = 0;
        k.v.deleted = true;
        k.seen = None;
        k.made = now;
        changed.push(rel.clone());
    }
    let cutoff = now - DELETED_DAYS * 86_400_000;
    st.files
        .retain(|_, k| !(k.v.deleted && k.v.time() < cutoff));
    Ok(Scanned { changed, due })
}

/// Keeps a version of a file that another place's change replaced.
fn keep(root: &Root, copy: Uuid, path: &str, k: &FileVersion, by: Uuid) -> Result<(), String> {
    keep_file(
        root,
        copy,
        path,
        &file_path(copy, path),
        k.mode,
        k.time(),
        by,
        false,
    )
}

/// Keeps a copy of the file at src in a copy's history as a version of
/// its file at path that another replaced; none when src is gone.
#[allow(clippy::too_many_arguments)]
fn keep_file(
    root: &Root,
    copy: Uuid,
    path: &str,
    src: &str,
    mode: u32,
    time: i64,
    by: Uuid,
    merged: bool,
) -> Result<(), String> {
    let id = Uuid::new_v4();
    match root.copy(src, &kept_path(copy, id), 0o600, &staging()) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io(e)),
    }
    let size = root.lstat(&kept_path(copy, id)).map_err(io)?.size;
    let mut h: Vec<Replaced> = load(root, &history_path(copy))?;
    h.push(Replaced {
        id,
        path: path.to_string(),
        size,
        mode,
        time,
        replaced_at: Utc::now(),
        by,
        merged,
    });
    prune(root, copy, &mut h);
    save(root, &history_path(copy), &h)
}

/// Puts one copy's files into another, as merging two packs does: a file
/// the other lacks is taken as it is; of a file both have that differ,
/// the one changed later is kept, the other going into the copy's
/// history. Which was changed later is as their versions say, which every
/// linked place agrees on, else as the files' modification times. A file
/// taken keeps its version, so a change made later elsewhere, arriving
/// after, still wins. Gives the paths of the copy's files changed.
pub fn merge_copy(root: &Root, place: Uuid, from: Uuid, into: Uuid) -> Result<Vec<String>, String> {
    let sf: State = load(root, &state_path(from))?;
    let mut si: State = load(root, &state_path(into))?;
    let version = |st: &State, rel: &str| {
        st.files
            .get(rel)
            .filter(|k| !k.v.deleted)
            .map(|k| k.v.clone())
    };
    let when = |v: &Option<FileVersion>, info: &crate::root::Info| {
        v.as_ref()
            .map_or((info.modified.timestamp_millis(), place), |v| {
                (v.time(), v.origin)
            })
    };
    let theirs: HashMap<String, crate::root::Info> = root
        .files(&into.to_string())
        .map_err(io)?
        .into_iter()
        .collect();
    let mut changed = vec![];
    for (rel, info) in root.files(&from.to_string()).map_err(io)? {
        if !valid(&rel) {
            continue;
        }
        let src = file_path(from, &rel);
        let digest = match file_digest(root, &src) {
            Ok((d, _)) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(io(e)),
        };
        let mode = info.mode & 0o777;
        let target = file_path(into, &rel);
        let fv = version(&sf, &rel);
        if let Some(there) = theirs.get(&rel) {
            if file_digest(root, &target).map_err(io)?.0 == digest {
                continue;
            }
            let iv = version(&si, &rel);
            let (a, b) = (when(&fv, &info), when(&iv, there));
            if a <= b {
                keep_file(root, into, &rel, &src, mode, a.0, place, true)?;
                continue;
            }
            keep_file(
                root,
                into,
                &rel,
                &target,
                there.mode & 0o777,
                b.0,
                place,
                true,
            )?;
        }
        match root.copy(&src, &target, mode, &staging()) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(io(e)),
        }
        if let Some(v) = fv {
            let seen = root.lstat(&target).ok().map(|i| Seen {
                size: i.size,
                mtime: i.modified.timestamp_nanos_opt().unwrap_or_default(),
                mode: i.mode & 0o777,
            });
            si.files.insert(rel.clone(), Kept { v, seen, made: 0 });
        }
        changed.push(rel);
    }
    save(root, &state_path(into), &si)?;
    Ok(changed)
}

/// Drops what is kept too long, and of each file all but the latest few.
fn prune(root: &Root, copy: Uuid, h: &mut Vec<Replaced>) {
    let cutoff = Utc::now() - chrono::Duration::days(HISTORY_DAYS);
    h.sort_by_key(|r| std::cmp::Reverse(r.replaced_at));
    let mut per: HashMap<String, usize> = HashMap::new();
    h.retain(|r| {
        let n = per.entry(r.path.clone()).or_default();
        *n += 1;
        let keep = *n <= HISTORY_KEPT && r.replaced_at > cutoff;
        if !keep {
            let _ = root.remove(&kept_path(copy, r.id));
        }
        keep
    });
}

/// A copy linked between places.
#[derive(Debug, Clone)]
pub struct Linked {
    /// The name every place knows it by.
    pub key: String,
    pub pack: Uuid,
    /// This place's copy; none when it is a person's own copy not yet
    /// made.
    pub copy: Option<Uuid>,
    /// The linked places its pack is for; none is every one.
    pub places: Vec<Uuid>,
}

impl Linked {
    /// Whether its files go to the place given.
    pub fn for_place(&self, place: Uuid) -> bool {
        self.places.is_empty() || self.places.contains(&place)
    }
}

/// The copies of a person's linked between places.
pub async fn linked(tx: &mut Tx, user: Uuid) -> StoreResult<Vec<Linked>> {
    let packs = tx.packs().await?;
    let copies = tx.copies().await?;
    let mut out = vec![];
    let mut profile = false;
    for p in packs.iter().filter(|p| !p.away) {
        let base = if p.builtin && !profile {
            profile = true;
            "profile".to_string()
        } else if !p.builtin && p.owner_id == Some(user) && p.team_id.is_none() {
            format!("pack:{}", p.id)
        } else {
            continue;
        };
        out.push(Linked {
            key: base,
            pack: p.id,
            places: p.places.clone(),
            copy: copies
                .iter()
                .find(|c| c.pack_id == p.id && c.personal && c.owner_id == Some(user))
                .map(|c| c.id),
        });
        for c in &copies {
            if c.pack_id == p.id && !c.personal && c.owner_id == Some(user) && c.team_id.is_none() {
                out.push(Linked {
                    key: format!("copy:{}", c.id),
                    pack: p.id,
                    places: p.places.clone(),
                    copy: Some(c.id),
                });
            }
        }
    }
    Ok(out)
}

/// This place's copy for a key, made if it is a person's own copy not yet
/// made; none when this place cannot hold it.
async fn local(app: &App, user: Uuid, key: &str) -> StoreResult<Option<Uuid>> {
    let mut tx = app.store.begin().await?;
    let Some(l) = linked(&mut tx, user)
        .await?
        .into_iter()
        .find(|l| l.key == key)
    else {
        return Ok(None);
    };
    if let Some(c) = l.copy {
        return Ok(Some(c));
    }
    let c = crate::packs::personal_copy(&mut tx, l.pack, user).await?;
    tx.commit().await?;
    Ok(Some(c))
}

fn store_err(e: StoreError) -> String {
    e.to_string()
}

/// Looks at the copies marked changed, and sends what changed in each to
/// the places its owner links, as soon as it is marked.
pub async fn run(app: Arc<App>, place: Uuid) {
    loop {
        let marked: Vec<Uuid> = {
            let woken = app.link_files.wake.notified();
            tokio::pin!(woken);
            woken.as_mut().enable();
            let now = now_ms();
            let next = {
                let mut later = app.link_files.later.lock().unwrap();
                let due: Vec<Uuid> = later
                    .iter()
                    .filter(|(_, at)| **at <= now)
                    .map(|(c, _)| *c)
                    .collect();
                for c in &due {
                    later.remove(c);
                }
                app.link_files.dirty.lock().unwrap().extend(due);
                later.values().min().copied()
            };
            let d: Vec<Uuid> = app.link_files.dirty.lock().unwrap().drain().collect();
            if d.is_empty() {
                match next {
                    Some(at) => {
                        let ms = u64::try_from(at - now).unwrap_or(0).max(1);
                        let _ =
                            tokio::time::timeout(std::time::Duration::from_millis(ms), woken).await;
                    }
                    None => woken.await,
                }
                continue;
            }
            d
        };
        if let Err(e) = look(&app, place, &marked).await {
            eprintln!("hangar-server: looking for changes to linked files: {e}");
        }
    }
}

/// Scans the copies given that are linked, and has what changed sent.
async fn look(app: &App, place: Uuid, copies: &[Uuid]) -> Result<(), String> {
    let mut whose: HashMap<Uuid, Vec<(String, Uuid, Uuid)>> = HashMap::new();
    let paths = {
        let mut tx = app.store.read().await.map_err(store_err)?;
        let users: HashSet<Uuid> = tx
            .linked_users()
            .await
            .map_err(store_err)?
            .into_iter()
            .collect();
        let rows = tx.copies().await.map_err(store_err)?;
        for id in copies {
            let Some(user) = rows.iter().find(|c| c.id == *id).and_then(|c| c.owner_id) else {
                continue;
            };
            if !users.contains(&user) {
                continue;
            }
            if let Some(l) = linked(&mut tx, user)
                .await
                .map_err(store_err)?
                .into_iter()
                .find(|l| l.copy == Some(*id))
            {
                whose.entry(user).or_default().push((l.key, *id, l.pack));
            }
        }
        crate::packs::all_paths(&mut tx).await.map_err(store_err)?
    };
    for (user, copies) in whose {
        let _held = app.link_files.lock(user).await;
        let mut jobs = vec![];
        for (key, copy, pack) in copies {
            let ps = paths.get(&pack).map(Vec::as_slice).unwrap_or_default();
            let changed = tokio::task::block_in_place(|| {
                let mut st: State = load(&app.root, &state_path(copy))?;
                let found = scan(&app.root, place, copy, &mut st, Some(&waits(ps)))?;
                if !found.changed.is_empty() {
                    save(&app.root, &state_path(copy), &st)?;
                }
                app.link_files.later(copy, found.due);
                Ok::<_, String>(found.changed)
            })?;
            jobs.extend(changed.into_iter().map(|path| Job {
                copy: key.clone(),
                path,
                send: Carry::Content,
            }));
        }
        if !jobs.is_empty() {
            app.link_conns.files(user, None, jobs);
        }
    }
    Ok(())
}

/// Every version this place has of a person's linked files that are for
/// the place to, but of files larger than it takes (max), each copy
/// scanned first; changes the scan found go to every connection but the
/// one asking.
pub async fn versions(
    app: &App,
    place: Uuid,
    user: Uuid,
    to: Uuid,
    max: u64,
) -> Result<(FileVersions, Vec<Job>), String> {
    let (all, paths) = {
        let mut tx = app.store.read().await.map_err(store_err)?;
        let all = linked(&mut tx, user).await.map_err(store_err)?;
        (
            all,
            crate::packs::all_paths(&mut tx).await.map_err(store_err)?,
        )
    };
    let _held = app.link_files.lock(user).await;
    let mut out = FileVersions::default();
    let mut changed = vec![];
    for l in all {
        if !l.for_place(to) {
            continue;
        }
        let Some(copy) = l.copy else {
            out.copies.insert(l.key, vec![]);
            continue;
        };
        let ps = paths.get(&l.pack).map(Vec::as_slice).unwrap_or_default();
        let listed = tokio::task::block_in_place(|| {
            let mut st: State = load(&app.root, &state_path(copy))?;
            let found = scan(&app.root, place, copy, &mut st, Some(&waits(ps)))?;
            if !found.changed.is_empty() {
                save(&app.root, &state_path(copy), &st)?;
            }
            app.link_files.later(copy, found.due);
            changed.extend(found.changed.into_iter().map(|path| Job {
                copy: l.key.clone(),
                path,
                send: Carry::Content,
            }));
            Ok::<_, String>(
                st.files
                    .into_iter()
                    .filter(|(_, k)| k.v.size <= max)
                    .map(|(path, k)| Listed { path, version: k.v })
                    .collect(),
            )
        })?;
        out.copies.insert(l.key, listed);
    }
    Ok((out, changed))
}

/// What comparing another place's versions with this place's found: the
/// files to ask it for, and to send it, and changes made here found on
/// the way, for the person's other places.
#[derive(Debug, Default)]
pub struct Compared {
    pub want: FileWant,
    pub send: Vec<Job>,
    pub found: Vec<Job>,
}

/// Compares another place's (from's) versions with this place's: theirs
/// newer are wanted, this place's newer are to be sent, and the same
/// content under two versions becomes one version, counting both.
pub async fn compare(
    app: &App,
    place: Uuid,
    user: Uuid,
    from: Uuid,
    theirs: &FileVersions,
) -> Result<Compared, String> {
    let (all, paths) = {
        let mut tx = app.store.read().await.map_err(store_err)?;
        let all = linked(&mut tx, user).await.map_err(store_err)?;
        (
            all,
            crate::packs::all_paths(&mut tx).await.map_err(store_err)?,
        )
    };
    let _held = app.link_files.lock(user).await;
    let mut out = Compared::default();
    for l in all {
        let Some(listed) = theirs.copies.get(&l.key) else {
            continue;
        };
        let ps = paths.get(&l.pack).map(Vec::as_slice).unwrap_or_default();
        let mut st = match l.copy {
            Some(copy) => tokio::task::block_in_place(|| {
                let mut st: State = load(&app.root, &state_path(copy))?;
                let found = scan(&app.root, place, copy, &mut st, Some(&waits(ps)))?;
                if !found.changed.is_empty() {
                    save(&app.root, &state_path(copy), &st)?;
                }
                app.link_files.later(copy, found.due);
                out.found.extend(found.changed.into_iter().map(|path| Job {
                    copy: l.key.clone(),
                    path,
                    send: Carry::Content,
                }));
                Ok::<_, String>(st)
            })?,
            None => State::default(),
        };
        let mut merged = false;
        let mut seen = HashSet::new();
        for t in listed {
            if !valid(&t.path) {
                continue;
            }
            seen.insert(t.path.clone());
            let mine = st.files.get(&t.path).map(|k| &k.v);
            let o = order(mine, &t.version);
            if o == Order::Same {
                continue;
            }
            if let Some(m) = mine
                && m.same_content(&t.version)
            {
                let winner = if o == Order::Mine {
                    m.origin
                } else {
                    t.version.origin
                };
                let vv = merge(&m.vv, &t.version.vv);
                if vv != t.version.vv {
                    out.send.push(Job {
                        copy: l.key.clone(),
                        path: t.path.clone(),
                        send: Carry::Version,
                    });
                }
                let k = st.files.get_mut(&t.path).expect("the file's version");
                k.v.vv = vv;
                k.v.origin = winner;
                merged = true;
                continue;
            }
            match o {
                Order::Theirs { .. } => {
                    if !(t.version.deleted && mine.is_none_or(|m| m.deleted)) {
                        out.want.want.push((l.key.clone(), t.path.clone()));
                        if let Some(have) =
                            app.link_files.held(user, from, &l.key, &t.path, &t.version)
                        {
                            out.want.held.push(Held {
                                copy: l.key.clone(),
                                path: t.path.clone(),
                                digest: t.version.digest.clone(),
                                have,
                            });
                        } else if let Some(copy) = l.copy
                            && let Some(b) = tokio::task::block_in_place(|| {
                                base(&app.root, copy, &l.key, &t.path, mine, &t.version)
                            })
                        {
                            out.want.bases.push(b);
                        }
                    }
                }
                Order::Mine => out.send.push(Job {
                    copy: l.key.clone(),
                    path: t.path.clone(),
                    send: Carry::content(mine.is_some_and(|m| m.deleted)),
                }),
                Order::Same => {}
            }
        }
        for (path, k) in &st.files {
            if !seen.contains(path) && !k.v.deleted {
                out.send.push(Job {
                    copy: l.key.clone(),
                    path: path.clone(),
                    send: Carry::Content,
                });
            }
        }
        if merged && let Some(copy) = l.copy {
            tokio::task::block_in_place(|| save(&app.root, &state_path(copy), &st))?;
        }
    }
    Ok(out)
}

/// What this place has of a file whose newer version another place has,
/// for that place to send what it lacks; none when the file is too small
/// to be worth it, or this place has none.
fn base(
    root: &Root,
    copy: Uuid,
    key: &str,
    path: &str,
    mine: Option<&FileVersion>,
    theirs: &FileVersion,
) -> Option<Base> {
    if theirs.deleted || theirs.size < DELTA_MIN || mine.is_none_or(|m| m.deleted || m.size == 0) {
        return None;
    }
    let f = root.open_read(&file_path(copy, path)).ok()?;
    let len = f.metadata().ok()?.len();
    Base::of(key, path, &f, len).ok()
}

/// What a file's pieces carry, read as each goes: ranges of an open
/// file, in order, from an offset into them on.
pub struct Body {
    file: Option<std::fs::File>,
    ranges: Vec<(u64, u64)>,
    pub from: u64,
}

impl Body {
    fn none() -> Body {
        Body {
            file: None,
            ranges: vec![],
            from: 0,
        }
    }

    /// The n bytes at offset into what it carries.
    pub fn read(&self, offset: u64, n: usize) -> std::io::Result<Vec<u8>> {
        let mut out = vec![0u8; n];
        let Some(file) = &self.file else {
            return Ok(vec![]);
        };
        let mut filled = 0usize;
        let mut at = 0u64;
        for (start, len) in &self.ranges {
            let end = at + len;
            let cur = offset + filled as u64;
            if filled < n && cur >= at && cur < end {
                let within = cur - at;
                let take = usize::try_from((len - within).min((n - filled) as u64)).unwrap_or(0);
                file.read_at(&mut out[filled..filled + take], start + within)?;
                filled += take;
            }
            at = end;
        }
        out.truncate(filled);
        Ok(out)
    }
}

/// A file to send: its version and what its pieces carry, as this place
/// holds it: the content whole, what a place that has a base lacks, the
/// rest of what a place holds the start of, or nothing for its version
/// alone. None when it holds no such file, the file is larger than the
/// place to takes (max), or the file changed since its version was made
/// (it is looked at again, and sent once it has a version). The file is
/// checked and planned outside the person's lock, read from the one it
/// opened under it.
pub async fn read(
    app: &App,
    user: Uuid,
    job: &Job,
    to: Uuid,
    max: u64,
) -> Result<Option<(FilePiece, Body)>, String> {
    let for_them = {
        let mut tx = app.store.read().await.map_err(store_err)?;
        linked(&mut tx, user)
            .await
            .map_err(store_err)?
            .iter()
            .any(|l| l.key == job.copy && l.for_place(to))
    };
    if !for_them {
        return Ok(None);
    }
    let Some(copy) = local(app, user, &job.copy).await.map_err(store_err)? else {
        return Ok(None);
    };
    let found = {
        let _held = app.link_files.lock(user).await;
        tokio::task::block_in_place(|| {
            let st: State = load(&app.root, &state_path(copy))?;
            let Some(k) = st.files.get(&job.path) else {
                return Ok(None);
            };
            if k.v.size > max {
                return Ok(None);
            }
            let piece = FilePiece {
                copy: job.copy.clone(),
                path: job.path.clone(),
                version: k.v.clone(),
                offset: 0,
                content: false,
                delta: None,
            };
            let content = match job.send {
                Carry::Version => false,
                Carry::Content => k.v.size < DELTA_MIN,
                Carry::Whole | Carry::Delta(_) | Carry::After { .. } => true,
            };
            if !content || k.v.deleted {
                return Ok(Some((piece, None)));
            }
            match app.root.open_read(&file_path(copy, &job.path)) {
                Ok(f) => Ok(Some((piece, Some(f)))),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    app.link_files.mark([copy]);
                    Ok(None)
                }
                Err(e) => Err(io(e)),
            }
        })?
    };
    let Some((mut piece, file)) = found else {
        return Ok(None);
    };
    let Some(file) = file else {
        return Ok(Some((piece, Body::none())));
    };
    tokio::task::block_in_place(|| {
        let len = file.metadata().map_err(io)?.len();
        if len != piece.version.size
            || crate::chunks::sha256(&file, len).map_err(io)? != piece.version.digest
        {
            app.link_files.mark([copy]);
            return Ok(None);
        }
        piece.content = true;
        if let Carry::Delta(base) = &job.send
            && base.digest != piece.version.digest
            && let Some(delta) = crate::chunks::plan(base, &file, len).map_err(io)?
        {
            let ranges = delta.sent_ranges();
            piece.delta = Some(delta);
            return Ok(Some((
                piece,
                Body {
                    file: Some(file),
                    ranges,
                    from: 0,
                },
            )));
        }
        let from = match &job.send {
            Carry::After { digest, from } if *digest == piece.version.digest && *from <= len => {
                *from
            }
            _ => 0,
        };
        Ok(Some((
            piece,
            Body {
                file: Some(file),
                ranges: vec![(0, len)],
                from,
            },
        )))
    })
}

/// The file a delta another place sent makes from this place's copy of it
/// and the bytes that came, written to `.links/incoming/`, which the bytes
/// that came leave; none when this place's copy is not the one the delta
/// is from, which is then asked for whole. A file that came whole is as
/// it came.
pub async fn rebuild(app: &App, user: Uuid, came: &Came) -> Result<Option<String>, String> {
    let Some(delta) = &came.piece.delta else {
        return Ok(Some(came.rel.clone()));
    };
    let piece = &came.piece;
    let local = if valid(&piece.path) {
        local(app, user, &piece.copy).await.map_err(store_err)?
    } else {
        None
    };
    tokio::task::block_in_place(|| {
        let made = (|| {
            let Some(copy) = local else {
                return Ok(None);
            };
            let base = match app.root.open_read(&file_path(copy, &piece.path)) {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(io(e)),
            };
            let base_len = base.metadata().map_err(io)?.len();
            let sent = app.root.open_read(&came.rel).map_err(io)?;
            let sent_len = sent.metadata().map_err(io)?.len();
            let rel = incoming();
            let out = app.root.create(&rel).map_err(io)?;
            let mut w = std::io::BufWriter::new(&out);
            let digest = crate::chunks::rebuild(
                delta,
                &base,
                base_len,
                &sent,
                sent_len,
                piece.version.size,
                &mut w,
            )
            .and_then(|d| w.flush().map(|()| d))
            .map_err(io)?;
            drop(w);
            if digest.as_deref() == Some(piece.version.digest.as_str()) {
                Ok(Some(rel))
            } else {
                let _ = app.root.remove(&rel);
                Ok(None)
            }
        })();
        let _ = app.root.remove(&came.rel);
        made
    })
}

/// What taking a file another place sent came to.
#[derive(Debug, Default)]
pub struct Taken {
    /// To send back to the place that sent it: this place's newer
    /// version, or the one it made of both.
    pub reply: Vec<Job>,
    /// To send to the person's other places: what this place took.
    pub relay: Vec<Job>,
    /// To ask it for: a version sent without the content this place
    /// lacks.
    pub want: FileWant,
    /// Changes made here, found on the way, for every place.
    pub found: Vec<Job>,
}

/// Takes a file another place (from) sent, whole: renamed into the copy,
/// in place of this place's at once, when its version is newer; this
/// place's goes into the copy's history when the two were made apart.
/// content is where in the files root its content is, matching its
/// digest; none for a version sent alone. Content not taken is left
/// where it is, for the caller to remove.
pub async fn take(
    app: &App,
    place: Uuid,
    user: Uuid,
    from: Uuid,
    piece: &FilePiece,
    content: Option<&str>,
) -> Result<Taken, String> {
    let mut out = Taken::default();
    if !valid(&piece.path) || piece.version.size > MAX_FILE {
        return Ok(out);
    }
    let Some(copy) = local(app, user, &piece.copy).await.map_err(store_err)? else {
        return Ok(out);
    };
    let _held = app.link_files.lock(user).await;
    let root = &app.root;
    let job = |send| Job {
        copy: piece.copy.clone(),
        path: piece.path.clone(),
        send,
    };
    tokio::task::block_in_place(|| {
        // Every change made here is found first, however soon after the
        // last, so none is written over unkept.
        let mut st: State = load(root, &state_path(copy))?;
        out.found = scan(root, place, copy, &mut st, None)?
            .changed
            .into_iter()
            .map(|path| Job {
                copy: piece.copy.clone(),
                path,
                send: Carry::Content,
            })
            .collect();
        let theirs = &piece.version;
        let mine = st.files.get(&piece.path).map(|k| k.v.clone());
        let o = order(mine.as_ref(), theirs);
        if o == Order::Same {
        } else if let Some(m) = mine.as_ref().filter(|m| m.same_content(theirs)) {
            let vv = merge(&m.vv, &theirs.vv);
            if vv != theirs.vv {
                out.reply.push(job(Carry::Version));
            }
            if vv != m.vv {
                out.relay.push(job(Carry::Version));
            }
            let k = st.files.get_mut(&piece.path).expect("the file's version");
            k.v.origin = if o == Order::Mine {
                m.origin
            } else {
                theirs.origin
            };
            k.v.vv = vv;
        } else if o == Order::Mine {
            out.reply.push(job(Carry::content(
                mine.as_ref().is_some_and(|m| m.deleted),
            )));
        } else if !theirs.deleted && content.is_none() {
            out.want.want.push((piece.copy.clone(), piece.path.clone()));
            match app
                .link_files
                .held(user, from, &piece.copy, &piece.path, theirs)
            {
                Some(have) => out.want.held.push(Held {
                    copy: piece.copy.clone(),
                    path: piece.path.clone(),
                    digest: theirs.digest.clone(),
                    have,
                }),
                None => out.want.bases.extend(base(
                    root,
                    copy,
                    &piece.copy,
                    &piece.path,
                    mine.as_ref(),
                    theirs,
                )),
            }
        } else if theirs.deleted && mine.as_ref().is_none_or(|m| m.deleted) {
            let vv = merge(&mine.map(|m| m.vv).unwrap_or_default(), &theirs.vv);
            st.files.insert(
                piece.path.clone(),
                Kept {
                    v: FileVersion {
                        vv,
                        ..theirs.clone()
                    },
                    seen: None,
                    made: 0,
                },
            );
        } else {
            if o == (Order::Theirs { apart: true })
                && let Some(m) = mine.as_ref().filter(|m| !m.deleted)
            {
                keep(root, copy, &piece.path, m, theirs.origin)?;
            }
            let target = file_path(copy, &piece.path);
            let seen = if theirs.deleted {
                root.remove(&target).map_err(io)?;
                None
            } else {
                let Some(content) = content else {
                    return Err(format!("{} came without its content", piece.path));
                };
                root.install(content, &target, theirs.mode).map_err(io)?;
                root.lstat(&target).ok().map(|i| Seen {
                    size: i.size,
                    mtime: i.modified.timestamp_nanos_opt().unwrap_or_default(),
                    mode: i.mode & 0o777,
                })
            };
            let vv = merge(&mine.map(|m| m.vv).unwrap_or_default(), &theirs.vv);
            if vv != theirs.vv {
                out.reply.push(job(Carry::Version));
            }
            st.files.insert(
                piece.path.clone(),
                Kept {
                    v: FileVersion {
                        vv,
                        ..theirs.clone()
                    },
                    seen,
                    made: 0,
                },
            );
            out.relay.push(job(Carry::content(theirs.deleted)));
        }
        save(root, &state_path(copy), &st)
    })?;
    Ok(out)
}

/// A copy p may change the files of, and the person whose files they
/// are, for its history.
async fn history_copy(app: &App, p: &Principal, id: Uuid, write: bool) -> fail::Result<Uuid> {
    let c = crate::packs::copy(&app.store, p, id)
        .await?
        .ok_or_else(|| Fail::NotFound("not found".into()))?;
    if write && !c.can_write {
        return fail::forbidden("only the copy's owner, or its team's members, change its files");
    }
    Ok(c.row.owner_id.unwrap_or(p.user_id))
}

/// The versions of a copy's files that other places' changes replaced,
/// the latest first.
pub async fn history(app: &App, p: &Principal, copy: Uuid) -> fail::Result<Vec<Replaced>> {
    let owner = history_copy(app, p, copy, false).await?;
    let _held = app.link_files.lock(owner).await;
    tokio::task::block_in_place(|| {
        let mut h: Vec<Replaced> = load(&app.root, &history_path(copy)).map_err(StoreError)?;
        let n = h.len();
        prune(&app.root, copy, &mut h);
        if h.len() != n {
            save(&app.root, &history_path(copy), &h).map_err(StoreError)?;
        }
        Ok(h)
    })
}

/// Puts a replaced version back in its copy, as a change made here, which
/// goes to every place linked; what it replaces is kept in its stead.
pub async fn restore(
    app: &App,
    place: Uuid,
    p: &Principal,
    ip: &str,
    copy: Uuid,
    entry: Uuid,
) -> fail::Result<()> {
    let owner = history_copy(app, p, copy, true).await?;
    let held = app.link_files.lock(owner).await;
    let path = tokio::task::block_in_place(|| {
        let root = &app.root;
        let fail = |e: String| Fail::Store(StoreError(e));
        let mut h: Vec<Replaced> = load(root, &history_path(copy)).map_err(fail)?;
        let Some(at) = h.iter().position(|r| r.id == entry) else {
            return Err(Fail::NotFound("not found".into()));
        };
        let r = h.remove(at);
        save(root, &history_path(copy), &h).map_err(fail)?;
        // What is there is kept as the restored version was.
        let mut st: State = load(root, &state_path(copy)).map_err(fail)?;
        scan(root, place, copy, &mut st, None).map_err(fail)?;
        save(root, &state_path(copy), &st).map_err(fail)?;
        if let Some(k) = st.files.get(&r.path).filter(|k| !k.v.deleted) {
            keep(root, copy, &r.path, &k.v, place).map_err(fail)?;
        }
        root.install(&kept_path(copy, r.id), &file_path(copy, &r.path), r.mode)
            .map_err(|e| fail(io(e)))?;
        Ok(r.path)
    })?;
    drop(held);
    app.link_files.mark([copy]);
    crate::packs::record_restore(&app.store, p, ip, copy, &path).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(pairs: &[(u128, i64)], origin: u128, digest: &str) -> FileVersion {
        FileVersion {
            vv: pairs
                .iter()
                .map(|(p, t)| (Uuid::from_u128(*p), *t))
                .collect(),
            origin: Uuid::from_u128(origin),
            digest: digest.into(),
            mode: 0o644,
            size: 1,
            deleted: false,
        }
    }

    #[test]
    fn versions_order_by_what_they_have_seen_then_by_time() {
        let a = v(&[(1, 10)], 1, "a");
        assert_eq!(order(None, &a), Order::Theirs { apart: false });
        assert_eq!(order(Some(&a), &a), Order::Same);
        let after = v(&[(1, 10), (2, 5)], 2, "b");
        assert_eq!(order(Some(&a), &after), Order::Theirs { apart: false });
        assert_eq!(order(Some(&after), &a), Order::Mine);
        let apart = v(&[(2, 20)], 2, "c");
        assert_eq!(order(Some(&a), &apart), Order::Theirs { apart: true });
        assert_eq!(order(Some(&apart), &a), Order::Mine);
        let tie = v(&[(2, 10)], 2, "d");
        assert_eq!(order(Some(&a), &tie), Order::Theirs { apart: true });
        assert_eq!(order(Some(&tie), &a), Order::Mine);
    }

    #[test]
    fn a_merged_vector_covers_both() {
        let (a, b) = (v(&[(1, 10), (2, 3)], 1, ""), v(&[(2, 7)], 2, ""));
        let m = merge(&a.vv, &b.vv);
        assert!(covers(&m, &a.vv) && covers(&m, &b.vv));
        assert_eq!(m[&Uuid::from_u128(2)], 7);
    }

    #[test]
    fn paths_stay_in_the_copy() {
        for ok in ["~/.gitconfig", "etc/hosts", "~/.config/a b"] {
            assert!(valid(ok), "{ok}");
        }
        for bad in ["", "/etc/hosts", "~/../x", "~//x", "./x", "~/.hangarfs-1"] {
            assert!(!valid(bad), "{bad}");
        }
    }
}
