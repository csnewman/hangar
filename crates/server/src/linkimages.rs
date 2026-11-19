//! A linked person's own image repositories, kept the same in every place
//! they link (docs/places.md §4). A repository is a record (`linksync`,
//! kind `image`) named by its path below the person's namespace, which
//! may be named otherwise in each place, holding its visibility and
//! description; each of its tags is a record of its own (`image_tag`,
//! `<repository>:<tag>`) holding the digest of the manifest it names, so
//! tags pushed in two places at once are both kept. What a tag names, its
//! manifest and the blobs and manifests that names, comes after, by
//! digest, from the place the record came from, a piece at a time as
//! files do, and only what this place lacks: a blob the blob store holds
//! already, for any repository, is used as it is.
//!
//! A tag's record taken is this place's version at once, but the tag can
//! be set only once what it names is here. Until then it is pending, kept
//! in `.links/images/<person>.json`, which stands in for the tag when this
//! place's records are read, so it is not taken for a change made here.
//! Each connection looks at the pending tags as it starts, hourly, and as
//! records and pieces come (`reconcile`), asks its place for what they
//! lack, keeps each manifest once what it names is here, and sets each
//! tag once its manifest is.
//!
//! A blob that comes is checked against its digest and kept in the blob
//! store under its key at once; it is a blob of the repository, which the
//! registry's collection could then delete, only once a manifest names it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::Utc;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::http::App;
use crate::registry::{Digest, Hasher, MAX_MANIFEST, parse_manifest, valid_name, valid_tag};
use crate::root::Root;
use crate::store::rows::{ImageRepository, Manifest};
use crate::store::{Error as StoreError, Result as StoreResult, Tx};

/// The record kinds a repository and its tags are.
pub const KIND: &str = "image";
pub const TAG_KIND: &str = "image_tag";

/// Where what comes is written as its pieces arrive.
const INCOMING: &str = ".links/incoming";

/// How long a blob that stopped coming part way is kept for the rest.
const PARTIAL_KEPT: Duration = Duration::from_secs(24 * 3600);

/// A repository as its record holds it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Repo {
    pub visibility: String,
    pub description: String,
}

/// A tag as its record holds it: the manifest it names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tag {
    pub digest: String,
}

/// The tags taken whose manifests are not here yet, by record ID, each
/// with the digest of the manifest it names.
type Pending = BTreeMap<String, String>;

fn tag_id(repo: &str, tag: &str) -> String {
    format!("{repo}:{tag}")
}

/// A tag record's repository and tag: neither has a colon in it.
fn split_tag_id(id: &str) -> Option<(&str, &str)> {
    id.rsplit_once(':')
}

fn pending_path(user: Uuid) -> String {
    format!(".links/images/{user}.json")
}

fn load_pending(root: &Root, user: Uuid) -> Pending {
    root.read(&pending_path(user))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_pending(root: &Root, user: Uuid, p: &Pending) -> StoreResult<()> {
    let rel = pending_path(user);
    let written = if p.is_empty() {
        root.remove(&rel)
    } else {
        let b = serde_json::to_vec(p).map_err(|e| StoreError(e.to_string()))?;
        let mut name = [0u8; 6];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut name);
        let staging: String = name.iter().map(|b| format!("{b:02x}")).collect();
        root.replace(&rel, &b, 0o600, &format!(".images-{staging}"))
    };
    written.map_err(|e| StoreError(format!("the files root: {e}")))
}

/// A person's own repositories, each by its record ID: its path below
/// their namespace, which is their username.
async fn own(tx: &mut Tx, user: Uuid) -> StoreResult<Vec<(String, ImageRepository)>> {
    let Some(u) = tx.user(user).await? else {
        return Ok(vec![]);
    };
    let prefix = format!("{}/", u.username);
    Ok(tx
        .image_repositories()
        .await?
        .into_iter()
        .filter(|r| r.owner_id == Some(user) && r.team_id.is_none())
        .filter_map(|r| Some((r.path.strip_prefix(&prefix)?.to_string(), r)))
        .collect())
}

fn value<T: Serialize>(v: &T) -> StoreResult<Value> {
    serde_json::to_value(v).map_err(|e| StoreError(e.to_string()))
}

