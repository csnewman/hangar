//! The files environments share (internal/packs).
//!
//! A pack is a named list of paths and holds no files. A copy is one set
//! of a pack's files, a directory of the files root named by its ID, which
//! every worker serves its environments over NFS and each environment's
//! agent routes the pack's paths to. A copy is a person's own, made when
//! they first need it, or shared: a team's, or one a person keeps apart
//! from their own.
//!
//! An environment has the packs its template lists, then those that
//! attach themselves -- its owner's, their teams', everyone's -- unless
//! its template keeps them out. Of each it uses one copy: the one chosen
//! on the environment, else the template's pin, else the one it was first
//! given, else its owner's default, else their own. The profile is a pack
//! like any other: built in, attached to everyone, changed by admins.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};
use hangar_api::{EnvironmentFiles, Spec, Squash};
use uuid::Uuid;

use crate::audit::{self, Event};
use crate::fail::{self, Fail};
use crate::root::Root;
use crate::store::rows::{Copy, Pack, PackPath, TeamMember};
use crate::store::signals::Signal;
use crate::store::{Error as StoreError, Result as StoreResult, Store, Tx};
use crate::users::Principal;

/// Starts a path in the home directory.
pub const HOME: &str = "~/";

/// Starts a path left out of a shared directory.
pub const EXCLUDE: &str = "!";

const MAX_PATHS: usize = 64;
const MAX_FILE_SIZE: usize = 4 << 20;
const MAX_COPY_SIZE: u64 = 64 << 20;

/// Names files written beside one they replace: this server's, and
/// hangarfs's for a save renamed over a routed name. They are not a copy's
/// files.
const STAGING: &str = ".hangarfs-";

/// One of a pack's paths.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Path {
    /// ~/ in the home directory or absolute; a directory's ends in a
    /// slash, and one left out starts with EXCLUDE.
    pub path: String,
    /// Keeps every file under it, in every copy, from environments given
    /// no sensitive files.
    pub sensitive: bool,
}

/// The built-in profile's paths as Hangar ships them (migrations 0009 and
/// 0011), which an admin may return it to.
pub fn profile_defaults() -> Vec<Path> {
    const SENSITIVE: &[&str] = &[
        "~/.claude/.credentials.json",
        "~/.git-credentials",
        "~/.netrc",
        "~/.config/gh/hosts.yml",
        "~/.config/glab-cli/config.yml",
        "~/.docker/config.json",
        "~/.npmrc",
        "~/.pypirc",
        "~/.aws/credentials",
        "~/.kube/config",
        "~/.config/gcloud/application_default_credentials.json",
    ];
    const PATHS: &[&str] = &[
        "~/.claude/",
        "~/.claude/.credentials.json",
        "!~/.claude/projects/",
        "!~/.claude/sessions/",
        "!~/.claude/session-env/",
        "!~/.claude/file-history/",
        "!~/.claude/shell-snapshots/",
        "!~/.claude/todos/",
        "!~/.claude/tasks/",
        "!~/.claude/jobs/",
        "!~/.claude/ide/",
        "!~/.claude/daemon/",
        "!~/.claude/daemon.log",
        "!~/.claude/statsig/",
        "!~/.claude/telemetry/",
        "!~/.claude/cache/",
        "!~/.claude/paste-cache/",
        "!~/.claude/downloads/",
        "!~/.claude/backups/",
        "!~/.claude/chrome/",
        "!~/.claude/state/",
        "!~/.claude/debug/",
        "!~/.claude/logs/",
        "!~/.claude/history.jsonl",
        "!~/.claude/stats-cache.json",
        "!~/.claude/settings.local.json",
        "!~/.claude/.last-cleanup",
        "!~/.claude/.last-update-result.json",
        "~/.gitconfig",
        "~/.git-credentials",
        "~/.netrc",
        "~/.config/gh/config.yml",
        "~/.config/gh/hosts.yml",
        "~/.config/glab-cli/config.yml",
        "~/.docker/config.json",
        "~/.npmrc",
        "~/.pypirc",
        "~/.aws/config",
        "~/.aws/credentials",
        "~/.kube/config",
        "~/.config/gcloud/configurations/",
        "~/.config/gcloud/active_config",
        "~/.config/gcloud/application_default_credentials.json",
        "~/.vscode-server-oss/data/User/settings.json",
        "~/.vscode-server-oss/data/User/keybindings.json",
        "~/.vscode-server-oss/data/User/snippets/",
        "~/.config/hangar/",
    ];
    PATHS
        .iter()
        .map(|p| Path {
            path: p.to_string(),
            sensitive: SENSITIVE.contains(p),
        })
        .collect()
}

/// A clean path, as Go's path.Clean would leave it.
fn clean(p: &str) -> bool {
    let rooted = p.starts_with('/');
    let parts: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
    let rebuilt = format!("{}{}", if rooted { "/" } else { "" }, parts.join("/"));
    rebuilt == p && !parts.iter().any(|s| *s == "." || *s == "..")
}

/// Whether p names a file of a copy: a clean path in the home directory,
/// ~/ and more, or a clean absolute one.
pub fn valid_key(p: &str) -> bool {
    if p.contains('\0') {
        return false;
    }
    if let Some(rel) = p.strip_prefix(HOME) {
        return !rel.is_empty() && clean(rel) && !rel.starts_with('/');
    }
    p.starts_with('/') && p != "/" && clean(p) && p != "/~" && !p.starts_with("/~/")
}

/// Why a path cannot be one of a pack's, if it cannot.
fn check_path(p: &str) -> Result<(), String> {
    let x = p.strip_prefix(EXCLUDE).unwrap_or(p);
    let bare = x.strip_suffix('/').unwrap_or(x);
    if !valid_key(bare) || x.ends_with("//") {
        return Err(format!(
            "{x:?} is neither a clean path in the home directory (~/…) nor a clean absolute one"
        ));
    }
    for r in ["/proc/", "/sys/", "/dev/", "/run/", "/boot/"] {
        if format!("{bare}/").starts_with(r) {
            return Err(format!("{r} is the system's, and holds no pack's files"));
        }
    }
    Ok(())
}

/// Whether p is inside s. A path shares what is inside it whether or not
/// it ends in a slash.
fn inside(p: &str, s: &str) -> bool {
    p.trim_end_matches('/')
        .starts_with(&format!("{}/", s.trim_end_matches('/')))
}

/// Whether p is s, or inside it.
fn under(p: &str, s: &str) -> bool {
    p == s.trim_end_matches('/') || inside(p, s)
}

