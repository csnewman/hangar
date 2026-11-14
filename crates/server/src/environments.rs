//! Environments as their owners see them: what exists, and what each should
//! be doing (internal/environments).
//!
//! A user reaches only the environments they own, an administrator all of
//! them, and one the asker may not reach is not found, so its existence is
//! not revealed. Where an environment runs is placement's business, and
//! what it is doing its worker's report: this changes only what it should
//! be doing, and signals whichever of the two acts on the change.

use std::collections::HashMap;

use chrono::Utc;
use hangar_api::{
    Display, EnvironmentStats, Gpu, GpuInfo, ImageRollback, ImageUpdate, Progress, Repo, Spec,
    TemplateSpec,
};
use rand::Rng;
use uuid::Uuid;

use crate::audit::{self, Event};
use crate::fail::{self, Fail};
use crate::store::environments::{Inserted, Only};
use crate::store::rows::{AgentSession, Environment, Task, Worker};
use crate::store::signals::Signal;
use crate::store::{Error as StoreError, Result as StoreResult, Store, Tx};
use crate::templates::{self, Vars};
use crate::users::Principal;
use crate::workers;

/// The room a running environment's memory keeps above what its programs
/// use when it shrinks: a guest asked for memory it is using gives up its
/// caches, then kills what it runs to find it.
pub const SHRINK_HEADROOM_MIB: i64 = 512;

/// The longest a task is, in characters.
pub const MAX_TASK_LENGTH: usize = 200;

/// The worker an environment is placed on, as the environment shows it.
#[derive(Debug, Clone)]
pub struct Placed {
    pub name: String,
    pub online: bool,
    pub monitor: String,
    pub suspends: bool,
    gpu: Option<GpuInfo>,
}

/// An environment as someone who may reach it sees it.
#[derive(Debug, Clone)]
pub struct View {
    pub row: Environment,
    pub owner: String,
    pub spec: Spec,
    pub worker: Option<Placed>,
    /// Its usage, while it runs.
    pub stats: Option<EnvironmentStats>,
    /// How far its start has got, while it starts.
    pub progress: Option<Progress>,
    /// The settings its template would give it otherwise.
    pub template_changes: Vec<&'static str>,
    /// Its template has changed since it took its settings.
    pub template_updated: bool,
    pub image_update: Option<ImageUpdate>,
    pub image_rollback: Option<ImageRollback>,
    /// What its next start does to its image.
    pub image_change: Option<&'static str>,
    /// What its virtual GPU renders with, and whether on the host's CPU.
    pub gpu_renderer: Option<(String, bool)>,
    pub task: Option<Task>,
    /// Its coding agents' sessions; a finished one its owner has seen is
    /// idle.
    pub agents: Vec<Agent>,
}

/// A coding agent's session.
#[derive(Debug, Clone)]
pub struct Agent {
    pub row: AgentSession,
    pub state: String,
    pub seen: bool,
}

impl Agent {
    /// It is asking, or has finished, and its owner has not yet looked.
    pub fn needs_you(&self) -> bool {
        !self.seen && matches!(self.state.as_str(), "asking" | "finished")
    }
}

impl View {
    /// Its machine is up and meant to stay so, on a worker that is
    /// reporting: one whose memory can be resized.
    fn running(&self) -> bool {
        self.row.desired == "running"
            && self.row.phase == "running"
            && self.worker.as_ref().is_some_and(|w| w.online)
    }

    fn vars(&self) -> Vars<'_> {
        Vars {
            name: &self.row.name,
            owner: &self.owner,
            short_id: &self.row.short_id,
            template: &self.row.template_name,
        }
    }
}

fn decode<T: serde::de::DeserializeOwned>(
    what: &str,
    id: Uuid,
    v: &serde_json::Value,
) -> StoreResult<T> {
    serde_json::from_value(v.clone())
        .map_err(|e| StoreError(format!("environment {id}'s {what}: {e}")))
}

