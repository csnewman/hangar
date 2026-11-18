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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use uuid::Uuid;

use crate::fail::{self, Fail};
use crate::http::App;
use crate::root::Root;
use crate::store::{Error as StoreError, Result as StoreResult, Tx};
use crate::users::Principal;

/// The largest file linked: a larger one stays in its place.
pub const MAX_FILE: u64 = 64 << 20;

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

/// The files a place wants sent, by copy and path.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct FileWant {
    pub want: Vec<(String, String)>,
}

/// One file's version and a piece of its content, from offset, the whole
/// being the version's size; without content, the version alone, for a
/// place that has its content already or for a deleted file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilePiece {
    pub copy: String,
    pub path: String,
    pub version: FileVersion,
    #[serde(default)]
    pub offset: u64,
    #[serde(default)]
    pub content: bool,
}

/// A file to send, by copy and path; with its content, or its version
/// alone.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Job {
    pub copy: String,
    pub path: String,
    pub content: bool,
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
}

/// The files a place is linking: copies changed and waiting to be looked
/// at, and a lock for each person, held while their files are read,
/// compared or written.
#[derive(Default)]
pub struct Files {
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

fn hex(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256(data: &[u8]) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, data).as_ref())
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
fn scan(root: &Root, place: Uuid, copy: Uuid, st: &mut State, wait: Wait) -> Result<Scanned, String> {
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
        let data = match root.read(&file_path(copy, &rel)) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                present.remove(&rel);
                continue;
            }
            Err(e) => return Err(io(e)),
        };
        let digest = sha256(&data);
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
                    size: data.len() as u64,
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
    let data = match root.read(&file_path(copy, path)) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io(e)),
    };
    let id = Uuid::new_v4();
    root.replace(&kept_path(copy, id), &data, 0o600, &staging())
        .map_err(io)?;
    let mut h: Vec<Replaced> = load(root, &history_path(copy))?;
    h.push(Replaced {
        id,
        path: path.to_string(),
        size: data.len() as u64,
        mode: k.mode,
        time: k.time(),
        replaced_at: Utc::now(),
        by,
    });
    prune(root, copy, &mut h);
    save(root, &history_path(copy), &h)
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
}

/// The copies of a person's linked between places.
pub async fn linked(tx: &mut Tx, user: Uuid) -> StoreResult<Vec<Linked>> {
    let packs = tx.packs().await?;
    let copies = tx.copies().await?;
    let mut out = vec![];
    let mut profile = false;
    for p in &packs {
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
                        let _ = tokio::time::timeout(std::time::Duration::from_millis(ms), woken)
                            .await;
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
        let mut tx = app.store.begin().await.map_err(store_err)?;
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
                content: true,
            }));
        }
        if !jobs.is_empty() {
            app.link_conns.files(user, None, jobs);
        }
    }
    Ok(())
}

/// Every version this place has of a person's linked files, each copy
/// scanned first; changes the scan found go to every connection but the
/// one asking.
pub async fn versions(
    app: &App,
    place: Uuid,
    user: Uuid,
) -> Result<(FileVersions, Vec<Job>), String> {
    let (all, paths) = {
        let mut tx = app.store.begin().await.map_err(store_err)?;
        let all = linked(&mut tx, user).await.map_err(store_err)?;
        (all, crate::packs::all_paths(&mut tx).await.map_err(store_err)?)
    };
    let _held = app.link_files.lock(user).await;
    let mut out = FileVersions::default();
    let mut changed = vec![];
    for l in all {
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
                content: true,
            }));
            Ok::<_, String>(
                st.files
                    .into_iter()
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
    pub want: Vec<(String, String)>,
    pub send: Vec<Job>,
    pub found: Vec<Job>,
}

/// Compares another place's versions with this place's: theirs newer are
/// wanted, this place's newer are to be sent, and the same content under
/// two versions becomes one version, counting both.
pub async fn compare(
    app: &App,
    place: Uuid,
    user: Uuid,
    theirs: &FileVersions,
) -> Result<Compared, String> {
    let (all, paths) = {
        let mut tx = app.store.begin().await.map_err(store_err)?;
        let all = linked(&mut tx, user).await.map_err(store_err)?;
        (all, crate::packs::all_paths(&mut tx).await.map_err(store_err)?)
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
                    content: true,
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
                        content: false,
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
                        out.want.push((l.key.clone(), t.path.clone()));
                    }
                }
                Order::Mine => out.send.push(Job {
                    copy: l.key.clone(),
                    path: t.path.clone(),
                    content: !mine.is_some_and(|m| m.deleted),
                }),
                Order::Same => {}
            }
        }
        for (path, k) in &st.files {
            if !seen.contains(path) && !k.v.deleted {
                out.send.push(Job {
                    copy: l.key.clone(),
                    path: path.clone(),
                    content: true,
                });
            }
        }
        if merged && let Some(copy) = l.copy {
            tokio::task::block_in_place(|| save(&app.root, &state_path(copy), &st))?;
        }
    }
    Ok(out)
}