/// Whether a file is shared: at one of the paths or under one, and not
/// left out.
pub fn shared(paths: &[String], p: &str) -> bool {
    if !valid_key(p) {
        return false;
    }
    let mut out = false;
    for s in paths {
        if let Some(x) = s.strip_prefix(EXCLUDE) {
            if under(p, x) {
                return false;
            }
            continue;
        }
        if under(p, s) {
            out = true;
        }
    }
    out
}

/// The paths an environment routes to a copy: those no other among them
/// already shares, and the exclusions inside one shared.
pub fn routed(paths: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in paths {
        let (x, exclude) = match p.strip_prefix(EXCLUDE) {
            Some(x) => (x, true),
            None => (p.as_str(), false),
        };
        let covered = paths
            .iter()
            .any(|s| s != p && !s.starts_with(EXCLUDE) && inside(x, s));
        if exclude == covered && !out.contains(p) {
            out.push(p.clone());
        }
    }
    out
}

/// Whether a file is under one of the sensitive paths.
fn sensitive(paths: &[Path], p: &str) -> bool {
    paths
        .iter()
        .any(|s| s.sensitive && !s.path.starts_with(EXCLUDE) && under(p, &s.path))
}

fn paths_of(paths: &[Path]) -> Vec<String> {
    paths.iter().map(|p| p.path.clone()).collect()
}

/// A pack's paths as kept: trimmed, without duplicates, in order; a path
/// left out must be inside a directory the pack shares.
fn check_paths(input: &[Path]) -> fail::Result<Vec<Path>> {
    if input.len() > MAX_PATHS {
        return fail::invalid(format!("a pack names at most {MAX_PATHS} paths"));
    }
    let mut out: Vec<Path> = Vec::new();
    for p in input {
        let mut p = Path {
            path: p.path.trim().to_string(),
            sensitive: p.sensitive,
        };
        check_path(&p.path).or_else(fail::invalid)?;
        if let Some(o) = out.iter_mut().find(|q| q.path == p.path) {
            o.sensitive = o.sensitive || p.sensitive;
            continue;
        }
        if p.path.starts_with(EXCLUDE) {
            p.sensitive = false;
        }
        out.push(p);
    }
    let all = paths_of(&out);
    for p in &all {
        let Some(x) = p.strip_prefix(EXCLUDE) else {
            continue;
        };
        if !all.iter().any(|s| !s.starts_with(EXCLUDE) && inside(x, s)) {
            return fail::invalid(format!("{x} is left out of no path the pack shares"));
        }
    }
    out.sort_by(|a, b| {
        a.path
            .trim_start_matches(EXCLUDE)
            .cmp(b.path.trim_start_matches(EXCLUDE))
    });
    Ok(out)
}

/// Everything the rules read: packs, their paths, copies, who is in which
/// team, and names.
struct World {
    packs: Vec<Pack>,
    paths: HashMap<Uuid, Vec<Path>>,
    copies: Vec<Copy>,
    members: Vec<TeamMember>,
    usernames: HashMap<Uuid, String>,
    slugs: HashMap<Uuid, String>,
}

/// A pack as someone sees it.
#[derive(Debug, Clone)]
pub struct PackView {
    pub row: Pack,
    pub owner: Option<String>,
    pub team: Option<String>,
    pub paths: Vec<Path>,
    /// A built-in pack's paths as Hangar ships them.
    pub defaults: Option<Vec<Path>>,
    pub can_change: bool,
    pub can_delete: bool,
}

/// A copy as someone sees it.
#[derive(Debug, Clone)]
pub struct CopyView {
    pub row: Copy,
    pub owner: Option<String>,
    pub team: Option<String>,
    pub can_write: bool,
    pub can_delete: bool,
}

impl CopyView {
    fn name(&self) -> String {
        if self.row.personal {
            format!("{}'s", self.owner.clone().unwrap_or_default())
        } else {
            self.row.name.clone()
        }
    }
}

/// Who asks: a person, or with none anyone at all.
#[derive(Clone, Copy)]
struct Asker {
    admin: bool,
    user: Uuid,
}

impl From<&Principal> for Asker {
    fn from(p: &Principal) -> Asker {
        Asker {
            admin: p.admin,
            user: p.user_id,
        }
    }
}

impl World {
    async fn load(tx: &mut Tx) -> StoreResult<World> {
        let mut paths: HashMap<Uuid, Vec<Path>> = HashMap::new();
        for p in tx.pack_paths().await? {
            paths.entry(p.pack_id).or_default().push(Path {
                path: p.path,
                sensitive: p.sensitive,
            });
        }
        for v in paths.values_mut() {
            v.sort_by(|a, b| {
                a.path
                    .trim_start_matches(EXCLUDE)
                    .cmp(b.path.trim_start_matches(EXCLUDE))
            });
        }
        let mut usernames: HashMap<Uuid, String> = tx
            .people()
            .await?
            .into_iter()
            .map(|u| (u.id, u.username))
            .collect();
        let copies = tx.copies().await?;
        let packs = tx.packs().await?;
        for id in copies
            .iter()
            .filter_map(|c| c.owner_id)
            .chain(packs.iter().filter_map(|k| k.owner_id))
        {
            if !usernames.contains_key(&id)
                && let Some(u) = tx.user(id).await?
            {
                usernames.insert(id, u.username);
            }
        }
        Ok(World {
            packs,
            paths,
            copies,
            members: tx.team_members().await?,
            usernames,
            slugs: tx
                .teams()
                .await?
                .into_iter()
                .map(|t| (t.id, t.slug))
                .collect(),
        })
    }

    fn role(&self, team: Option<Uuid>, user: Uuid) -> Option<&str> {
        let team = team?;
        self.members
            .iter()
            .find(|m| m.team_id == team && m.user_id == user)
            .map(|m| m.role.as_str())
    }

    fn pack_use(&self, a: Asker, k: &Pack) -> bool {
        a.admin
            || k.builtin
            || k.attach == "everyone"
            || k.owner_id == Some(a.user)
            || self.role(k.team_id, a.user).is_some()
    }