/// A person's repositories and their tags as records, by kind and ID: a
/// pending tag as it was taken, the rest as they are here.
pub async fn records(
    tx: &mut Tx,
    root: &Root,
    user: Uuid,
) -> StoreResult<Vec<(&'static str, String, Value)>> {
    let pending = load_pending(root, user);
    let tags = tx.image_tags(None).await?;
    let mut out = vec![];
    for (id, r) in own(tx, user).await? {
        out.push((
            KIND,
            id.clone(),
            value(&Repo {
                visibility: r.visibility.clone(),
                description: r.description.clone(),
            })?,
        ));
        for t in tags.iter().filter(|t| t.repository_id == r.id) {
            let key = tag_id(&id, &t.tag);
            if !pending.contains_key(&key) {
                out.push((
                    TAG_KIND,
                    key,
                    value(&Tag {
                        digest: t.digest.clone(),
                    })?,
                ));
            }
        }
        for (key, digest) in &pending {
            if split_tag_id(key).is_some_and(|(repo, _)| repo == id) {
                out.push((
                    TAG_KIND,
                    key.clone(),
                    value(&Tag {
                        digest: digest.clone(),
                    })?,
                ));
            }
        }
    }
    Ok(out)
}

/// Writes a repository's or tag's record to its table, or deletes it
/// (body none). A tag whose manifest is not here is pending until it is.
/// False when it cannot be written here: its path is another's, or not a
/// repository's, or a tag's repository is not here.
pub async fn apply(
    tx: &mut Tx,
    root: &Root,
    user: Uuid,
    kind: &str,
    id: &str,
    body: Option<&Value>,
) -> StoreResult<bool> {
    let Some(u) = tx.user(user).await? else {
        return Ok(false);
    };
    let (repo_id, tag) = match kind {
        TAG_KIND => match split_tag_id(id) {
            Some((r, t)) if valid_tag(t) => (r, Some(t)),
            _ => return Ok(false),
        },
        _ => (id, None),
    };
    let path = format!("{}/{repo_id}", u.username);
    if !valid_name(&path) {
        return Ok(false);
    }
    let repo = tx
        .image_repositories()
        .await?
        .into_iter()
        .find(|r| r.path == path);
    if repo
        .as_ref()
        .is_some_and(|r| r.owner_id != Some(user) || r.team_id.is_some())
    {
        return Ok(false);
    }
    let mut pending = load_pending(root, user);
    let now = Utc::now();
    if let Some(tag) = tag {
        let Some(r) = repo else {
            // A tag deleted with its repository is gone already.
            return Ok(body.is_none());
        };
        match body {
            None => {
                tx.delete_tag(r.id, tag).await?;
                pending.remove(id);
            }
            Some(v) => {
                let Ok(t) = serde_json::from_value::<Tag>(v.clone()) else {
                    return Ok(false);
                };
                if Digest::parse(&t.digest).is_none() {
                    return Ok(false);
                }
                if tx.manifest(r.id, Some(&t.digest), None).await?.is_some() {
                    tx.set_tag(r.id, tag, &t.digest, now).await?;
                    pending.remove(id);
                } else {
                    pending.insert(id.to_string(), t.digest);
                }
            }
        }
        save_pending(root, user, &pending)?;
        return Ok(true);
    }
    let Some(v) = body else {
        if let Some(r) = repo {
            tx.delete_image_repository(r.id).await?;
        }
        pending.retain(|k, _| split_tag_id(k).is_none_or(|(r, _)| r != id));
        save_pending(root, user, &pending)?;
        return Ok(true);
    };
    let Ok(b) = serde_json::from_value::<Repo>(v.clone()) else {
        return Ok(false);
    };
    if !matches!(b.visibility.as_str(), "private" | "shared") {
        return Ok(false);
    }
    match repo {
        Some(mut r) => {
            r.visibility = b.visibility;
            r.description = b.description;
            r.updated_at = now;
            tx.save_image_repository(&r).await?;
        }
        None => {
            let r = ImageRepository {
                id: Uuid::new_v4(),
                path,
                owner_id: Some(user),
                team_id: None,
                visibility: b.visibility,
                description: b.description,
                created_at: now,
                updated_at: now,
            };
            if !tx.insert_image_repository(&r).await? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// What a place asks of one repository: manifests by digest, and blobs by
/// digest, each with how much of it it holds already.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Want {
    pub repo: String,
    pub manifests: Vec<String>,
    pub blobs: Vec<(String, u64)>,
}

/// The message asking for them.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Wants {
    pub wants: Vec<Want>,
}

/// One manifest's or blob's piece of content, from offset, the whole
/// being size.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImagePiece {
    /// The repository's record ID.
    pub repo: String,
    pub digest: String,
    /// A manifest's type; none for a blob.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<String>,
    pub size: u64,
    pub offset: u64,
}