/// The environments only names that p may reach, or every one with no p,
/// oldest first.
async fn views(tx: &mut Tx, p: Option<&Principal>, only: Only) -> StoreResult<Vec<View>> {
    let rows: Vec<Environment> = tx
        .environments(only)
        .await?
        .into_iter()
        .filter(|e| p.is_none_or(|p| p.admin || e.owner_id == p.user_id))
        .collect();
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let mut owners: HashMap<Uuid, String> = HashMap::new();
    if rows.iter().any(|e| e.owner_id != rows[0].owner_id) {
        owners = tx
            .people()
            .await?
            .into_iter()
            .map(|u| (u.id, u.username))
            .collect();
    }
    for e in &rows {
        if !owners.contains_key(&e.owner_id)
            && let Some(u) = tx.user(e.owner_id).await?
        {
            owners.insert(u.id, u.username);
        }
    }
    let placed = rows.iter().any(|e| e.worker_id.is_some());
    let workers: HashMap<Uuid, Worker> = if placed {
        tx.workers(None)
            .await?
            .into_iter()
            .map(|l| (l.worker.id, l.worker))
            .collect()
    } else {
        HashMap::new()
    };
    let occupying = if placed {
        tx.occupying().await?
    } else {
        Vec::new()
    };
    let templates: HashMap<Uuid, (TemplateSpec, i64)> =
        if rows.iter().any(|e| e.template_id.is_some()) {
            tx.templates()
                .await?
                .into_iter()
                .filter(|t| rows.iter().any(|e| e.template_id == Some(t.id)))
                .map(|t| {
                    (
                        t.id,
                        (
                            serde_json::from_value(t.spec).unwrap_or_default(),
                            t.revision,
                        ),
                    )
                })
                .collect()
        } else {
            HashMap::new()
        };
    let ids: Vec<Uuid> = rows.iter().map(|e| e.id).collect();
    let mut tasks: HashMap<Uuid, Task> = tx
        .current_tasks(&ids)
        .await?
        .into_iter()
        .map(|t| (t.environment_id, t))
        .collect();
    let sessions = tx.agent_sessions(&ids).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let spec: Spec = decode("spec", row.id, &row.spec)?;
        let worker = row.worker_id.and_then(|id| workers.get(&id));
        let mut stats: Option<EnvironmentStats> = None;
        if row.phase == "running"
            && let Some(s) = &row.stats
        {
            let mut s: EnvironmentStats = decode("stats", row.id, s)?;
            // It can grow only as far as its worker has room.
            if s.machine_memory_max_mib > 0 {
                let room = worker.map_or(0, |w| {
                    w.memory_mib as i64
                        - occupying
                            .iter()
                            .filter(|(wid, eid, _, _)| *wid == w.id && *eid != row.id)
                            .map(|o| o.3)
                            .sum::<i64>()
                });
                s.machine_memory_max_mib = s
                    .machine_memory_max_mib
                    .min(room)
                    .max(row.memory_mib as i64);
            }
            stats = Some(s);
        }
        let progress = match (&row.progress, row.phase.as_str()) {
            (Some(p), "starting") => Some(decode("progress", row.id, p)?),
            _ => None,
        };
        let image_update: Option<ImageUpdate> = row
            .image_update
            .as_ref()
            .map(|u| decode("image update", row.id, u))
            .transpose()?;
        let image_rollback: Option<ImageRollback> = row
            .image_rollback
            .as_ref()
            .map(|r| decode("image rollback", row.id, r))
            .transpose()?;
        let image_change = match &row.image_pin_want {
            Some(want) if *want != row.image_digest => {
                if image_rollback.as_ref().is_some_and(|r| r.digest == *want) {
                    Some(hangar_api::IMAGE_CHANGE_ROLLBACK)
                } else {
                    Some(hangar_api::IMAGE_CHANGE_UPGRADE)
                }
            }
            _ => None,
        };
        let gpu: Option<GpuInfo> = worker
            .and_then(|w| w.gpu.as_ref())
            .map(|g| serde_json::from_value(g.clone()).unwrap_or_default());
        let gpu_renderer = match (&gpu, spec.gpu) {
            (Some(g), Gpu::Virtual) => g.gl.as_ref().map(|gl| (gl.renderer.clone(), gl.software)),
            _ => None,
        };
        let agents = sessions
            .iter()
            .filter(|a| a.environment_id == row.id)
            .map(|a| {
                let seen = a.seen_at.is_some_and(|s| s >= a.since);
                let state = if a.state == "finished" && seen {
                    "idle".to_string()
                } else {
                    a.state.clone()
                };
                Agent {
                    row: a.clone(),
                    state,
                    seen,
                }
            })
            .collect();
        let mut v = View {
            owner: owners.get(&row.owner_id).cloned().unwrap_or_default(),
            spec,
            worker: worker.map(|w| Placed {
                name: w.name.clone(),
                online: workers::online(w),
                monitor: w.monitor.clone(),
                suspends: w.suspends,
                gpu,
            }),
            stats,
            progress,
            template_changes: Vec::new(),
            template_updated: false,
            image_update,
            image_rollback,
            image_change,
            gpu_renderer,
            task: tasks.remove(&row.id),
            agents,
            row,
        };
        if let Some((t, revision)) = v.row.template_id.and_then(|id| templates.get(&id)) {
            v.template_changes = template_changes(&v, t);
            v.template_updated = *revision > v.row.template_revision;
        }
        out.push(v);
    }
    Ok(out)
}

/// One environment, if p may reach it.
async fn view(tx: &mut Tx, p: Option<&Principal>, id: Uuid) -> StoreResult<Option<View>> {
    Ok(views(tx, p, Only::Id(id)).await?.pop())
}