    fn pack_view(&self, a: Asker, k: &Pack) -> Option<PackView> {
        if !self.pack_use(a, k) {
            return None;
        }
        let role = self.role(k.team_id, a.user);
        Some(PackView {
            owner: k.owner_id.and_then(|u| self.usernames.get(&u).cloned()),
            team: k.team_id.and_then(|t| self.slugs.get(&t).cloned()),
            paths: self.paths.get(&k.id).cloned().unwrap_or_default(),
            defaults: k.builtin.then(profile_defaults),
            can_change: a.admin
                || (!k.builtin
                    && (k.owner_id == Some(a.user) || matches!(role, Some("member" | "admin")))),
            can_delete: !k.builtin
                && (a.admin || k.owner_id == Some(a.user) || role == Some("admin")),
            row: k.clone(),
        })
    }

    fn pack(&self, a: Asker, id: Uuid) -> Option<PackView> {
        self.packs
            .iter()
            .find(|k| k.id == id)
            .and_then(|k| self.pack_view(a, k))
    }

    /// A copy a person may use, change and delete: their own is theirs
    /// alone, an admin's included.
    fn copy_view(&self, a: Asker, c: &Copy) -> Option<CopyView> {
        let role = self.role(c.team_id, a.user);
        let mine = c.owner_id == Some(a.user);
        let (usable, write, drop) = if c.personal {
            (mine, mine, mine)
        } else {
            (
                a.admin || mine || role.is_some(),
                a.admin || mine || matches!(role, Some("member" | "admin")),
                a.admin || mine || role == Some("admin"),
            )
        };
        usable.then(|| CopyView {
            owner: c.owner_id.and_then(|u| self.usernames.get(&u).cloned()),
            team: c.team_id.and_then(|t| self.slugs.get(&t).cloned()),
            can_write: write,
            can_delete: drop,
            row: c.clone(),
        })
    }

    fn copy(&self, a: Asker, id: Uuid) -> Option<CopyView> {
        self.copies
            .iter()
            .find(|c| c.id == id)
            .and_then(|c| self.copy_view(a, c))
    }

    fn personal(&self, pack: Uuid, user: Uuid) -> Option<&Copy> {
        self.copies
            .iter()
            .find(|c| c.personal && c.pack_id == pack && c.owner_id == Some(user))
    }
}

fn not_found<T>() -> fail::Result<T> {
    Err(Fail::NotFound("not found".into()))
}

/// A person's own copy of a pack, made if it is not yet.
async fn personal(tx: &mut Tx, w: &mut World, pack: Uuid, user: Uuid) -> StoreResult<Uuid> {
    if let Some(c) = w.personal(pack, user) {
        return Ok(c.id);
    }
    let now = Utc::now();
    let c = Copy {
        id: Uuid::new_v4(),
        pack_id: pack,
        owner_id: Some(user),
        team_id: None,
        personal: true,
        name: String::new(),
        created_at: now,
        updated_at: now,
    };
    if tx.insert_copy(&c).await? {
        w.copies.push(c);
    } else {
        w.copies = tx.copies().await?;
    }
    if !w.usernames.contains_key(&user)
        && let Some(u) = tx.user(user).await?
    {
        w.usernames.insert(user, u.username);
    }
    w.personal(pack, user)
        .map(|c| c.id)
        .ok_or_else(|| StoreError("a personal copy could not be made".into()))
}

/// The copy a person uses of a pack by default: one they chose that they
/// may still use, or their own.
async fn default_copy(tx: &mut Tx, w: &mut World, pack: Uuid, user: Uuid) -> StoreResult<Uuid> {
    let asker = Asker { admin: false, user };
    for d in tx.pack_defaults(user).await? {
        if d.pack_id == pack && w.copy(asker, d.copy_id).is_some() {
            return Ok(d.copy_id);
        }
    }
    personal(tx, w, pack, user).await
}

/// The packs p may use: the built-in ones first, then by name.
pub async fn packs(store: &Store, p: &Principal) -> StoreResult<Vec<PackView>> {
    let w = World::load(&mut store.begin().await?).await?;
    let mut out: Vec<PackView> = w
        .packs
        .iter()
        .filter_map(|k| w.pack_view(p.into(), k))
        .collect();
    out.sort_by(|a, b| {
        (!a.row.builtin, a.row.name.to_lowercase(), a.row.id).cmp(&(
            !b.row.builtin,
            b.row.name.to_lowercase(),
            b.row.id,
        ))
    });
    Ok(out)
}

pub async fn pack(store: &Store, p: &Principal, id: Uuid) -> StoreResult<Option<PackView>> {
    Ok(World::load(&mut store.begin().await?)
        .await?
        .pack(p.into(), id))
}

/// The copies p chose to use of packs by default, by pack, among those
/// they may still use.
pub async fn defaults(store: &Store, p: &Principal) -> StoreResult<HashMap<Uuid, Uuid>> {
    let mut tx = store.begin().await?;
    let w = World::load(&mut tx).await?;
    let asker = Asker {
        admin: false,
        user: p.user_id,
    };
    Ok(tx
        .pack_defaults(p.user_id)
        .await?
        .into_iter()
        .filter(|d| w.copy(asker, d.copy_id).is_some())
        .map(|d| (d.pack_id, d.copy_id))
        .collect())
}

/// What a pack is made or changed with.
pub struct PackInput {
    pub name: String,
    pub description: String,
    pub team: Option<String>,
    pub attach: String,
    pub squash: String,
    pub paths: Vec<Path>,
}

/// A pack's name, attach rule, squash and paths as kept.
fn check_pack(
    p: &Principal,
    mut input: PackInput,
    team: bool,
    builtin: bool,
) -> fail::Result<PackInput> {
    input.name = input.name.trim().to_string();
    input.description = input.description.trim().to_string();
    if input.name.is_empty() || input.name.chars().count() > 100 {
        return fail::invalid("a pack needs a name of at most 100 characters");
    }
    if input.attach.is_empty() {
        input.attach = "listed".into();
    }
    match input.attach.as_str() {
        "listed" => {}
        "owner" if team || builtin => {
            return fail::invalid("only a person's pack attaches to its owner's environments");
        }
        "owner" => {}
        "team" if !team => {
            return fail::invalid("only a team's pack attaches to its members' environments");
        }
        "team" => {}
        "everyone" if !p.admin => {
            return fail::forbidden("only an admin attaches a pack to everyone's environments");
        }
        "everyone" => {}
        other => return fail::invalid(format!("no such attach rule {other:?}")),
    }
    if input.squash.is_empty() {
        input.squash = "all_squash".into();
    }
    if !matches!(
        input.squash.as_str(),
        "all_squash" | "root_squash" | "no_root_squash"
    ) {
        return fail::invalid(format!("no such squash {:?}", input.squash));
    }
    input.paths = check_paths(&input.paths)?;
    Ok(input)
}