/// A manifest or blob to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub repo: String,
    pub digest: String,
    pub manifest: bool,
    pub from: u64,
}

/// What a manifest's or blob's pieces carry, read as each goes.
pub enum Source {
    Bytes(Vec<u8>),
    Blob(crate::blob::Body),
}

/// What this place sends of what a place asked for: the manifests and
/// blobs of the person's own repositories here, those it has.
pub async fn jobs(app: &App, user: Uuid, wants: &[Want]) -> StoreResult<Vec<Job>> {
    let mut tx = app.store.read().await?;
    let own = own(&mut tx, user).await?;
    let mut out = vec![];
    for w in wants {
        let Some((_, r)) = own.iter().find(|(id, _)| *id == w.repo) else {
            continue;
        };
        for d in &w.manifests {
            if Digest::parse(d).is_some() && tx.manifest(r.id, Some(d), None).await?.is_some() {
                out.push(Job {
                    repo: w.repo.clone(),
                    digest: d.clone(),
                    manifest: true,
                    from: 0,
                });
            }
        }
        for (d, have) in &w.blobs {
            if Digest::parse(d).is_some() && tx.repository_blob(r.id, d).await?.is_some() {
                out.push(Job {
                    repo: w.repo.clone(),
                    digest: d.clone(),
                    manifest: false,
                    from: *have,
                });
            }
        }
    }
    Ok(out)
}

/// A manifest's or blob's first piece and what its pieces carry, from the
/// job's offset on; none when this place has it no more.
pub async fn read(
    app: &App,
    user: Uuid,
    job: &Job,
) -> Result<Option<(ImagePiece, Source)>, String> {
    let Some(blobs) = &app.blobs else {
        return Ok(None);
    };
    let found = async {
        let mut tx = app.store.read().await?;
        let Some((_, r)) = own(&mut tx, user)
            .await?
            .into_iter()
            .find(|(id, _)| *id == job.repo)
        else {
            return Ok(None);
        };
        if job.manifest {
            Ok(tx
                .manifest(r.id, Some(&job.digest), None)
                .await?
                .map(|m| (Some(m.media_type), m.content.len() as u64, Some(m.content))))
        } else {
            Ok(tx
                .repository_blob(r.id, &job.digest)
                .await?
                .map(|size| (None, size as u64, None)))
        }
    };
    let Some((manifest, size, content)) = found.await.map_err(|e: StoreError| e.to_string())?
    else {
        return Ok(None);
    };
    let from = job.from.min(size);
    let piece = ImagePiece {
        repo: job.repo.clone(),
        digest: job.digest.clone(),
        manifest,
        size,
        offset: from,
    };
    let source = match content {
        Some(c) => Source::Bytes(c),
        None => {
            let Some(d) = Digest::parse(&job.digest) else {
                return Ok(None);
            };
            match blobs
                .get(&d.key(), (from < size).then_some((from, None)))
                .await
                .map_err(|e| e.0)?
            {
                Some(b) => Source::Blob(b),
                None => return Ok(None),
            }
        }
    };
    Ok(Some((piece, source)))
}

/// A manifest or blob coming in: a manifest held in memory, a blob
/// written to `.links/incoming/`.
struct Partial {
    first: ImagePiece,
    data: Vec<u8>,
    spool: Option<(String, std::fs::File)>,
    got: u64,
    hash: Hasher,
    touched: Instant,
}

/// Manifests held, by digest, each with its type.
type Held = HashMap<String, (String, Vec<u8>)>;

