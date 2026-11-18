//! A linked person's records, kept the same in every place they link
//! (docs/places.md §4.3): their theme, sign-in keys, SSH keys, own packs
//! and the shared copies they own of those and the profile. Each place holds them in its own tables as ever, and keeps
//! beside them (`link_records`) a version of each: the time of its last
//! change on the places' shared clock and the place that made it, which
//! together say which of two versions is newer, and a digest of its
//! content, by which a change made here is noticed.
//!
//! Two places exchanging a person's records each first notice their own
//! changes (`scan`); the one asking sends every version it has, the one
//! asked answers with what it wants and the records it has newer, and the
//! one asking sends what was wanted. The newer version of a record wins; a
//! deleted one stays deleted. Records pass on from place to place, so a
//! chain of links is enough.

use std::collections::HashMap;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::root::Root;
use crate::seal::Sealer;
use crate::store::rows::{Copy, LinkRecord, LoginKey, Pack, PackPath, SshKey};
use crate::store::{Result, Tx};

/// What a place needs to read and write a person's records.
pub struct Here<'a> {
    pub place: Uuid,
    pub sealer: Option<&'a Sealer>,
    pub root: &'a Root,
}

/// A record's version, as one place tells another.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Version {
    pub kind: String,
    pub id: String,
    pub hlc: i64,
    pub origin: Uuid,
    pub digest: String,
    pub deleted: bool,
}

/// A record whole: its version, and its content unless it is deleted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    #[serde(flatten)]
    pub version: Version,
    #[serde(default)]
    pub body: Option<Value>,
}

/// What the place asking sends: every version it has.
#[derive(Debug, Serialize, Deserialize)]
pub struct Versions {
    pub versions: Vec<Version>,
}

/// What the place asked answers: the records it wants, by kind and ID,
/// and those it has newer.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Exchange {
    pub want: Vec<(String, String)>,
    pub records: Vec<Record>,
}

/// Records sent to a place.
#[derive(Debug, Serialize, Deserialize)]
pub struct Records {
    pub records: Vec<Record>,
}

const THEME: &str = "theme";
const LOGIN_KEY: &str = "login_key";
const SSH_KEY: &str = "ssh_key";
const PACK: &str = "pack";
const COPY: &str = "copy";
const SENDING: &str = "sending";
const DEFAULT: &str = "default";

/// The kinds of record this place holds: SSH keys only with a secret key
/// to seal them.
fn kinds(here: &Here) -> Vec<&'static str> {
    let mut k = vec![THEME, LOGIN_KEY, PACK, COPY, SENDING, DEFAULT];
    if here.sealer.is_some() {
        k.push(SSH_KEY);
    }
    k
}

/// The order records are taken in: a pack before its copies, and a copy
/// before the default that names it.
fn rank(kind: &str) -> u8 {
    match kind {
        PACK => 1,
        COPY => 2,
        DEFAULT => 3,
        _ => 0,
    }
}

/// Whether a pack is one of the person's own, which linked places share.
fn own_pack(p: &Pack, user: Uuid) -> bool {
    p.owner_id == Some(user) && !p.builtin && p.team_id.is_none()
}

/// What a shared copy's pack is called between places: the profile, or
/// the person's own pack by ID; none for any other.
fn copy_pack(packs: &[Pack], c: &Copy, user: Uuid) -> Option<String> {
    let p = packs.iter().find(|p| p.id == c.pack_id)?;
    if p.builtin {
        Some("profile".into())
    } else if own_pack(p, user) {
        Some(p.id.to_string())
    } else {
        None
    }
}

/// Whether version a is newer than b: a later time, or the same time from
/// a place of greater ID; anything is newer than nothing.
pub fn newer(a: (i64, Uuid), b: Option<(i64, Uuid)>) -> bool {
    b.is_none_or(|b| a > b)
}