/// Whether p may give a team a pack or a copy: a member or admin of it, or
/// an admin.
fn can_give(w: &World, p: &Principal, team: &str) -> fail::Result<Uuid> {
    let Some(id) = Uuid::parse_str(team)
        .ok()
        .filter(|t| w.slugs.contains_key(t))
    else {
        return fail::invalid("no such team");
    };
    if !p.admin && !matches!(w.role(Some(id), p.user_id), Some("member" | "admin")) {
        return fail::forbidden("only the team's members and admins give it packs and copies");
    }
    Ok(id)
}

fn rows_of(pack: Uuid, paths: &[Path]) -> Vec<PackPath> {
    paths
        .iter()
        .map(|p| PackPath {
            pack_id: pack,
            path: p.path.clone(),
            sensitive: p.sensitive,
        })
        .collect()
}

fn pack_event(action: &'static str, k: &PackView) -> Event {
    Event::new(action, "file_pack", k.row.id, &k.row.name)
}

/// Makes a pack, owned by the team it names, which p must be a member or
/// admin of, or else by p.
pub async fn create_pack(
    store: &Store,
    p: &Principal,
    ip: &str,
    input: PackInput,
) -> fail::Result<PackView> {
    let team = input_team(&input);
    let input = check_pack(p, input, team, false)?;
    let mut tx = store.begin().await?;
    let w = World::load(&mut tx).await?;
    let team = match input.team.as_deref().filter(|t| !t.is_empty()) {
        Some(t) => Some(can_give(&w, p, t)?),
        None => None,
    };
    let now = Utc::now();
    let k = Pack {
        id: Uuid::new_v4(),
        name: input.name.clone(),
        description: input.description.clone(),
        owner_id: team.is_none().then_some(p.user_id),
        team_id: team,
        builtin: false,
        attach: input.attach.clone(),
        squash: input.squash.clone(),
        created_at: now,
        updated_at: now,
    };
    if !tx.put_pack(&k, true).await? {
        return fail::invalid(format!("its owner already has a pack named {}", input.name));
    }
    tx.set_pack_paths(k.id, &rows_of(k.id, &input.paths))
        .await?;
    let v = World::load(&mut tx)
        .await?
        .pack(p.into(), k.id)
        .ok_or_else(|| Fail::NotFound("not found".into()))?;
    audit::record(
        &mut tx,
        &p.actor(ip),
        pack_event("file_pack.create", &v).details(serde_json::json!({
            "paths": paths_of(&v.paths),
            "attach": v.row.attach,
        })),
    )
    .await?;
    tx.commit().await?;
    Ok(v)
}

fn input_team(input: &PackInput) -> bool {
    input.team.as_deref().is_some_and(|t| !t.is_empty())
}

/// Changes a pack's name, description, attach rule, squash and paths.
/// Copies keep their files under a path taken out, unshared, until someone
/// deletes them.
pub async fn update_pack(
    store: &Store,
    p: &Principal,
    ip: &str,
    id: Uuid,
    mut input: PackInput,
) -> fail::Result<PackView> {
    let mut tx = store.begin().await?;
    tx.lock_pack(id).await?;
    let w = World::load(&mut tx).await?;
    let Some(cur) = w.pack(p.into(), id) else {
        return not_found();
    };
    if !cur.can_change {
        return fail::forbidden(
            "only the pack's owner, its team's members, or an admin for a built-in one, change it",
        );
    }
    // What is not given stays as it is.
    if input.attach.is_empty() {
        input.attach = cur.row.attach.clone();
    }
    if input.squash.is_empty() {
        input.squash = cur.row.squash.clone();
    }
    let input = check_pack(p, input, cur.row.team_id.is_some(), cur.row.builtin)?;
    if cur.row.builtin && input.attach != "everyone" && input.attach != "listed" {
        return fail::invalid("a built-in pack attaches to everyone's environments or none");
    }
    if cur.row.attach == "everyone" && input.attach != "everyone" && !p.admin {
        return fail::forbidden("only an admin stops a pack attaching to everyone's environments");
    }
    let mut k = cur.row.clone();
    k.name = input.name.clone();
    k.description = input.description.clone();
    k.attach = input.attach.clone();
    k.squash = input.squash.clone();
    k.updated_at = Utc::now();
    if !tx.put_pack(&k, false).await? {
        return fail::invalid(format!("its owner already has a pack named {}", input.name));
    }
    tx.set_pack_paths(id, &rows_of(id, &input.paths)).await?;
    let v = World::load(&mut tx)
        .await?
        .pack(p.into(), id)
        .ok_or_else(|| Fail::NotFound("not found".into()))?;
    audit::record(
        &mut tx,
        &p.actor(ip),
        pack_event("file_pack.update", &v).details(serde_json::json!({
            "paths": paths_of(&v.paths),
            "attach": v.row.attach,
            "squash": v.row.squash,
        })),
    )
    .await?;
    tx.commit().await?;
    Ok(v)
}

/// Deletes a pack and every copy of its files. A built-in pack is not
/// deleted.
pub async fn delete_pack(
    store: &Store,
    root: &Root,
    p: &Principal,
    ip: &str,
    id: Uuid,
) -> fail::Result<()> {
    let mut tx = store.begin().await?;
    tx.lock_pack(id).await?;
    let w = World::load(&mut tx).await?;
    let Some(cur) = w.pack(p.into(), id) else {
        return not_found();
    };
    if !cur.can_delete {
        return fail::forbidden(
            "only the pack's owner, or its team's admins, delete it; a built-in one is not deleted",
        );
    }
    let copies: Vec<Uuid> = w
        .copies
        .iter()
        .filter(|c| c.pack_id == id)
        .map(|c| c.id)
        .collect();
    tx.delete_pack(id).await?;
    audit::record(&mut tx, &p.actor(ip), pack_event("file_pack.delete", &cur)).await?;
    tx.commit().await?;
    remove_copies(root, &copies)
}

/// The copies whose rows go with a person: those they own, and those of
/// the packs they own; for removing their files once they are deleted.
pub async fn copies_of(store: &Store, user: Uuid) -> StoreResult<Vec<Uuid>> {
    let w = World::load(&mut store.begin().await?).await?;
    let packs: Vec<Uuid> = w
        .packs
        .iter()
        .filter(|k| k.owner_id == Some(user))
        .map(|k| k.id)
        .collect();
    Ok(w.copies
        .iter()
        .filter(|c| c.owner_id == Some(user) || packs.contains(&c.pack_id))
        .map(|c| c.id)
        .collect())
}