/// What a connection has of the person's repositories coming in.
#[derive(Default)]
pub struct Images {
    /// Manifests come in whole, held until what they name is here: by
    /// person and repository, by digest, with their type.
    held: Mutex<HashMap<(Uuid, String), Held>>,
    /// What is coming in, by person, the place it comes from, and digest.
    partials: Mutex<HashMap<(Uuid, Uuid, String), Partial>>,
}

impl Images {
    /// How much this place holds of a blob coming from a place.
    fn have(&self, user: Uuid, from: Uuid, digest: &str) -> u64 {
        self.partials
            .lock()
            .unwrap()
            .get(&(user, from, digest.to_string()))
            .filter(|p| p.first.manifest.is_none())
            .map_or(0, |p| p.got)
    }

    fn held_digests(&self, user: Uuid, repo: &str) -> Held {
        self.held
            .lock()
            .unwrap()
            .get(&(user, repo.to_string()))
            .cloned()
            .unwrap_or_default()
    }
}

/// Takes a piece of a manifest or blob a place (from) sends: true once it
/// has come whole, been checked against its digest and kept, a manifest
/// held for its repository and a blob in the blob store. A first piece
/// starts it afresh; a piece that does not follow the last lets it go.
pub async fn receive(
    app: &App,
    user: Uuid,
    from: Uuid,
    p: ImagePiece,
    bytes: &[u8],
) -> Result<bool, String> {
    let Some(blobs) = &app.blobs else {
        return Ok(false);
    };
    let Some(d) = Digest::parse(&p.digest) else {
        return Err(format!("{} is not a digest", p.digest));
    };
    if p.manifest.is_some() && p.size > MAX_MANIFEST as u64 {
        return Ok(false);
    }
    let images = &app.link_images;
    let key = (user, from, p.digest.clone());
    let done = tokio::task::block_in_place(|| {
        let found = {
            let mut parts = images.partials.lock().unwrap();
            parts.retain(|_, q| {
                let keep = q.touched.elapsed() < PARTIAL_KEPT;
                if !keep && let Some((rel, _)) = &q.spool {
                    let _ = app.root.remove(rel);
                }
                keep
            });
            parts.remove(&key)
        };
        let mut part = match found {
            Some(q)
                if p.offset > 0
                    && q.got == p.offset
                    && q.first.size == p.size
                    && q.first.manifest == p.manifest =>
            {
                q
            }
            other => {
                if let Some((rel, _)) = other.and_then(|q| q.spool) {
                    let _ = app.root.remove(&rel);
                }
                if p.offset > 0 {
                    return Ok(None);
                }
                let spool = if p.manifest.is_none() {
                    let rel = format!("{INCOMING}/{}", Uuid::new_v4());
                    let f = app.root.create(&rel).map_err(|e| e.to_string())?;
                    Some((rel, f))
                } else {
                    None
                };
                Partial {
                    first: p,
                    data: vec![],
                    spool,
                    got: 0,
                    hash: Hasher::new(d.algorithm),
                    touched: Instant::now(),
                }
            }
        };
        if part.got + bytes.len() as u64 > part.first.size {
            if let Some((rel, _)) = &part.spool {
                let _ = app.root.remove(rel);
            }
            return Err(format!("{} is longer than it says", part.first.digest));
        }
        match &part.spool {
            Some((_, f)) => std::os::unix::fs::FileExt::write_all_at(f, bytes, part.got)
                .map_err(|e| e.to_string())?,
            None => part.data.extend_from_slice(bytes),
        }
        part.hash.update(bytes);
        part.got += bytes.len() as u64;
        part.touched = Instant::now();
        if part.got < part.first.size {
            images.partials.lock().unwrap().insert(key, part);
            return Ok(None);
        }
        let got = std::mem::replace(&mut part.hash, Hasher::new(d.algorithm)).finish();
        if got != d {
            if let Some((rel, _)) = &part.spool {
                let _ = app.root.remove(rel);
            }
            return Err(format!("{}'s content does not match it", part.first.digest));
        }
        Ok(Some(part))
    })?;
    let Some(part) = done else {
        return Ok(false);
    };
    match (part.first.manifest.clone(), part.spool) {
        (Some(media_type), _) => {
            images
                .held
                .lock()
                .unwrap()
                .entry((user, part.first.repo.clone()))
                .or_default()
                .insert(part.first.digest.clone(), (media_type, part.data));
        }
        (None, Some((rel, mut file))) => {
            let kept = async {
                if blobs.stat(&d.key()).await.map_err(|e| e.0)?.is_none() {
                    use std::io::Seek;
                    file.seek(std::io::SeekFrom::Start(0))
                        .map_err(|e| e.to_string())?;
                    blobs
                        .put(&d.key(), tokio::fs::File::from_std(file))
                        .await
                        .map_err(|e| e.0)?;
                }
                Ok::<_, String>(())
            }
            .await;
            let _ = tokio::task::block_in_place(|| app.root.remove(&rel));
            kept?;
        }
        (None, None) => {}
    }
    Ok(true)
}