/// The settings in which an environment differs from what its template's
/// current ones would give it.
fn template_changes(e: &View, t: &TemplateSpec) -> Vec<&'static str> {
    let Ok(want) = templates::resolve(t, &e.vars()) else {
        return vec!["name"];
    };
    let s = &e.spec;
    [
        (s.image != want.image, "image"),
        (s.cpus != want.cpus, "cpus"),
        (s.memory_mib != want.memory_mib, "memory"),
        (s.display != want.display, "display"),
        (s.gpu != want.gpu, "gpu"),
        (s.dax != want.dax, "dax"),
        (s.swap_mib != want.swap_mib, "swap"),
        (s.memory_reclaim != want.memory_reclaim, "memory_reclaim"),
        (s.repos != want.repos, "repos"),
        (s.project_folders() != want.project_folders(), "projects"),
        (s.access != want.access, "access"),
        (s.trusted_folders != want.trusted_folders, "trusted_folders"),
        (s.web_names != want.web_names, "web_names"),
        (s.packs != want.packs, "packs"),
        (s.placement != want.placement, "placement"),
    ]
    .into_iter()
    .filter_map(|(differ, name)| differ.then_some(name))
    .collect()
}

fn not_found<T>() -> fail::Result<T> {
    Err(Fail::NotFound("environment not found".into()))
}

/// The environment with id, if p may reach it, its row held until the
/// transaction ends.
async fn locked(tx: &mut Tx, p: &Principal, id: Uuid) -> fail::Result<View> {
    tx.lock_environment(id).await?;
    match view(tx, Some(p), id).await? {
        Some(v) => Ok(v),
        None => not_found(),
    }
}

/// The environment with id as it stands, which the caller has just
/// written.
async fn after(tx: &mut Tx, id: Uuid) -> fail::Result<View> {
    match view(tx, None, id).await? {
        Some(v) => Ok(v),
        None => not_found(),
    }
}

/// Every environment p may reach, oldest first.
pub async fn list(store: &Store, p: &Principal) -> StoreResult<Vec<View>> {
    let only = if p.admin {
        Only::All
    } else {
        Only::Owner(p.user_id)
    };
    views(&mut store.begin().await?, Some(p), only).await
}

pub async fn get(store: &Store, p: &Principal, id: Uuid) -> StoreResult<Option<View>> {
    view(&mut store.begin().await?, Some(p), id).await
}

/// A machine an environment is made with in place of its template's.
pub struct Machine {
    pub cpus: i64,
    pub memory_mib: i64,
    pub display: Display,
    pub gpu: Gpu,
    pub dax: bool,
    pub swap_mib: Option<i64>,
    pub memory_reclaim: bool,
}

/// What an environment is made from.
pub struct Create {
    pub template_id: String,
    pub name: String,
    pub machine: Option<Machine>,
    pub repos: Option<Vec<Repo>>,
}

fn passthrough_refused<T>() -> fail::Result<T> {
    fail::invalid("a GPU passed through is only a template's to ask for")
}

/// A template's spec with the machine and repositories asked for in place
/// of its own, held to what a template may ask. Repositories chosen for an
/// environment are its projects.
fn with_own(mut t: TemplateSpec, req: &Create) -> fail::Result<TemplateSpec> {
    if req.machine.is_none() && req.repos.is_none() {
        return Ok(t);
    }
    if let Some(m) = &req.machine {
        if m.gpu == Gpu::Passthrough && t.spec.gpu != Gpu::Passthrough {
            return passthrough_refused();
        }
        let s = &mut t.spec;
        (s.cpus, s.memory_mib, s.display, s.gpu, s.dax) =
            (m.cpus, m.memory_mib, m.display, m.gpu, m.dax);
        (s.swap_mib, s.memory_reclaim) = (m.swap_mib, m.memory_reclaim);
    }
    if let Some(repos) = &req.repos {
        t.spec.repos = repos.clone();
        t.spec.projects = Vec::new();
        t.spec.editor_path = String::new();
    }
    templates::validate(t)
}