/// Removes copies' files, once the copies are gone.
pub fn remove_copies(root: &Root, ids: &[Uuid]) -> fail::Result<()> {
    for id in ids {
        root.remove_all(&id.to_string())
            .map_err(|e| StoreError(format!("removing copy {id}'s files: {e}")))?;
    }
    Ok(())
}

/// The copies of a pack p may use: their own first, made if they have
/// none yet, then the shared ones by name.
pub async fn copies(store: &Store, p: &Principal, pack: Uuid) -> fail::Result<Vec<CopyView>> {
    let mut tx = store.begin().await?;
    let mut w = World::load(&mut tx).await?;
    if w.pack(p.into(), pack).is_none() {
        return not_found();
    }
    personal(&mut tx, &mut w, pack, p.user_id).await?;
    let mut out: Vec<CopyView> = w
        .copies
        .iter()
        .filter(|c| c.pack_id == pack)
        .filter_map(|c| w.copy_view(p.into(), c))
        .collect();
    out.sort_by(|a, b| {
        (!a.row.personal, a.row.name.to_lowercase(), a.row.id).cmp(&(
            !b.row.personal,
            b.row.name.to_lowercase(),
            b.row.id,
        ))
    });
    tx.commit().await?;
    Ok(out)
}

pub async fn copy(store: &Store, p: &Principal, id: Uuid) -> StoreResult<Option<CopyView>> {
    Ok(World::load(&mut store.begin().await?)
        .await?
        .copy(p.into(), id))
}

fn copy_event(action: &'static str, c: &CopyView) -> Event {
    Event::new(action, "pack_copy", c.row.id, &c.name())
}

/// Makes a shared copy of a pack p may use, owned by the team it names,
/// which p must be a member or admin of, or else by p.
pub async fn create_copy(
    store: &Store,
    p: &Principal,
    ip: &str,
    pack: Uuid,
    name: &str,
    team: Option<&str>,
) -> fail::Result<CopyView> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return fail::invalid("a shared copy needs a name of at most 100 characters");
    }
    let mut tx = store.begin().await?;
    let w = World::load(&mut tx).await?;
    if w.pack(p.into(), pack).is_none() {
        return not_found();
    }
    let team = match team.filter(|t| !t.is_empty()) {
        Some(t) => Some(can_give(&w, p, t)?),
        None => None,
    };
    let now = Utc::now();
    let c = Copy {
        id: Uuid::new_v4(),
        pack_id: pack,
        owner_id: team.is_none().then_some(p.user_id),
        team_id: team,
        personal: false,
        name: name.to_string(),
        created_at: now,
        updated_at: now,
    };
    tx.insert_copy(&c).await?;
    let v = World::load(&mut tx)
        .await?
        .copy(p.into(), c.id)
        .ok_or_else(|| Fail::NotFound("not found".into()))?;
    audit::record(
        &mut tx,
        &p.actor(ip),
        copy_event("pack_copy.create", &v).related("file_pack", pack),
    )
    .await?;
    tx.commit().await?;
    Ok(v)
}

/// Renames a shared copy p may change.
pub async fn rename_copy(
    store: &Store,
    p: &Principal,
    id: Uuid,
    name: &str,
) -> fail::Result<CopyView> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return fail::invalid("a shared copy needs a name of at most 100 characters");
    }
    let mut tx = store.begin().await?;
    let w = World::load(&mut tx).await?;
    let Some(cur) = w.copy(p.into(), id) else {
        return not_found();
    };
    if cur.row.personal || !cur.can_write {
        return fail::forbidden("only a shared copy's owner, or its team's members, rename it");
    }
    tx.rename_copy(id, name, Utc::now()).await?;
    let v = World::load(&mut tx)
        .await?
        .copy(p.into(), id)
        .ok_or_else(|| Fail::NotFound("not found".into()))?;
    tx.commit().await?;
    Ok(v)
}

/// Deletes a copy p may delete, and its files. Environments that used it
/// are given another by the rules that would have chosen it.
pub async fn delete_copy(
    store: &Store,
    root: &Root,
    p: &Principal,
    ip: &str,
    id: Uuid,
) -> fail::Result<()> {
    let mut tx = store.begin().await?;
    let w = World::load(&mut tx).await?;
    let Some(cur) = w.copy(p.into(), id) else {
        return not_found();
    };
    if !cur.can_delete {
        return fail::forbidden("only a copy's owner, or its team's admins, delete it");
    }
    tx.delete_copy(id).await?;
    audit::record(&mut tx, &p.actor(ip), copy_event("pack_copy.delete", &cur)).await?;
    tx.commit().await?;
    remove_copies(root, &[id])
}

/// Makes a copy the one p uses of its pack unless told otherwise; none
/// returns them to their own.
pub async fn set_default(
    store: &Store,
    p: &Principal,
    pack: Uuid,
    copy: Option<&str>,
) -> fail::Result<()> {
    let mut tx = store.begin().await?;
    let w = World::load(&mut tx).await?;
    if w.pack(p.into(), pack).is_none() {
        return not_found();
    }
    let copy = match copy.filter(|c| !c.is_empty()) {
        None => None,
        Some(c) => {
            let asker = Asker {
                admin: false,
                user: p.user_id,
            };
            match Uuid::parse_str(c).ok().and_then(|id| w.copy(asker, id)) {
                Some(v) if v.row.pack_id == pack => Some(v.row.id),
                _ => return fail::invalid("not a copy of the pack you may use"),
            }
        }
    };
    tx.set_pack_default(p.user_id, pack, copy).await?;
    tx.commit().await?;
    Ok(())
}

/// One of a copy's files.
pub struct File {
    /// Its key: ~/ in the home directory, or absolute.
    pub path: String,
    pub data: Option<Vec<u8>>,
    pub size: u64,
    pub mode: u32,
    pub sensitive: bool,
    /// Under a path the pack shares: a copy keeps a file the pack stopped
    /// sharing until someone deletes it.
    pub shared: bool,
    pub updated_at: DateTime<Utc>,
}

/// Where a copy's file is under the root.
fn key_path(copy: Uuid, key: &str) -> String {
    format!("{copy}/{}", key.trim_start_matches('/'))
}

/// A copy p may use, and its pack's paths.
async fn copy_and_paths(
    store: &Store,
    p: &Principal,
    id: Uuid,
) -> fail::Result<(CopyView, Vec<Path>)> {
    let w = World::load(&mut store.begin().await?).await?;
    let Some(c) = w.copy(p.into(), id) else {
        return not_found();
    };
    let paths = w.paths.get(&c.row.pack_id).cloned().unwrap_or_default();
    Ok((c, paths))
}