/// Brings the person's pending tags as far as what is here lets: each
/// manifest held whose blobs and manifests are here is kept, and each tag
/// whose manifest is here is set and taken off the pending list. Gives
/// what to ask the place (from) for, but what was asked of it already
/// over this connection (asked).
pub async fn reconcile(
    app: &App,
    user: Uuid,
    from: Uuid,
    asked: &mut HashSet<String>,
) -> Result<Vec<Want>, String> {
    let Some(blobs) = &app.blobs else {
        return Ok(vec![]);
    };
    let fail = |e: StoreError| e.to_string();
    let mut tx = app.store.begin().await.map_err(fail)?;
    tx.lock_link_records(user).await.map_err(fail)?;
    let mut pending = load_pending(&app.root, user);
    let own = own(&mut tx, user).await.map_err(fail)?;
    let held_repos: Vec<String> = app
        .link_images
        .held
        .lock()
        .unwrap()
        .keys()
        .filter(|(u, _)| *u == user)
        .map(|(_, r)| r.clone())
        .collect();
    let mut repos: Vec<String> = pending
        .keys()
        .filter_map(|k| split_tag_id(k).map(|(r, _)| r.to_string()))
        .chain(held_repos)
        .collect();
    repos.sort();
    repos.dedup();
    let mut wants = vec![];
    let now = Utc::now();
    for id in repos {
        let Some((_, r)) = own.iter().find(|(o, _)| *o == id) else {
            // Deleted here since: that change is this place's.
            pending.retain(|k, _| split_tag_id(k).is_none_or(|(rid, _)| rid != id));
            app.link_images.held.lock().unwrap().remove(&(user, id));
            continue;
        };
        let mut want = Want {
            repo: id.clone(),
            ..Want::default()
        };
        // Keep each manifest held once what it names is here, those an
        // index names before the index.
        loop {
            let held = app.link_images.held_digests(user, &id);
            want.blobs.clear();
            want.manifests.clear();
            let mut kept_one = false;
            for (digest, (media_type, content)) in &held {
                let Ok(m) = parse_manifest(content, media_type) else {
                    drop_held(app, user, &id, digest);
                    continue;
                };
                let mut whole = true;
                for b in &m.blobs {
                    if !blob_here(&mut tx, blobs, r.id, b, now).await? {
                        whole = false;
                        want.blobs
                            .push((b.clone(), app.link_images.have(user, from, b)));
                    }
                }
                for x in &m.manifests {
                    if tx
                        .manifest(r.id, Some(x), None)
                        .await
                        .map_err(fail)?
                        .is_none()
                    {
                        whole = false;
                        if !held.contains_key(x) {
                            want.manifests.push(x.clone());
                        }
                    }
                }
                if !whole {
                    continue;
                }
                let refs: Vec<String> = m.blobs.iter().chain(m.manifests.iter()).cloned().collect();
                tx.insert_manifest(
                    &Manifest {
                        repository_id: r.id,
                        digest: digest.clone(),
                        media_type: m.media_type,
                        artifact_type: m.artifact_type,
                        subject: m.subject,
                        annotations: m.annotations,
                        content: content.clone(),
                        created_at: now,
                    },
                    &refs,
                )
                .await
                .map_err(fail)?;
                drop_held(app, user, &id, digest);
                kept_one = true;
            }
            if !kept_one {
                break;
            }
        }
        let held = app.link_images.held_digests(user, &id);
        for (key, digest) in pending.clone() {
            let Some((_, tag)) = split_tag_id(&key).filter(|(rid, _)| *rid == id) else {
                continue;
            };
            if tx
                .manifest(r.id, Some(&digest), None)
                .await
                .map_err(fail)?
                .is_some()
            {
                tx.set_tag(r.id, tag, &digest, now).await.map_err(fail)?;
                pending.remove(&key);
            } else if !held.contains_key(&digest) {
                want.manifests.push(digest);
            }
        }
        want.manifests.sort();
        want.manifests.dedup();
        want.manifests.retain(|d| asked.insert(d.clone()));
        want.blobs.sort();
        want.blobs.dedup_by(|a, b| a.0 == b.0);
        want.blobs.retain(|(d, _)| asked.insert(d.clone()));
        if !want.manifests.is_empty() || !want.blobs.is_empty() {
            wants.push(want);
        }
    }
    save_pending(&app.root, user, &pending).map_err(fail)?;
    tx.commit().await.map_err(fail)?;
    Ok(wants)
}