/// A short ID: six lower-case letters and digits, at random.
fn short_id() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut r = rand::thread_rng();
    (0..6)
        .map(|_| ALPHABET[r.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

/// Makes an environment, owned by p, from a template p may use, and asks
/// for it to run; placement takes it from there. The template's spec is
/// resolved for the name and copied into the environment as it is now.
pub async fn create(store: &Store, p: &Principal, ip: &str, req: Create) -> fail::Result<View> {
    let mut tx = store.begin().await?;
    let template = match Uuid::parse_str(&req.template_id) {
        Ok(id) => templates::for_use(&mut tx, p, id).await?,
        Err(_) => None,
    };
    let Some((tname, tspec, revision)) = template else {
        return fail::invalid("no such template");
    };
    let tspec = with_own(tspec, &req)?;
    let Some(owner) = tx.user(p.user_id).await? else {
        return not_found();
    };
    let now = Utc::now();
    let mut tries = 0;
    let (row, spec) = loop {
        if tries == 10 {
            return Err(Fail::Store(StoreError(
                "no free short ID for the environment after 10 tries".into(),
            )));
        }
        tries += 1;
        let short = short_id();
        let spec = templates::resolve(
            &tspec,
            &Vars {
                name: &req.name,
                owner: &owner.username,
                short_id: &short,
                template: &tname,
            },
        )?;
        let row = Environment {
            id: Uuid::new_v4(),
            short_id: short,
            owner_id: p.user_id,
            name: req.name.clone(),
            template_id: Uuid::parse_str(&req.template_id).ok(),
            template_name: tname.clone(),
            template_revision: revision,
            spec: serde_json::to_value(&spec).map_err(|e| StoreError(e.to_string()))?,
            image: spec.image.clone(),
            image_digest: String::new(),
            image_pin_want: None,
            image_drop_rollback: false,
            image_update: None,
            image_rollback: None,
            ports_public: false,
            cpus: spec.cpus as i32,
            memory_mib: spec.memory_mib as i32,
            desired: "running".into(),
            worker_id: None,
            phase: "pending".into(),
            reason: String::new(),
            stats: None,
            progress: None,
            created_at: now,
            updated_at: now,
        };
        match tx.insert_environment(&row).await? {
            Inserted::Done => break (row, spec),
            Inserted::ShortIdTaken => continue,
            Inserted::NameTaken => {
                return fail::conflict(format!(
                    "you already have an environment named {}",
                    req.name
                ));
            }
        }
    };
    tx.signal(Signal::Placement);
    audit::record(
        &mut tx,
        &p.actor(ip),
        Event::new("environment.create", "environment", row.id, &row.name)
            .related("owner", row.owner_id)
            .related("template", &req.template_id)
            .related("image", &spec.image)
            .details(serde_json::json!({
                "cpus": spec.cpus,
                "memory_mib": spec.memory_mib,
                "access": spec.access,
            })),
    )
    .await?;
    let v = after(&mut tx, row.id).await?;
    tx.commit().await?;
    Ok(v)
}

/// The audit action for asking an environment to be in a state.
fn verb(desired: &str) -> &'static str {
    match desired {
        "running" => "environment.start",
        "stopped" => "environment.stop",
        "suspended" => "environment.suspend",
        _ => "environment.delete",
    }
}

/// Changes what an environment should be doing, and says whether it still
/// exists afterwards. One being deleted cannot be brought back. One no
/// worker holds has nothing to wait for: deleting it removes it at once,
/// and stopping it stops it.
pub async fn set_desired(
    store: &Store,
    p: &Principal,
    ip: &str,
    id: Uuid,
    desired: &str,
) -> fail::Result<bool> {
    let mut tx = store.begin().await?;
    let cur = locked(&mut tx, p, id).await?;
    let mut row = cur.row.clone();
    if row.desired == desired {
        return Ok(true);
    }
    if row.desired == "deleted" {
        return fail::conflict("the environment is being deleted");
    }
    // Only a machine that is running has memory to keep.
    if desired == "suspended" && (row.desired != "running" || row.worker_id.is_none()) {
        return fail::conflict("only a running environment can be suspended");
    }
    if desired == "suspended"
        && let Some(w) = &cur.worker
        && !w.suspends
    {
        return fail::conflict(format!(
            "its worker, {}, cannot suspend a machine; stop it instead",
            w.name
        ));
    }
    // A stopped or suspended environment's disks are on its worker, so it
    // runs only there, and only when that worker has room: its CPUs and
    // memory were given back when it stopped.
    if let Some(worker) = row.worker_id
        && desired == "running"
    {
        let Some(room) = tx.room_for(worker, id).await? else {
            return not_found();
        };
        if row.cpus as i64 > room.free_cpus || row.memory_mib as i64 > room.free_memory_mib {
            return fail::conflict(format!(
                "its worker, {}, has no room for it: it needs {} vCPUs and {} MiB, and {} vCPUs and {} MiB are free; stop another environment there first",
                room.name,
                row.cpus,
                row.memory_mib,
                room.free_cpus.max(0),
                room.free_memory_mib.max(0)
            ));
        }
    }
    let from = row.desired.clone();
    let mut exists = true;
    let now = Utc::now();
    if let Some(worker) = row.worker_id {
        row.desired = desired.to_string();
        row.updated_at = now;
        tx.save_environment(&row).await?;
        tx.bump(worker).await?;
    } else {
        match desired {
            "deleted" => {
                exists = false;
                tx.delete_environment(id).await?;
            }
            "stopped" => {
                row.desired = desired.to_string();
                row.phase = "stopped".into();
                row.reason = String::new();
                row.updated_at = now;
                tx.save_environment(&row).await?;
            }
            _ => {
                row.desired = desired.to_string();
                row.phase = "pending".into();
                row.reason = String::new();
                row.updated_at = now;
                tx.save_environment(&row).await?;
                tx.signal(Signal::Placement);
            }
        }
    }
    let mut e = Event::new(verb(desired), "environment", id, &row.name)
        .related("owner", row.owner_id)
        .related("image", &row.image);
    if let Some(w) = row.worker_id {
        e = e.related("worker", w);
    }
    audit::record(
        &mut tx,
        &p.actor(ip),
        e.details(serde_json::json!({ "from": from })),
    )
    .await?;
    tx.commit().await?;
    Ok(exists)
}

/// Fails for an environment whose settings cannot change as it is: one
/// with a machine up, or one suspended.
fn stopped_for_change(e: &View) -> fail::Result<()> {
    let r = &e.row;
    if r.desired == "suspended" || r.phase == "suspended" {
        return fail::conflict(
            "the environment is suspended; stop it first, which ends what is running in it",
        );
    }
    // One not yet placed has no machine to start with the settings it had.
    let unplaced = r.worker_id.is_none() && r.phase == "pending";
    if (r.desired != "stopped" && !unplaced) || r.phase == "stopping" {
        return fail::conflict(
            "stop the environment first; the change takes effect when it next starts",
        );
    }
    Ok(())
}

/// Fails for a virtual GPU on a worker that offers none.
fn check_gpu(e: &View, gpu: Gpu) -> fail::Result<()> {
    let Some(w) = &e.worker else {
        return Ok(());
    };
    if gpu != Gpu::Virtual {
        return Ok(());
    }
    match &w.gpu {
        None => fail::invalid(format!(
            "its worker, {}, has no virtual GPU to offer",
            w.name
        )),
        Some(g) if !g.unavailable.is_empty() => fail::invalid(format!(
            "its worker, {}, offers no virtual GPU: {}",
            w.name, g.unavailable
        )),
        Some(_) => Ok(()),
    }
}

/// Fails for a memory size a running environment cannot be resized to.
async fn resizable(tx: &mut Tx, e: &View, mib: i64) -> fail::Result<()> {
    let Some(st) = &e.stats else {
        return fail::conflict(
            "its worker has not yet said how far its memory can be resized; try again in a few seconds",
        );
    };
    if st.machine_memory_max_mib == 0 {
        return fail::conflict(
            "it started with no room to resize its memory while it runs; stop it to change its memory, and from its next start it can be resized while it runs",
        );
    }
    if mib < st.machine_memory_min_mib {
        return fail::conflict(format!(
            "while it runs, its memory can go down to {} MiB, what it started with; stop it to go lower",
            st.machine_memory_min_mib
        ));
    }
    if mib > st.machine_memory_max_mib {
        return fail::conflict(format!(
            "while it runs, its memory can go up to {} MiB, all its worker has room for",
            st.machine_memory_max_mib
        ));
    }
    if mib < e.spec.memory_mib && mib < st.memory_used_mib + SHRINK_HEADROOM_MIB {
        return fail::conflict(format!(
            "its programs use {} MiB, so while it runs its memory can go down to {} MiB; stop it, or what uses the memory, to go lower",
            st.memory_used_mib,
            st.memory_used_mib + SHRINK_HEADROOM_MIB
        ));
    }
    if mib <= e.spec.memory_mib {
        return Ok(());
    }
    let Some(worker) = e.row.worker_id else {
        return Ok(());
    };
    let Some(room) = tx.room_for(worker, e.row.id).await? else {
        return not_found();
    };
    if mib > room.free_memory_mib {
        return fail::conflict(format!(
            "its worker, {}, has room for {} MiB of memory for it, and it asks for {mib} MiB",
            room.name,
            room.free_memory_mib.max(0)
        ));
    }
    Ok(())
}

/// Writes an environment's spec, and the template revision it follows when
/// reset to its template.
async fn save_spec(
    tx: &mut Tx,
    cur: &View,
    spec: &Spec,
    revision: Option<i64>,
) -> fail::Result<()> {
    let mut row = cur.row.clone();
    row.spec = serde_json::to_value(spec).map_err(|e| StoreError(e.to_string()))?;
    row.image = spec.image.clone();
    row.cpus = spec.cpus as i32;
    row.memory_mib = spec.memory_mib as i32;
    if let Some(r) = revision {
        row.template_revision = r;
    }
    row.updated_at = Utc::now();
    tx.save_environment(&row).await?;
    // One waiting for a worker may fit somewhere it did not.
    if row.worker_id.is_none() && row.desired == "running" {
        tx.signal(Signal::Placement);
    }
    Ok(())
}

/// What of an environment's machine changes after it is made. They take
/// effect when it next starts, but for its memory, which a running machine
/// is resized to, and its memory reclaim, which a running guest is told
/// of.
pub type Settings = Machine;

/// Changes a stopped environment's machine, or a running one's memory and
/// memory reclaim, held to what a template may ask.
pub async fn update_settings(
    store: &Store,
    p: &Principal,
    ip: &str,
    id: Uuid,
    s: Settings,
) -> fail::Result<View> {
    let mut tx = store.begin().await?;
    let cur = locked(&mut tx, p, id).await?;
    let c = &cur.spec;
    let live = cur.running()
        && s.cpus == c.cpus
        && s.display == c.display
        && s.gpu == c.gpu
        && s.dax == c.dax
        && s.swap_mib == c.swap_mib;
    if !live && let Err(e) = stopped_for_change(&cur) {
        if cur.running() {
            return fail::conflict(
                "stop the environment to change its vCPUs, display, GPU or swap; its memory and memory reclaim can change while it runs",
            );
        }
        return Err(e);
    }
    let mut spec = cur.spec.clone();
    (spec.cpus, spec.memory_mib, spec.display, spec.gpu, spec.dax) =
        (s.cpus, s.memory_mib, s.display, s.gpu, s.dax);
    (spec.swap_mib, spec.memory_reclaim) = (s.swap_mib, s.memory_reclaim);
    if s.gpu == Gpu::Passthrough && cur.spec.gpu != Gpu::Passthrough {
        return passthrough_refused();
    }
    let spec = templates::validate(TemplateSpec {
        spec,
        ..TemplateSpec::default()
    })?
    .spec;
    check_gpu(&cur, spec.gpu)?;
    if live && spec.memory_mib != c.memory_mib {
        resizable(&mut tx, &cur, spec.memory_mib).await?;
    }
    save_spec(&mut tx, &cur, &spec, None).await?;
    if live && let Some(w) = cur.row.worker_id {
        // The worker resizes the machine when it sees the new spec.
        tx.bump(w).await?;
    }
    let c = &cur.spec;
    audit::record(
        &mut tx,
        &p.actor(ip),
        Event::new(
            "environment.update_settings",
            "environment",
            id,
            &cur.row.name,
        )
        .related("owner", cur.row.owner_id)
        .details(serde_json::json!({
            "cpus": { "from": c.cpus, "to": spec.cpus },
            "memory_mib": { "from": c.memory_mib, "to": spec.memory_mib },
            "display": { "from": c.display, "to": spec.display },
            "gpu": { "from": c.gpu, "to": spec.gpu },
            "dax": { "from": c.dax, "to": spec.dax },
            "swap_mib": { "from": c.swap_mib, "to": spec.swap_mib },
            "memory_reclaim": { "from": c.memory_reclaim, "to": spec.memory_reclaim },
            "running": live,
        })),
    )
    .await?;
    let v = after(&mut tx, id).await?;
    tx.commit().await?;
    Ok(v)
}

/// Gives a stopped environment its template's current settings, keeping
/// its disks. What cannot change under those disks refuses it: another
/// image, which its writable layer was not made over; other repositories,
/// cloned when it was made; and placement its worker does not meet.
pub async fn reset_to_template(
    store: &Store,
    p: &Principal,
    ip: &str,
    id: Uuid,
) -> fail::Result<View> {
    let mut tx = store.begin().await?;
    let cur = locked(&mut tx, p, id).await?;
    stopped_for_change(&cur)?;
    let Some(template) = cur.row.template_id else {
        return fail::conflict(format!(
            "its template, {}, has been deleted",
            cur.row.template_name
        ));
    };
    let Some((_, tspec, revision)) = templates::for_use(&mut tx, p, template).await? else {
        return fail::conflict(format!(
            "its template, {}, is not one you may use",
            cur.row.template_name
        ));
    };
    let spec = match templates::resolve(&tspec, &cur.vars()) {
        Ok(s) => s,
        Err(Fail::Invalid(m)) => {
            return fail::conflict(format!(
                "its template does not accept the name {}: {}",
                cur.row.name,
                m.strip_prefix("invalid: ").unwrap_or(&m)
            ));
        }
        Err(e) => return Err(e),
    };
    if spec.image != cur.spec.image {
        return fail::conflict(format!(
            "the template names another image, {}; make a new environment from it",
            spec.image
        ));
    }
    if spec.repos != cur.spec.repos {
        return fail::conflict(
            "the template has other repositories, which are cloned only when an environment is made; make a new environment from it",
        );
    }
    if let Some(w) = cur.row.worker_id
        && !spec.placement.is_empty()
    {
        let labels: HashMap<String, String> = tx
            .workers(Some(w))
            .await?
            .pop()
            .and_then(|l| serde_json::from_value(l.worker.labels).ok())
            .unwrap_or_default();
        for (k, v) in &spec.placement {
            if labels.get(k) != Some(v) {
                return fail::conflict(format!(
                    "the template's placement wants {k}={v}, which its worker, {}, where its disks are, does not have",
                    cur.worker
                        .as_ref()
                        .map(|w| w.name.as_str())
                        .unwrap_or_default()
                ));
            }
        }
    }
    check_gpu(&cur, spec.gpu)?;
    save_spec(&mut tx, &cur, &spec, Some(revision)).await?;
    audit::record(
        &mut tx,
        &p.actor(ip),
        Event::new(
            "environment.reset_to_template",
            "environment",
            id,
            &cur.row.name,
        )
        .related("owner", cur.row.owner_id)
        .related("template", template)
        .details(serde_json::json!({ "changed": cur.template_changes, "revision": revision })),
    )
    .await?;
    let v = after(&mut tx, id).await?;
    tx.commit().await?;
    Ok(v)
}

/// A change to an environment's image: the copy its next start pins it
/// to, whether its rollback is dropped, and what the audit log says.
struct ImageChange {
    want: Option<String>,
    drop: bool,
    action: &'static str,
    details: serde_json::Value,
}

/// Sets the copy an environment's next start pins it to, and whether its
/// rollback is dropped, and tells its worker. Some changes need it
/// stopped.
async fn change_image(
    store: &Store,
    p: &Principal,
    ip: &str,
    id: Uuid,
    stopped: bool,
    decide: impl FnOnce(&View) -> fail::Result<ImageChange>,
) -> fail::Result<View> {
    let mut tx = store.begin().await?;
    let cur = locked(&mut tx, p, id).await?;
    if stopped {
        stopped_for_change(&cur)?;
    }
    let c = decide(&cur)?;
    let mut row = cur.row.clone();
    row.image_pin_want = c.want;
    row.image_drop_rollback = c.drop;
    row.updated_at = Utc::now();
    tx.save_environment(&row).await?;
    if let Some(w) = row.worker_id {
        tx.bump(w).await?;
    }
    audit::record(
        &mut tx,
        &p.actor(ip),
        Event::new(c.action, "environment", id, &row.name)
            .related("owner", row.owner_id)
            .related("image", &row.image)
            .details(c.details),
    )
    .await?;
    let v = after(&mut tx, id).await?;
    tx.commit().await?;
    Ok(v)
}

/// Why upgrading is not known to be safe, if it is not.
fn upgrade_risk(u: &ImageUpdate) -> fail::Result<()> {
    if !u.checked {
        return fail::conflict(
            "its writable disk is still being compared with the newer image; try again in a moment",
        );
    }
    if !u.error.is_empty() {
        return fail::conflict(format!(
            "its writable disk could not be compared with the newer image: {}",
            u.error
        ));
    }
    if u.packages {
        return fail::conflict(
            "packages have been installed in it, and its package database would hide the newer image's",
        );
    }
    if !u.conflicts.is_empty() {
        let n = u.conflicts.len() as i64 + u.more_conflicts;
        let mut shown = u.conflicts[..u.conflicts.len().min(3)].join(", ");
        if n > 3 {
            shown.push_str(", ...");
        }
        return fail::conflict(format!(
            "{n} file(s) it has changed would hide the newer image's: {shown}"
        ));
    }
    Ok(())
}

/// Re-pins a stopped environment to the newer copy of its image its worker
/// holds, at its next start, its writable layer kept over it. Unless
/// forced, a file the layer changed that the newer copy also changes
/// refuses it, as does a comparison not yet made or that failed.
pub async fn upgrade_image(
    store: &Store,
    p: &Principal,
    ip: &str,
    id: Uuid,
    force: bool,
) -> fail::Result<View> {
    change_image(store, p, ip, id, true, |cur| {
        let Some(u) = &cur.image_update else {
            return fail::conflict("its worker holds no newer copy of its image");
        };
        if !force {
            upgrade_risk(u)?;
        }
        Ok(ImageChange {
            want: Some(u.digest.clone()),
            drop: false,
            action: "environment.image_upgrade",
            details: serde_json::json!({
                "from": cur.row.image_digest,
                "to": u.digest,
                "forced": force,
                "conflicts": u.conflicts.len() as i64 + u.more_conflicts,
                "packages": u.packages,
            }),
        })
    })
    .await
}

/// Puts a stopped environment back as it was before its last upgrade at
/// its next start, losing what was written to its disk since.
pub async fn rollback_image(
    store: &Store,
    p: &Principal,
    ip: &str,
    id: Uuid,
) -> fail::Result<View> {
    change_image(store, p, ip, id, true, |cur| {
        let Some(rb) = &cur.image_rollback else {
            return fail::conflict("there is no upgrade to roll back");
        };
        Ok(ImageChange {
            want: Some(rb.digest.clone()),
            drop: false,
            action: "environment.image_rollback",
            details: serde_json::json!({ "from": cur.row.image_digest, "to": rb.digest }),
        })
    })
    .await
}

/// Keeps an environment's upgrade, discarding the disk kept to roll back
/// to.
pub async fn keep_image(store: &Store, p: &Principal, ip: &str, id: Uuid) -> fail::Result<View> {
    change_image(store, p, ip, id, false, |cur| {
        if cur.image_rollback.is_none() {
            return fail::conflict("there is no upgrade to keep");
        }
        Ok(ImageChange {
            want: None,
            drop: true,
            action: "environment.image_keep",
            details: serde_json::json!({ "digest": cur.row.image_digest }),
        })
    })
    .await
}

/// Withdraws an upgrade or rollback not yet made.
pub async fn cancel_image_change(
    store: &Store,
    p: &Principal,
    ip: &str,
    id: Uuid,
) -> fail::Result<View> {
    change_image(store, p, ip, id, false, |cur| {
        let Some(change) = cur.image_change else {
            return fail::conflict("no change to its image is waiting");
        };
        Ok(ImageChange {
            want: None,
            drop: false,
            action: "environment.image_change_cancel",
            details: serde_json::json!({ "change": change }),
        })
    })
    .await
}

/// Says whether anyone may reach an environment's own web servers without
/// signing in, or only those who may reach the environment.
pub async fn set_ports_public(
    store: &Store,
    p: &Principal,
    ip: &str,
    id: Uuid,
    public: bool,
) -> fail::Result<View> {
    let mut tx = store.begin().await?;
    let cur = locked(&mut tx, p, id).await?;
    if cur.row.ports_public != public {
        let mut row = cur.row.clone();
        row.ports_public = public;
        row.updated_at = Utc::now();
        tx.save_environment(&row).await?;
        audit::record(
            &mut tx,
            &p.actor(ip),
            Event::new("environment.ports_public", "environment", id, &row.name)
                .related("owner", row.owner_id)
                .details(serde_json::json!({ "public": public })),
        )
        .await?;
    }
    let v = after(&mut tx, id).await?;
    tx.commit().await?;
    Ok(v)
}

/// A task as it is kept: on one line, spaces collapsed, between 1 and
/// MAX_TASK_LENGTH characters.
pub fn clean_task(text: &str) -> fail::Result<String> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return fail::invalid("a task needs some text");
    }
    if text.chars().count() > MAX_TASK_LENGTH {
        return fail::invalid(format!("a task has at most {MAX_TASK_LENGTH} characters"));
    }
    Ok(text)
}