/// A file to send: its version and content as this place holds them, or
/// none when it holds no such file, or the file changed since its version
/// was made (it is looked at again, and sent once it has a version).
pub async fn read(
    app: &App,
    user: Uuid,
    job: &Job,
) -> Result<Option<(FilePiece, Vec<u8>)>, String> {
    let Some(copy) = local(app, user, &job.copy).await.map_err(store_err)? else {
        return Ok(None);
    };
    let _held = app.link_files.lock(user).await;
    tokio::task::block_in_place(|| {
        let st: State = load(&app.root, &state_path(copy))?;
        let Some(k) = st.files.get(&job.path) else {
            return Ok(None);
        };
        let piece = FilePiece {
            copy: job.copy.clone(),
            path: job.path.clone(),
            version: k.v.clone(),
            offset: 0,
            content: job.content && !k.v.deleted,
        };
        if !piece.content {
            return Ok(Some((piece, vec![])));
        }
        let data = match app.root.read(&file_path(copy, &job.path)) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                app.link_files.mark([copy]);
                return Ok(None);
            }
            Err(e) => return Err(io(e)),
        };
        if sha256(&data) != k.v.digest {
            app.link_files.mark([copy]);
            return Ok(None);
        }
        Ok(Some((piece, data)))
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
    pub want: Vec<(String, String)>,
    /// Changes made here, found on the way, for every place.
    pub found: Vec<Job>,
}

/// Takes a file another place sent, whole: written into the copy, in place
/// of this place's at once, when its version is newer; this place's goes
/// into the copy's history when the two were made apart. data is its
/// content, empty for a version sent alone.
pub async fn take(
    app: &App,
    place: Uuid,
    user: Uuid,
    piece: &FilePiece,
    data: &[u8],
) -> Result<Taken, String> {
    let mut out = Taken::default();
    if !valid(&piece.path) || piece.version.size > MAX_FILE {
        return Ok(out);
    }
    let Some(copy) = local(app, user, &piece.copy).await.map_err(store_err)? else {
        return Ok(out);
    };
    if piece.content && sha256(data) != piece.version.digest {
        return Err(format!(
            "{}'s content does not match its digest",
            piece.path
        ));
    }
    let _held = app.link_files.lock(user).await;
    let root = &app.root;
    let job = |content| Job {
        copy: piece.copy.clone(),
        path: piece.path.clone(),
        content,
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
                content: true,
            })
            .collect();
        let theirs = &piece.version;
        let mine = st.files.get(&piece.path).map(|k| k.v.clone());
        let o = order(mine.as_ref(), theirs);
        if o == Order::Same {
        } else if let Some(m) = mine.as_ref().filter(|m| m.same_content(theirs)) {
            let vv = merge(&m.vv, &theirs.vv);
            if vv != theirs.vv {
                out.reply.push(job(false));
            }
            if vv != m.vv {
                out.relay.push(job(false));
            }
            let k = st.files.get_mut(&piece.path).expect("the file's version");
            k.v.origin = if o == Order::Mine {
                m.origin
            } else {
                theirs.origin
            };
            k.v.vv = vv;
        } else if o == Order::Mine {
            out.reply
                .push(job(!mine.as_ref().is_some_and(|m| m.deleted)));
        } else if !theirs.deleted && !piece.content {
            out.want.push((piece.copy.clone(), piece.path.clone()));
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
                root.replace(&target, data, theirs.mode, &staging())
                    .map_err(io)?;
                root.lstat(&target).ok().map(|i| Seen {
                    size: i.size,
                    mtime: i.modified.timestamp_nanos_opt().unwrap_or_default(),
                    mode: i.mode & 0o777,
                })
            };
            let vv = merge(&mine.map(|m| m.vv).unwrap_or_default(), &theirs.vv);
            if vv != theirs.vv {
                out.reply.push(job(false));
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
            out.relay.push(job(!theirs.deleted));
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
        let data = root.read(&kept_path(copy, r.id)).map_err(|e| fail(io(e)))?;
        save(root, &history_path(copy), &h).map_err(fail)?;
        // What is there is kept as the restored version was.
        let mut st: State = load(root, &state_path(copy)).map_err(fail)?;
        scan(root, place, copy, &mut st, None).map_err(fail)?;
        save(root, &state_path(copy), &st).map_err(fail)?;
        if let Some(k) = st.files.get(&r.path).filter(|k| !k.v.deleted) {
            keep(root, copy, &r.path, &k.v, place).map_err(fail)?;
        }
        root.replace(&file_path(copy, &r.path), &data, r.mode, &staging())
            .map_err(|e| fail(io(e)))?;
        let _ = root.remove(&kept_path(copy, r.id));
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