fn drop_held(app: &App, user: Uuid, repo: &str, digest: &str) {
    if let Some(h) = app
        .link_images
        .held
        .lock()
        .unwrap()
        .get_mut(&(user, repo.to_string()))
    {
        h.remove(digest);
    }
}

/// Whether a repository has the blob digest, made its own if the blob
/// store holds it: for another repository, or come from a linked place.
async fn blob_here(
    tx: &mut Tx,
    blobs: &crate::blob::Blobs,
    repo: Uuid,
    digest: &str,
    now: chrono::DateTime<Utc>,
) -> Result<bool, String> {
    let fail = |e: StoreError| e.to_string();
    if tx
        .repository_blob(repo, digest)
        .await
        .map_err(fail)?
        .is_some()
    {
        return Ok(true);
    }
    tx.lock_blob(digest).await.map_err(fail)?;
    tx.mount_blob(repo, digest, now).await.map_err(fail)?;
    if tx
        .repository_blob(repo, digest)
        .await
        .map_err(fail)?
        .is_some()
    {
        return Ok(true);
    }
    let Some(d) = Digest::parse(digest) else {
        return Ok(false);
    };
    match blobs.stat(&d.key()).await.map_err(|e| e.0)? {
        Some(o) => {
            tx.add_blob(repo, digest, o.size as i64, now)
                .await
                .map_err(fail)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// A manifest's or blob's pieces, read as each is asked for.
pub struct Pieces {
    piece: ImagePiece,
    source: Source,
    size: usize,
    buf: Vec<u8>,
    done: bool,
}

impl Pieces {
    /// The pieces of size bytes, but the last, a source's first piece
    /// and what follows carry.
    pub fn new(piece: ImagePiece, source: Source, size: usize) -> Pieces {
        Pieces {
            piece,
            source,
            size,
            buf: vec![],
            done: false,
        }
    }

    /// The next piece, and what it carries; none once the last has gone.
    /// A source with nothing to carry is one empty piece.
    pub async fn next(&mut self) -> Result<Option<(ImagePiece, Vec<u8>)>, String> {
        if self.done {
            return Ok(None);
        }
        let bytes = match &mut self.source {
            Source::Bytes(data) => {
                let start = usize::try_from(self.piece.offset)
                    .unwrap_or(data.len())
                    .min(data.len());
                let end = (start + self.size).min(data.len());
                self.done = end >= data.len();
                data[start..end].to_vec()
            }
            Source::Blob(body) => {
                while self.buf.len() < self.size {
                    match body.stream.next().await {
                        Some(chunk) => self
                            .buf
                            .extend_from_slice(&chunk.map_err(|e| e.to_string())?),
                        None => {
                            self.done = true;
                            break;
                        }
                    }
                }
                let rest = self.buf.split_off(self.buf.len().min(self.size));
                let bytes = std::mem::replace(&mut self.buf, rest);
                if bytes.is_empty() && self.done && self.piece.offset > 0 {
                    return Ok(None);
                }
                bytes
            }
        };
        let piece = self.piece.clone();
        self.piece.offset += bytes.len() as u64;
        if self.piece.offset >= self.piece.size {
            self.done = true;
        }
        Ok(Some((piece, bytes)))
    }
}