/// Sets an environment's task, by agent or user; the task before it is
/// done. The same text again is the same task.
async fn put_task(tx: &mut Tx, env: Uuid, text: &str, by: &str) -> fail::Result<Task> {
    let text = clean_task(text)?;
    if let Some(t) = tx.current_tasks(&[env]).await?.pop() {
        if t.text == text {
            return Ok(t);
        }
        tx.end_task(env, Utc::now()).await?;
    }
    let t = Task {
        id: Uuid::new_v4(),
        environment_id: env,
        text,
        set_by: by.to_string(),
        set_at: Utc::now(),
        done_at: None,
    };
    tx.insert_task(&t).await?;
    Ok(t)
}

/// Sets the task of an environment p may reach, as its owner would.
pub async fn set_task(store: &Store, p: &Principal, id: Uuid, text: &str) -> fail::Result<Task> {
    let mut tx = store.begin().await?;
    locked(&mut tx, p, id).await?;
    let t = put_task(&mut tx, id, text, "user").await?;
    tx.commit().await?;
    Ok(t)
}

/// Marks the task of an environment p may reach done.
pub async fn end_task(store: &Store, p: &Principal, id: Uuid) -> fail::Result<()> {
    let mut tx = store.begin().await?;
    locked(&mut tx, p, id).await?;
    tx.end_task(id, Utc::now()).await?;
    tx.commit().await?;
    Ok(())
}