/// The SHA-256 of a body, its object keys sorted, so two places holding
/// the same record give the same digest.
fn digest(body: &Value) -> String {
    fn canonical(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                let mut out = serde_json::Map::new();
                for k in keys {
                    out.insert(k.clone(), canonical(&m[k]));
                }
                Value::Object(out)
            }
            Value::Array(a) => Value::Array(a.iter().map(canonical).collect()),
            v => v.clone(),
        }
    }
    let bytes = serde_json::to_vec(&canonical(body)).unwrap_or_default();
    ring::digest::digest(&ring::digest::SHA256, &bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A person's records as this place holds them, by kind and ID.
async fn snapshot(
    tx: &mut Tx,
    here: &Here<'_>,
    user: Uuid,
) -> Result<HashMap<(String, String), Value>> {
    let mut out = HashMap::new();
    // The default theme is no record, so a theme chosen in one place is
    // not undone by another place that never had one chosen.
    if let Some(u) = tx.user(user).await?
        && u.theme != "system"
    {
        out.insert((THEME.into(), THEME.into()), json!({ "theme": u.theme }));
    }
    // No limit is no record, as the default theme is none.
    let bandwidth = tx.link_bandwidth(user).await?;
    if bandwidth > 0 {
        out.insert(
            (SENDING.into(), SENDING.into()),
            json!({ "bandwidth": bandwidth }),
        );
    }
    for k in tx.login_keys(user).await? {
        out.insert(
            (LOGIN_KEY.into(), k.fingerprint.clone()),
            json!({ "name": k.name, "public_key": k.public_key }),
        );
    }
    if let Some(sealer) = here.sealer {
        for k in tx.ssh_keys(user).await? {
            let Some(public) = parse_public(&k.public_key) else {
                continue;
            };
            let bound = format!("{user}/ssh/{}", crate::keys::fingerprint(&public));
            let Ok(pem) = sealer.open(&k.private_key, &bound) else {
                continue;
            };
            out.insert(
                (SSH_KEY.into(), k.id.to_string()),
                json!({
                    "name": k.name,
                    "public_key": k.public_key,
                    "private_key": String::from_utf8_lossy(&pem),
                }),
            );
        }
    }
    let paths = tx.pack_paths().await?;
    let packs = tx.packs().await?;
    let copies = tx.copies().await?;
    // The copy the person uses of a pack by default, when it is one that
    // travels: a shared copy of theirs, of the profile or a pack of
    // theirs. Their own copy is the default without one.
    for d in tx.pack_defaults(user).await? {
        let Some(c) = copies.iter().find(|c| c.id == d.copy_id) else {
            continue;
        };
        if c.personal || c.owner_id != Some(user) || c.team_id.is_some() {
            continue;
        }
        if let Some(pack) = copy_pack(&packs, c, user) {
            out.insert((DEFAULT.into(), pack), json!({ "copy": c.id.to_string() }));
        }
    }
    for c in copies {
        if c.personal || c.owner_id != Some(user) || c.team_id.is_some() {
            continue;
        }
        if let Some(pack) = copy_pack(&packs, &c, user) {
            out.insert(
                (COPY.into(), c.id.to_string()),
                json!({ "pack": pack, "name": c.name }),
            );
        }
    }
    for p in packs {
        if !own_pack(&p, user) {
            continue;
        }
        let mut ps: Vec<&PackPath> = paths.iter().filter(|x| x.pack_id == p.id).collect();
        ps.sort_by(|a, b| a.path.cmp(&b.path));
        // A path's send rule is written only where it has one, so a pack
        // without any reads as places of an older build write it.
        let ps: Vec<Value> = ps
            .into_iter()
            .map(|x| match x.send_every {
                Some(n) => json!([x.path, x.sensitive, n]),
                None => json!([x.path, x.sensitive]),
            })
            .collect();
        let mut body = json!({
            "name": p.name,
            "description": p.description,
            "attach": p.attach,
            "squash": p.squash,
            "paths": ps,
        });
        if let Some(o) = p.same_name {
            body["same_name"] = json!(o.to_string());
        }
        if !p.merged.is_empty() {
            let mut m: Vec<String> = p.merged.iter().map(Uuid::to_string).collect();
            m.sort();
            body["merged"] = json!(m);
        }
        out.insert((PACK.into(), p.id.to_string()), body);
    }
    Ok(out)
}

fn parse_public(text: &str) -> Option<russh::keys::ssh_key::PublicKey> {
    text.split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
        .parse()
        .ok()
}

/// The next time on the shared clock: now, unless a time seen is later.
async fn next_hlc(tx: &mut Tx) -> Result<i64> {
    Ok(Utc::now()
        .timestamp_millis()
        .max(tx.max_link_hlc().await? + 1))
}

/// Notices the changes made here to a person's records since their
/// versions were last brought up to date: each is a new version from this
/// place. Gives whether there were any. The caller holds the person's
/// records (`lock_link_records`).
pub async fn scan(tx: &mut Tx, here: &Here<'_>, user: Uuid) -> Result<bool> {
    let kinds = kinds(here);
    let now = snapshot(tx, here, user).await?;
    let known: HashMap<(String, String), LinkRecord> = tx
        .link_records(user)
        .await?
        .into_iter()
        .map(|r| ((r.kind.clone(), r.record_id.clone()), r))
        .collect();
    let mut hlc = next_hlc(tx).await?;
    let mut changed = false;
    let mut keys: Vec<&(String, String)> = now.keys().collect();
    keys.sort();
    for key in keys {
        let d = digest(&now[key]);
        if known.get(key).is_some_and(|r| !r.deleted && r.digest == d) {
            continue;
        }
        tx.put_link_record(&LinkRecord {
            user_id: user,
            kind: key.0.clone(),
            record_id: key.1.clone(),
            hlc,
            origin: here.place,
            digest: d,
            deleted: false,
        })
        .await?;
        hlc += 1;
        changed = true;
    }
    for (key, r) in &known {
        if r.deleted || now.contains_key(key) || !kinds.contains(&key.0.as_str()) {
            continue;
        }
        tx.put_link_record(&LinkRecord {
            hlc,
            origin: here.place,
            digest: String::new(),
            deleted: true,
            ..r.clone()
        })
        .await?;
        hlc += 1;
        changed = true;
    }
    Ok(changed)
}

fn version(r: &LinkRecord) -> Version {
    Version {
        kind: r.kind.clone(),
        id: r.record_id.clone(),
        hlc: r.hlc,
        origin: r.origin,
        digest: r.digest.clone(),
        deleted: r.deleted,
    }
}

/// Every version this place has of a person's records.
pub async fn versions(tx: &mut Tx, user: Uuid) -> Result<Vec<Version>> {
    Ok(tx.link_records(user).await?.iter().map(version).collect())
}

/// The records of the kinds and IDs given, whole, as this place holds
/// them: those it holds no longer go as deleted, at their versions.
pub async fn records(
    tx: &mut Tx,
    here: &Here<'_>,
    user: Uuid,
    which: &[(String, String)],
) -> Result<Vec<Record>> {
    let now = snapshot(tx, here, user).await?;
    let known: HashMap<(String, String), LinkRecord> = tx
        .link_records(user)
        .await?
        .into_iter()
        .map(|r| ((r.kind.clone(), r.record_id.clone()), r))
        .collect();
    Ok(which
        .iter()
        .filter_map(|key| {
            let r = known.get(key)?;
            let body = if r.deleted {
                None
            } else {
                now.get(key).cloned()
            };
            if body.is_none() && !r.deleted {
                return None;
            }
            Some(Record {
                version: version(r),
                body,
            })
        })
        .collect())
}

/// Answers another place's versions: what this place wants of them, and
/// the records it has newer. The caller has scanned.
pub async fn exchange(
    tx: &mut Tx,
    here: &Here<'_>,
    user: Uuid,
    theirs: &[Version],
) -> Result<Exchange> {
    let mine: HashMap<(String, String), LinkRecord> = tx
        .link_records(user)
        .await?
        .into_iter()
        .map(|r| ((r.kind.clone(), r.record_id.clone()), r))
        .collect();
    let theirs_by: HashMap<(String, String), &Version> = theirs
        .iter()
        .map(|v| ((v.kind.clone(), v.id.clone()), v))
        .collect();
    let mut want = vec![];
    for v in theirs {
        let key = (v.kind.clone(), v.id.clone());
        if newer((v.hlc, v.origin), mine.get(&key).map(|r| (r.hlc, r.origin))) {
            want.push(key);
        }
    }
    let mut send: Vec<(String, String)> = mine
        .iter()
        .filter(|(key, r)| {
            newer(
                (r.hlc, r.origin),
                theirs_by.get(*key).map(|v| (v.hlc, v.origin)),
            )
        })
        .map(|(key, _)| key.clone())
        .collect();
    send.sort();
    Ok(Exchange {
        want,
        records: records(tx, here, user, &send).await?,
    })
}

/// Takes records another place sent: each newer than this place's
/// version is written to its table and becomes this place's version.
/// Gives how many were taken. The caller holds the person's records.
pub async fn accept(tx: &mut Tx, here: &Here<'_>, user: Uuid, records: &[Record]) -> Result<usize> {
    let kinds = kinds(here);
    let mine: HashMap<(String, String), LinkRecord> = tx
        .link_records(user)
        .await?
        .into_iter()
        .map(|r| ((r.kind.clone(), r.record_id.clone()), r))
        .collect();
    let mut taken = 0;
    let mut removed_copies = vec![];
    let mut records: Vec<&Record> = records.iter().collect();
    records.sort_by_key(|r| rank(&r.version.kind));
    for r in records {
        let v = &r.version;
        if !kinds.contains(&v.kind.as_str()) {
            continue;
        }
        let key = (v.kind.clone(), v.id.clone());
        if !newer((v.hlc, v.origin), mine.get(&key).map(|m| (m.hlc, m.origin))) {
            continue;
        }
        let body = match (&r.body, v.deleted) {
            (_, true) => None,
            (Some(b), false) => Some(b),
            (None, false) => continue,
        };
        if !apply(tx, here, user, &v.kind, &v.id, body, &mut removed_copies).await? {
            continue;
        }
        tx.put_link_record(&LinkRecord {
            user_id: user,
            kind: v.kind.clone(),
            record_id: v.id.clone(),
            hlc: v.hlc,
            origin: v.origin,
            digest: if v.deleted {
                String::new()
            } else {
                v.digest.clone()
            },
            deleted: v.deleted,
        })
        .await?;
        taken += 1;
    }
    if !removed_copies.is_empty()
        && let Err(e) = crate::packs::remove_copies(here.root, &removed_copies)
    {
        eprintln!("hangar-server: removing the copies of a pack deleted elsewhere: {e:?}");
    }
    Ok(taken)
}

/// Writes one record to its table, or deletes it (body None). False when
/// it cannot be written here as it is.
async fn apply(
    tx: &mut Tx,
    here: &Here<'_>,
    user: Uuid,
    kind: &str,
    id: &str,
    body: Option<&Value>,
    removed_copies: &mut Vec<Uuid>,
) -> Result<bool> {
    let text = |b: &Value, k: &str| b[k].as_str().unwrap_or_default().to_string();
    match kind {
        THEME => {
            let theme = body.map_or("system".to_string(), |b| text(b, "theme"));
            if !matches!(theme.as_str(), "system" | "light" | "dark") {
                return Ok(false);
            }
            tx.set_theme(user, &theme).await?;
            Ok(true)
        }
        DEFAULT => {
            let packs = tx.packs().await?;
            let pack = match id {
                "profile" => packs.iter().find(|p| p.builtin).map(|p| p.id),
                p => Uuid::parse_str(p)
                    .ok()
                    .filter(|id| packs.iter().any(|k| k.id == *id && own_pack(k, user))),
            };
            let Some(pack) = pack else {
                return Ok(false);
            };
            let Some(b) = body else {
                tx.set_pack_default(user, pack, None).await?;
                return Ok(true);
            };
            let Some(copy) = b["copy"].as_str().and_then(|c| Uuid::parse_str(c).ok()) else {
                return Ok(false);
            };
            // Taken once the copy is here: its record comes first.
            let here = tx.copies().await?.into_iter().any(|c| {
                c.id == copy && c.pack_id == pack && c.owner_id == Some(user) && !c.personal
            });
            if !here {
                return Ok(false);
            }
            tx.set_pack_default(user, pack, Some(copy)).await?;
            Ok(true)
        }
        SENDING => {
            let bandwidth = body.map_or(0, |b| b["bandwidth"].as_i64().unwrap_or(0).max(0));
            tx.set_link_bandwidth(user, bandwidth).await?;
            Ok(true)
        }
        LOGIN_KEY => {
            let existing = tx
                .login_keys(user)
                .await?
                .into_iter()
                .find(|k| k.fingerprint == id);
            let Some(b) = body else {
                if let Some(k) = existing {
                    tx.delete_login_key(user, k.id).await?;
                }
                return Ok(true);
            };
            let public_key = text(b, "public_key");
            let Some(public) = parse_public(&public_key) else {
                return Ok(false);
            };
            if crate::keys::fingerprint(&public) != id {
                return Ok(false);
            }
            let name = text(b, "name");
            if let Some(k) = existing {
                if k.name == name && k.public_key == public_key {
                    return Ok(true);
                }
                tx.delete_login_key(user, k.id).await?;
            }
            tx.insert_login_key(&LoginKey {
                id: Uuid::new_v4(),
                user_id: user,
                name,
                public_key,
                fingerprint: id.to_string(),
                created_at: Utc::now(),
            })
            .await?;
            Ok(true)
        }
        SSH_KEY => {
            let (Some(sealer), Ok(key_id)) = (here.sealer, Uuid::parse_str(id)) else {
                return Ok(false);
            };
            tx.delete_ssh_key(user, key_id).await?;
            let Some(b) = body else { return Ok(true) };
            let public_key = text(b, "public_key");
            let pem = text(b, "private_key");
            let (Some(public), Ok(private)) = (
                parse_public(&public_key),
                russh::keys::decode_secret_key(&pem, None),
            ) else {
                return Ok(false);
            };
            if private.public_key().key_data() != public.key_data() {
                return Ok(false);
            }
            let bound = format!("{user}/ssh/{}", crate::keys::fingerprint(&public));
            tx.insert_ssh_key(&SshKey {
                id: key_id,
                user_id: user,
                name: text(b, "name"),
                public_key,
                private_key: sealer.seal(pem.as_bytes(), &bound),
                created_at: Utc::now(),
            })
            .await?;
            tx.signal(crate::store::signals::Signal::Shared);
            Ok(true)
        }
        PACK => {
            let Ok(pack_id) = Uuid::parse_str(id) else {
                return Ok(false);
            };
            let packs = tx.packs().await?;
            let existing = packs.iter().find(|p| p.id == pack_id).cloned();
            if existing
                .as_ref()
                .is_some_and(|p| p.owner_id != Some(user) || p.builtin || p.team_id.is_some())
            {
                return Ok(false);
            }
            let Some(b) = body else {
                if existing.is_some() {
                    removed_copies.extend(
                        tx.copies()
                            .await?
                            .iter()
                            .filter(|c| c.pack_id == pack_id)
                            .map(|c| c.id),
                    );
                    tx.lock_pack(pack_id).await?;
                    tx.delete_pack(pack_id).await?;
                }
                return Ok(true);
            };
            let attach = match b["attach"].as_str() {
                Some(a @ ("owner" | "listed")) => a.to_string(),
                _ => "listed".into(),
            };
            let squash = match b["squash"].as_str() {
                Some(s @ ("all_squash" | "root_squash" | "no_root_squash")) => s.to_string(),
                _ => "all_squash".into(),
            };
            let now = Utc::now();
            let mut pack = Pack {
                id: pack_id,
                name: text(b, "name"),
                description: text(b, "description"),
                owner_id: Some(user),
                team_id: None,
                builtin: false,
                attach,
                squash,
                created_at: existing.as_ref().map_or(now, |p| p.created_at),
                updated_at: now,
                same_name: b["same_name"]
                    .as_str()
                    .and_then(|s| Uuid::parse_str(s).ok()),
                merged: b["merged"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| Uuid::parse_str(x.as_str()?).ok())
                            .collect()
                    })
                    .unwrap_or_default(),
            };
            if pack.name.trim().is_empty() {
                return Ok(false);
            }
            // Another of the person's packs may have the name. The one of
            // the greater ID is renamed, the same way in every place, so
            // the places agree on both, and marked for the person to merge
            // or keep apart. Looked for first: Postgres ends a transaction
            // whose write breaks the name's uniqueness.
            let lower = pack.name.to_lowercase();
            let clash = packs
                .iter()
                .find(|p| {
                    p.id != pack_id
                        && p.owner_id == Some(user)
                        && p.team_id.is_none()
                        && p.name.to_lowercase() == lower
                })
                .cloned();
            if let Some(other) = clash {
                let taken: Vec<String> = packs
                    .iter()
                    .filter(|p| p.owner_id == Some(user) && p.team_id.is_none())
                    .map(|p| p.name.to_lowercase())
                    .collect();
                let free = (2..)
                    .map(|n| format!("{} ({n})", pack.name))
                    .find(|n| !taken.contains(&n.to_lowercase()))
                    .unwrap_or_default();
                if other.id > pack_id {
                    let renamed = Pack {
                        name: free,
                        updated_at: now,
                        same_name: Some(pack_id),
                        ..other
                    };
                    if !tx.put_pack(&renamed, false).await? {
                        return Ok(false);
                    }
                } else {
                    pack.name = free;
                    pack.same_name = Some(other.id);
                }
            }
            if !tx.put_pack(&pack, existing.is_none()).await? {
                return Ok(false);
            }
            let paths: Vec<PackPath> = b["paths"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|p| {
                            Some(PackPath {
                                pack_id,
                                path: p[0].as_str()?.to_string(),
                                sensitive: p[1].as_bool().unwrap_or(false),
                                send_every: p[2]
                                    .as_i64()
                                    .filter(|n| {
                                        (0..=i64::from(crate::packs::MAX_SEND_EVERY)).contains(n)
                                    })
                                    .map(|n| n as i32),
                            })
                        })
                        .filter(|p| !p.path.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            tx.set_pack_paths(pack_id, &paths).await?;
            Ok(true)
        }
        COPY => {
            let Ok(copy_id) = Uuid::parse_str(id) else {
                return Ok(false);
            };
            let existing = tx.copies().await?.into_iter().find(|c| c.id == copy_id);
            if existing
                .as_ref()
                .is_some_and(|c| c.personal || c.owner_id != Some(user) || c.team_id.is_some())
            {
                return Ok(false);
            }
            let Some(b) = body else {
                if existing.is_some() {
                    tx.delete_copy(copy_id).await?;
                    removed_copies.push(copy_id);
                }
                return Ok(true);
            };
            let packs = tx.packs().await?;
            let pack = match b["pack"].as_str() {
                Some("profile") => packs.iter().find(|p| p.builtin).map(|p| p.id),
                Some(p) => Uuid::parse_str(p)
                    .ok()
                    .filter(|id| packs.iter().any(|k| k.id == *id && own_pack(k, user))),
                None => None,
            };
            let name = text(b, "name");
            let Some(pack) = pack.filter(|_| !name.trim().is_empty()) else {
                return Ok(false);
            };
            let now = Utc::now();
            match existing {
                Some(c) if c.pack_id != pack => Ok(false),
                Some(c) => {
                    if c.name != name {
                        tx.rename_copy(copy_id, &name, now).await?;
                    }
                    Ok(true)
                }
                None => {
                    tx.insert_copy(&Copy {
                        id: copy_id,
                        pack_id: pack,
                        owner_id: Some(user),
                        team_id: None,
                        personal: false,
                        name,
                        created_at: now,
                        updated_at: now,
                    })
                    .await
                }
            }
        }
        _ => Ok(false),
    }
}