fn io(e: std::io::Error) -> Fail {
    Fail::Store(StoreError(format!("the files root: {e}")))
}

/// A copy's files, without their contents, by path.
pub async fn files(store: &Store, root: &Root, p: &Principal, id: Uuid) -> fail::Result<Vec<File>> {
    let (c, paths) = copy_and_paths(store, p, id).await?;
    let shared_paths = paths_of(&paths);
    let mut out: Vec<File> = root
        .files(&c.row.id.to_string())
        .map_err(io)?
        .into_iter()
        .filter(|(rel, _)| !rel.rsplit('/').next().unwrap_or(rel).starts_with(STAGING))
        .map(|(rel, info)| {
            let key = if rel.starts_with(HOME) {
                rel
            } else {
                format!("/{rel}")
            };
            File {
                sensitive: sensitive(&paths, &key),
                shared: shared(&shared_paths, &key),
                size: info.size,
                mode: info.mode & 0o777,
                updated_at: info.modified,
                data: None,
                path: key,
            }
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn read_file(root: &Root, copy: Uuid, paths: &[Path], key: &str) -> fail::Result<File> {
    let rel = key_path(copy, key);
    let info = match root.lstat(&rel) {
        Ok(i) if i.regular => i,
        Ok(_) => return not_found(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return not_found(),
        Err(e) => return Err(io(e)),
    };
    let data = root.read(&rel).map_err(io)?;
    Ok(File {
        path: key.to_string(),
        size: data.len() as u64,
        data: Some(data),
        mode: info.mode & 0o777,
        sensitive: sensitive(paths, key),
        shared: shared(&paths_of(paths), key),
        updated_at: info.modified,
    })
}

/// One of a copy's files with its contents.
pub async fn file(
    store: &Store,
    root: &Root,
    p: &Principal,
    id: Uuid,
    key: &str,
) -> fail::Result<File> {
    if !valid_key(key) {
        return not_found();
    }
    let (c, paths) = copy_and_paths(store, p, id).await?;
    read_file(root, c.row.id, &paths, key)
}

/// A copy p may change the files of, and its pack's paths.
async fn writable(store: &Store, p: &Principal, id: Uuid) -> fail::Result<(CopyView, Vec<Path>)> {
    let (c, paths) = copy_and_paths(store, p, id).await?;
    if !c.can_write {
        return fail::forbidden("only the copy's owner, or its team's members, change its files");
    }
    Ok((c, paths))
}

async fn record_file(
    store: &Store,
    p: &Principal,
    ip: &str,
    action: &'static str,
    c: &CopyView,
    key: &str,
    details: serde_json::Value,
) -> fail::Result<()> {
    let mut tx = store.begin().await?;
    audit::record(
        &mut tx,
        &p.actor(ip),
        Event::new(action, "profile_file", format!("{}:{key}", c.row.id), key)
            .related("pack_copy", c.row.id)
            .details(details),
    )
    .await?;
    tx.signal(Signal::Shared);
    tx.commit().await?;
    Ok(())
}

/// Writes a file at a path the pack shares, giving it as stored. A mode
/// of 0 is the file's own, or 0600 for a sensitive file and 0644 for
/// another.
#[allow(clippy::too_many_arguments)]
pub async fn put(
    store: &Store,
    root: &Root,
    p: &Principal,
    ip: &str,
    id: Uuid,
    key: &str,
    data: &[u8],
    mode: u32,
) -> fail::Result<File> {
    let (c, paths) = writable(store, p, id).await?;
    if !shared(&paths_of(&paths), key) {
        return fail::invalid(format!("the pack does not share {key}"));
    }
    if data.len() > MAX_FILE_SIZE {
        return fail::invalid(format!("{key} is larger than {MAX_FILE_SIZE} bytes"));
    }
    let target = key_path(c.row.id, key);
    let others: u64 = root
        .files(&c.row.id.to_string())
        .map_err(io)?
        .iter()
        .filter(|(rel, _)| format!("{}/{rel}", c.row.id) != target)
        .map(|(_, i)| i.size)
        .sum();
    if others + data.len() as u64 > MAX_COPY_SIZE {
        return fail::invalid(format!(
            "the copy would hold more than {} MiB",
            MAX_COPY_SIZE >> 20
        ));
    }
    let mut mode = mode & 0o777;
    if mode == 0 {
        mode = match root.lstat(&target) {
            Ok(i) => i.mode & 0o777,
            Err(_) if sensitive(&paths, key) => 0o600,
            Err(_) => 0o644,
        };
    }
    let mut name = [0u8; 6];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut name);
    let staging = format!(
        "{STAGING}{}",
        name.iter().map(|b| format!("{b:02x}")).collect::<String>()
    );
    root.replace(&target, data, mode, &staging).map_err(io)?;
    record_file(
        store,
        p,
        ip,
        "pack_copy.file_write",
        &c,
        key,
        serde_json::json!({ "size": data.len() }),
    )
    .await?;
    read_file(root, c.row.id, &paths, key)
}

/// Changes a file's mode.
pub async fn chmod(
    store: &Store,
    root: &Root,
    p: &Principal,
    ip: &str,
    id: Uuid,
    key: &str,
    mode: u32,
) -> fail::Result<File> {
    if !valid_key(key) {
        return not_found();
    }
    let mode = mode & 0o777;
    if mode == 0 {
        return fail::invalid("a file needs a mode");
    }
    let (c, paths) = writable(store, p, id).await?;
    let rel = key_path(c.row.id, key);
    match root.lstat(&rel) {
        Ok(i) if i.regular => {}
        Ok(_) => return not_found(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return not_found(),
        Err(e) => return Err(io(e)),
    }
    root.chmod(&rel, mode).map_err(io)?;
    record_file(
        store,
        p,
        ip,
        "pack_copy.file_mode",
        &c,
        key,
        serde_json::json!({ "mode": format!("{mode:#o}") }),
    )
    .await?;
    read_file(root, c.row.id, &paths, key)
}

/// Removes a file from a copy, and so from every environment using it.
pub async fn delete_file(
    store: &Store,
    root: &Root,
    p: &Principal,
    ip: &str,
    id: Uuid,
    key: &str,
) -> fail::Result<()> {
    if !valid_key(key) {
        return fail::invalid(format!("{key:?} is not a file's path"));
    }
    let (c, _) = writable(store, p, id).await?;
    root.remove(&key_path(c.row.id, key)).map_err(io)?;
    record_file(
        store,
        p,
        ip,
        "pack_copy.file_delete",
        &c,
        key,
        serde_json::json!({}),
    )
    .await
}

/// How the copy an environment uses of a pack was chosen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Chosen {
    Environment,
    Template,
    Kept,
    Default,
}

impl Chosen {
    pub fn as_str(self) -> &'static str {
        match self {
            Chosen::Environment => "environment",
            Chosen::Template => "template",
            Chosen::Kept => "kept",
            Chosen::Default => "default",
        }
    }
}

/// A pack an environment has, and the copy it uses of it.
#[derive(Debug, Clone)]
pub struct Attached {
    pub pack: PackView,
    pub copy: CopyView,
    pub from: Chosen,
    /// Its template lists it, rather than its attaching itself.
    pub listed: bool,
}

/// The packs an environment has, in order, with the copy of each it uses:
/// those its template lists, then -- unless it keeps them out -- those
/// attaching themselves, its owner's, their teams', then everyone's. A
/// pack its owner may not use is left out. A copy given by default is
/// kept, so it does not change under the environment.
async fn attached(
    tx: &mut Tx,
    w: &mut World,
    env: Uuid,
    owner: Uuid,
    spec: &Spec,
) -> StoreResult<Vec<Attached>> {
    let asker = Asker {
        admin: false,
        user: owner,
    };
    let mut out: Vec<(PackView, bool, Option<String>)> = Vec::new();
    for r in &spec.packs {
        let Ok(id) = Uuid::parse_str(&r.pack) else {
            continue;
        };
        if out.iter().any(|(k, _, _)| k.row.id == id) {
            continue;
        }
        if let Some(k) = w.pack(asker, id) {
            out.push((k, true, Some(r.copy.clone())));
        }
    }
    if !spec.access.no_self_attached {
        let mut own: Vec<&Pack> = w
            .packs
            .iter()
            .filter(|k| w.pack_use(asker, k))
            .filter(|k| {
                (k.attach == "owner" && k.owner_id == Some(owner))
                    || (k.attach == "team" && w.role(k.team_id, owner).is_some())
                    || k.attach == "everyone"
            })
            .collect();
        let rank = |k: &Pack| match k.attach.as_str() {
            "owner" => 0,
            "team" => 1,
            _ => 2,
        };
        own.sort_by(|a, b| {
            (rank(a), a.builtin, a.name.to_lowercase(), a.id).cmp(&(
                rank(b),
                b.builtin,
                b.name.to_lowercase(),
                b.id,
            ))
        });
        let own: Vec<PackView> = own
            .into_iter()
            .filter_map(|k| w.pack_view(asker, k))
            .collect();
        for k in own {
            if !out.iter().any(|(o, _, _)| o.row.id == k.row.id) {
                out.push((k, false, None));
            }
        }
    }
    let keeps: HashMap<Uuid, (Uuid, bool)> = tx
        .environment_copies(env)
        .await?
        .into_iter()
        .map(|e| (e.pack_id, (e.copy_id, e.chosen)))
        .collect();
    let mut result = Vec::with_capacity(out.len());
    for (k, listed, pin) in out {
        let usable = |w: &World, id: Option<Uuid>| {
            id.and_then(|id| w.copy(asker, id))
                .filter(|c| c.row.pack_id == k.row.id)
        };
        let kept = keeps.get(&k.row.id).copied();
        let pin = pin.and_then(|p| Uuid::parse_str(&p).ok());
        let tries = [
            (
                kept.filter(|(_, chosen)| *chosen).map(|(c, _)| c),
                Chosen::Environment,
            ),
            (pin, Chosen::Template),
            (
                kept.filter(|(_, chosen)| !*chosen).map(|(c, _)| c),
                Chosen::Kept,
            ),
        ];
        let found = tries
            .into_iter()
            .find_map(|(id, from)| usable(w, id).map(|c| (c, from)));
        let (copy, from) = match found {
            Some(x) => x,
            None => {
                let id = default_copy(tx, w, k.row.id, owner).await?;
                tx.set_environment_copy(env, k.row.id, Some((id, false)))
                    .await?;
                let c = w
                    .copy(asker, id)
                    .ok_or_else(|| StoreError(format!("copy {id} is not its owner's to use")))?;
                (c, Chosen::Default)
            }
        };
        result.push(Attached {
            pack: k,
            copy,
            from,
            listed,
        });
    }
    Ok(result)
}

/// The packs an environment has and the copy it uses of each.
pub async fn environment_packs(store: &Store, env: Uuid) -> fail::Result<Vec<Attached>> {
    let mut tx = store.begin().await?;
    let Some(e) = tx
        .environments(crate::store::environments::Only::Id(env))
        .await?
        .pop()
    else {
        return not_found();
    };
    let spec: Spec = serde_json::from_value(e.spec).unwrap_or_default();
    let mut w = World::load(&mut tx).await?;
    let out = attached(&mut tx, &mut w, env, e.owner_id, &spec).await?;
    tx.commit().await?;
    Ok(out)
}

/// Has an environment use a copy of one of its packs that its owner may
/// use; none returns it to the rules. p must own the environment.
pub async fn choose_copy(
    store: &Store,
    p: &Principal,
    ip: &str,
    env: Uuid,
    pack: Uuid,
    copy: Option<&str>,
) -> fail::Result<()> {
    let mut tx = store.begin().await?;
    let Some(e) = tx
        .environments(crate::store::environments::Only::Id(env))
        .await?
        .pop()
    else {
        return not_found();
    };
    if e.owner_id != p.user_id {
        return fail::forbidden("only the environment's owner chooses its copies");
    }
    let chosen = match copy.filter(|c| !c.is_empty()) {
        None => None,
        Some(c) => {
            let w = World::load(&mut tx).await?;
            let asker = Asker {
                admin: false,
                user: e.owner_id,
            };
            match Uuid::parse_str(c).ok().and_then(|id| w.copy(asker, id)) {
                Some(v) if v.row.pack_id == pack => Some((v.row.id, true)),
                _ => return fail::invalid("not a copy of the pack its owner may use"),
            }
        }
    };
    tx.set_environment_copy(env, pack, chosen).await?;
    audit::record(
        &mut tx,
        &p.actor(ip),
        Event::new("environment.choose_copy", "environment", env, "")
            .related("file_pack", pack)
            .related("pack_copy", copy.unwrap_or_default()),
    )
    .await?;
    tx.signal(Signal::Shared);
    tx.commit().await?;
    Ok(())
}

/// The copies an environment routes to, in order, with each one's routed
/// paths. A path two of them name is the first's.
pub async fn environment_sets(store: &Store, env: Uuid) -> fail::Result<Vec<(Uuid, Vec<String>)>> {
    Ok(environment_packs(store, env)
        .await?
        .into_iter()
        .map(|a| (a.copy.row.id, routed(&paths_of(&a.pack.paths))))
        .collect())
}

/// Records the copies an environment's agent routes to: its worker goes on
/// serving those beside the ones it has, so the agent can copy a copy's
/// files down as it stops routing to it.
pub async fn set_routed(store: &Store, env: Uuid, copies: &[String]) -> fail::Result<()> {
    let mut ids: Vec<Uuid> = Vec::new();
    for c in copies {
        if let Ok(id) = Uuid::parse_str(c)
            && !ids.contains(&id)
        {
            ids.push(id);
        }
    }
    let mut tx = store.begin().await?;
    tx.set_routed(env, &ids).await?;
    tx.commit().await?;
    Ok(())
}

/// What of the files root an environment placed on a worker may reach: the
/// copies it has and those its agent still routes to, and in each, for an
/// environment given no sensitive files, those under sensitive paths,
/// hidden. One placed elsewhere, or nowhere, is not found.
pub async fn environment_files(
    store: &Store,
    worker: Uuid,
    env: Uuid,
) -> fail::Result<EnvironmentFiles> {
    let mut tx = store.begin().await?;
    let Some(e) = tx
        .environments(crate::store::environments::Only::Id(env))
        .await?
        .pop()
        .filter(|e| e.worker_id == Some(worker))
    else {
        return not_found();
    };
    let spec: Spec = serde_json::from_value(e.spec).unwrap_or_default();
    let mut w = World::load(&mut tx).await?;
    let attached = attached(&mut tx, &mut w, env, e.owner_id, &spec).await?;
    let mut out = EnvironmentFiles::default();
    let mut pack_of: HashMap<Uuid, Vec<Path>> = HashMap::new();
    let squash_of =
        |s: &str| serde_json::from_value::<Squash>(serde_json::json!(s)).unwrap_or_default();
    let squash = |out: &mut EnvironmentFiles, id: Uuid, s: &str| {
        if !s.is_empty() && s != "all_squash" {
            out.squash.insert(id.to_string(), squash_of(s));
        }
    };
    for a in &attached {
        let id = a.copy.row.id;
        if !out.sets.contains(&id.to_string()) {
            out.sets.push(id.to_string());
            pack_of.insert(id, a.pack.paths.clone());
            squash(&mut out, id, &a.pack.row.squash);
        }
    }
    for id in tx.environment_routed(env).await? {
        if out.sets.contains(&id.to_string()) {
            continue;
        }
        let Some(c) = w.copies.iter().find(|c| c.id == id) else {
            continue;
        };
        let Some(k) = w.packs.iter().find(|k| k.id == c.pack_id) else {
            continue;
        };
        out.sets.push(id.to_string());
        squash(&mut out, id, &k.squash);
        pack_of.insert(
            id,
            w.paths
                .get(&k.id)
                .map(|ps| ps.iter().filter(|p| p.sensitive).cloned().collect())
                .unwrap_or_default(),
        );
    }
    tx.commit().await?;
    if !spec.access.no_sensitive_files {
        return Ok(out);
    }
    let mut hidden: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (id, paths) in pack_of {
        for p in paths
            .iter()
            .filter(|p| p.sensitive && !p.path.starts_with(EXCLUDE))
        {
            hidden
                .entry(id.to_string())
                .or_default()
                .push(p.path.trim_start_matches('/').to_string());
        }
    }
    out.hidden = hidden;
    Ok(out)
}

/// Records a conflict an environment's agent found.
pub async fn add_conflict(store: &Store, env: Uuid, copy: &str, path: &str) -> fail::Result<()> {
    let Some(copy) = Uuid::parse_str(copy).ok().filter(|_| valid_key(path)) else {
        return fail::invalid("not a conflict");
    };
    let mut tx = store.begin().await?;
    tx.add_conflict(env, copy, path, Utc::now()).await?;
    tx.commit().await?;
    Ok(())
}

/// An environment's conflicts, by path.
pub async fn conflicts(store: &Store, env: Uuid) -> StoreResult<Vec<crate::store::rows::Conflict>> {
    store.begin().await?.conflicts(env).await
}

/// Records how p, the environment's owner, resolves one of its conflicts;
/// its agent does it when it next hears from the server. To use the
/// environment's file, p must be able to change the copy's.
pub async fn resolve_conflict(
    store: &Store,
    p: &Principal,
    ip: &str,
    env: Uuid,
    copy: &str,
    path: &str,
    resolution: &str,
) -> fail::Result<()> {
    if resolution != "shared" && resolution != "environment" {
        return fail::invalid("resolve with \"shared\" or \"environment\"");
    }
    let Ok(copy) = Uuid::parse_str(copy) else {
        return not_found();
    };
    let mut tx = store.begin().await?;
    let Some(e) = tx
        .environments(crate::store::environments::Only::Id(env))
        .await?
        .pop()
    else {
        return not_found();
    };
    if e.owner_id != p.user_id {
        return fail::forbidden("only the environment's owner resolves its conflicts");
    }
    if resolution == "environment" {
        let w = World::load(&mut tx).await?;
        let asker = Asker {
            admin: false,
            user: e.owner_id,
        };
        let Some(c) = w.copy(asker, copy) else {
            return not_found();
        };
        if !c.can_write {
            return fail::forbidden("you may not change the copy's files");
        }
    }
    if !tx.resolve_conflict(env, copy, path, resolution).await? {
        return not_found();
    }
    audit::record(
        &mut tx,
        &p.actor(ip),
        Event::new("environment.resolve_conflict", "environment", env, "")
            .related("pack_copy", copy)
            .details(serde_json::json!({ "path": path, "resolution": resolution })),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Forgets a conflict the environment's agent has resolved.
pub async fn conflict_done(store: &Store, env: Uuid, copy: &str, path: &str) -> fail::Result<()> {
    let Ok(copy) = Uuid::parse_str(copy) else {
        return Ok(());
    };
    let mut tx = store.begin().await?;
    tx.delete_conflict(env, copy, path).await?;
    tx.commit().await?;
    Ok(())
}