/// The tasks of an environment p may reach, newest first.
pub async fn tasks(store: &Store, p: &Principal, id: Uuid) -> fail::Result<Vec<Task>> {
    let mut tx = store.begin().await?;
    locked(&mut tx, p, id).await?;
    Ok(tx.tasks_of(id).await?)
}

/// Notes that p has looked at an agent's session in an environment they
/// may reach.
pub async fn see_agent(store: &Store, p: &Principal, id: Uuid, session: &str) -> fail::Result<()> {
    let mut tx = store.begin().await?;
    locked(&mut tx, p, id).await?;
    if !tx.see_agent(id, session, Utc::now()).await? {
        return Err(Fail::NotFound("no such session".into()));
    }
    tx.commit().await?;
    Ok(())
}

/// Records someone reaching into an environment: a terminal, its editor,
/// its desktop. It changes nothing, so it is recorded on its own, and a
/// failure to record it is logged rather than refusing them.
pub async fn record_access(
    store: &Store,
    actor: &crate::audit::Actor,
    v: &View,
    action: &'static str,
    details: serde_json::Value,
) {
    let mut e = Event::new(action, "environment", v.row.id, &v.row.name)
        .related("owner", v.row.owner_id)
        .related("image", &v.row.image)
        .details(details);
    if let Some(w) = v.row.worker_id {
        e = e.related("worker", w);
    }
    let done = async {
        let mut tx = store.begin().await?;
        audit::record(&mut tx, actor, e).await?;
        tx.commit().await
    };
    if let Err(err) = done.await {
        eprintln!("hangar-server: recording {action}: {err}");
    }
}
