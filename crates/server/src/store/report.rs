//! A worker's status report taken into the store, as internal/workers'
//! ReportStatus takes it: each environment it reports, those it has let
//! go, and the worker's own capacity and state.

use chrono::{DateTime, Utc};
use hangar_api::{ObservedEnvironment, WorkerStatus};
use sqlx::Row;
use uuid::Uuid;

use super::{Backend, Result, Tx};

/// One environment the report moved from one phase to another.
pub struct PhaseChange {
    pub id: Uuid,
    pub name: String,
    pub owner: Uuid,
    pub from: String,
    pub to: String,
    pub reason: String,
}

/// What a report changed, for the audit log and whoever waits on it.
pub struct Taken {
    pub worker_name: String,
    pub phases: Vec<PhaseChange>,
    /// Environments deleted, which the worker has let go: ID, name,
    /// owner.
    pub deleted: Vec<(Uuid, String, Uuid)>,
}

/// An environment placed on the worker, as the report finds it.
struct Placed {
    id: Uuid,
    desired: String,
    name: String,
    owner: Uuid,
    phase: String,
}

fn json<T: serde::Serialize>(v: &Option<T>) -> Option<serde_json::Value> {
    v.as_ref().and_then(|v| serde_json::to_value(v).ok())
}

fn phase_name(o: &ObservedEnvironment) -> String {
    serde_json::to_value(o.phase)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The phases a phase leaves the environment's coding agents' sessions in.
fn keeps_agents(phase: &str) -> bool {
    matches!(phase, "starting" | "running" | "suspending" | "suspended")
}

impl Tx {
    /// Takes st, from the worker with id, at `now`; None if there is no such
    /// worker.
    pub async fn report(
        &mut self,
        worker_id: Uuid,
        st: &WorkerStatus,
        now: DateTime<Utc>,
    ) -> Result<Option<Taken>> {
        let placed = self.placed(worker_id).await?;
        let Some(worker_name) = self.worker_name(worker_id).await? else {
            return Ok(None);
        };
        let mut unknown = Vec::new();
        let mut reported = Vec::new();
        let mut phases = Vec::new();
        for o in &st.environments {
            let id = Uuid::parse_str(&o.id).ok();
            let Some(p) = id.and_then(|id| placed.iter().find(|p| p.id == id)) else {
                unknown.push(o.id.clone());
                continue;
            };
            reported.push(p.id);
            let to = phase_name(o);
            if p.phase != to {
                phases.push(PhaseChange {
                    id: p.id,
                    name: p.name.clone(),
                    owner: p.owner,
                    from: p.phase.clone(),
                    to: to.clone(),
                    reason: o.reason.clone(),
                });
            }
            self.observe(p.id, o, &to, now).await?;
        }
        let mut deleted = Vec::new();
        for p in placed
            .iter()
            .filter(|p| p.desired == "deleted" && !reported.contains(&p.id))
        {
            self.forget_environment(p.id).await?;
            deleted.push((p.id, p.name.clone(), p.owner));
        }
        if !deleted.is_empty() {
            self.bump(worker_id).await?;
        }
        self.worker_reported(worker_id, st, &unknown, now).await?;
        Ok(Some(Taken {
            worker_name,
            phases,
            deleted,
        }))
    }

    /// The environments placed on a worker, held until the transaction ends.
    async fn placed(&mut self, worker_id: Uuid) -> Result<Vec<Placed>> {
        match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(
                "SELECT id, desired, name, owner_id, phase FROM environments WHERE worker_id = $1
                 ORDER BY id FOR UPDATE",
            )
            .bind(worker_id)
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(|r| {
                Ok(Placed {
                    id: r.try_get(0)?,
                    desired: r.try_get(1)?,
                    name: r.try_get(2)?,
                    owner: r.try_get(3)?,
                    phase: r.try_get(4)?,
                })
            })
            .collect(),
            Backend::Json(tx) => Ok(tx
                .work
                .environments
                .iter()
                .filter(|e| e.worker_id == Some(worker_id))
                .map(|e| Placed {
                    id: e.id,
                    desired: e.desired.clone(),
                    name: e.name.clone(),
                    owner: e.owner_id,
                    phase: e.phase.clone(),
                })
                .collect()),
        }
    }

    /// A worker's name, its row held until the transaction ends.
    async fn worker_name(&mut self, id: Uuid) -> Result<Option<String>> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query_scalar(
                "SELECT name FROM workers WHERE id = $1 FOR UPDATE",
            )
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?),
            Backend::Json(tx) => Ok(tx
                .work
                .workers
                .iter()
                .find(|w| w.id == id)
                .map(|w| w.name.clone())),
        }
    }

    /// Takes what a worker says of one of its environments.
    async fn observe(
        &mut self,
        id: Uuid,
        o: &ObservedEnvironment,
        phase: &str,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let (stats, progress) = (json(&o.stats), json(&o.progress));
        let (update, rollback) = (json(&o.image_update), json(&o.image_rollback));
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query(
                    "UPDATE environments SET phase = $2, reason = $3, stats = $4, progress = $5,
                        image_digest = CASE WHEN $6 <> '' OR $7 THEN $6 ELSE image_digest END,
                        image_pin_want = CASE WHEN $7 AND image_pin_want = $6 THEN NULL ELSE image_pin_want END,
                        image_update = $8, image_rollback = $9,
                        image_drop_rollback = image_drop_rollback AND $9::jsonb IS NOT NULL,
                        updated_at = CASE WHEN (phase, reason) IS DISTINCT FROM ($2, $3) THEN $10 ELSE updated_at END
                     WHERE id = $1",
                )
                .bind(id)
                .bind(phase)
                .bind(&o.reason)
                .bind(&stats)
                .bind(&progress)
                .bind(&o.image_digest)
                .bind(o.image_pinned)
                .bind(&update)
                .bind(&rollback)
                .bind(now)
                .execute(&mut **tx)
                .await?;
                if !keeps_agents(phase) {
                    sqlx::query("DELETE FROM agent_sessions WHERE environment_id = $1")
                        .bind(id)
                        .execute(&mut **tx)
                        .await?;
                }
            }
            Backend::Json(tx) => {
                if let Some(e) = tx.work.environments.iter_mut().find(|e| e.id == id) {
                    if (e.phase.as_str(), e.reason.as_str()) != (phase, o.reason.as_str()) {
                        e.updated_at = now;
                    }
                    e.phase = phase.to_string();
                    e.reason = o.reason.clone();
                    e.stats = stats;
                    e.progress = progress;
                    if !o.image_digest.is_empty() || o.image_pinned {
                        e.image_digest = o.image_digest.clone();
                    }
                    if o.image_pinned
                        && e.image_pin_want.as_deref() == Some(o.image_digest.as_str())
                    {
                        e.image_pin_want = None;
                    }
                    e.image_drop_rollback = e.image_drop_rollback && rollback.is_some();
                    e.image_update = update;
                    e.image_rollback = rollback;
                }
            }
        }
        Ok(())
    }

    /// Deletes the row of an environment its worker has let go.
    async fn forget_environment(&mut self, id: Uuid) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query("DELETE FROM environments WHERE id = $1")
                    .bind(id)
                    .execute(&mut **tx)
                    .await?;
            }
            Backend::Json(tx) => tx.work.environments.retain(|e| e.id != id),
        }
        Ok(())
    }

    /// Keeps what a worker says of itself, seen at `now`, and lets go of the
    /// removals it has done.
    async fn worker_reported(
        &mut self,
        id: Uuid,
        st: &WorkerStatus,
        unknown: &[String],
        now: DateTime<Utc>,
    ) -> Result<()> {
        let labels = serde_json::to_value(&st.labels).unwrap_or_else(|_| serde_json::json!({}));
        let unknown_json = serde_json::to_value(unknown).unwrap_or_else(|_| serde_json::json!([]));
        let stats = json(&st.stats);
        let images = serde_json::to_value(&st.images).unwrap_or_else(|_| serde_json::json!([]));
        let gpu = json(&st.gpu);
        let held: Vec<String> = st
            .images
            .iter()
            .map(|i| format!("{}@{}", i.r#ref, i.digest))
            .collect();
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query(
                    "UPDATE workers SET cpus = $2, memory_mib = $3, labels = $4, unknown = $5,
                        stats = $6, images = $7, gpu = $8, monitor = $9, suspends = $10, last_seen_at = $11
                     WHERE id = $1",
                )
                .bind(id)
                .bind(st.capacity.cpus as i32)
                .bind(st.capacity.memory_mib as i32)
                .bind(&labels)
                .bind(&unknown_json)
                .bind(&stats)
                .bind(&images)
                .bind(&gpu)
                .bind(&st.monitor)
                .bind(st.suspends)
                .bind(now)
                .execute(&mut **tx)
                .await?;
                sqlx::query(
                    "DELETE FROM worker_image_removals
                     WHERE worker_id = $1 AND NOT (ref || '@' || digest = ANY($2::text[]))",
                )
                .bind(id)
                .bind(&held)
                .execute(&mut **tx)
                .await?;
                sqlx::query(
                    "DELETE FROM worker_environment_removals
                     WHERE worker_id = $1 AND NOT (environment = ANY($2::text[]))",
                )
                .bind(id)
                .bind(unknown)
                .execute(&mut **tx)
                .await?;
            }
            Backend::Json(tx) => {
                if let Some(w) = tx.work.workers.iter_mut().find(|w| w.id == id) {
                    w.cpus = st.capacity.cpus as i32;
                    w.memory_mib = st.capacity.memory_mib as i32;
                    w.labels = labels;
                    w.unknown = unknown_json;
                    w.stats = stats;
                    w.images = images;
                    w.gpu = gpu;
                    w.monitor = st.monitor.clone();
                    w.suspends = st.suspends;
                    w.last_seen_at = Some(now);
                }
                tx.work.worker_image_removals.retain(|r| {
                    r.worker_id != id || held.contains(&format!("{}@{}", r.r#ref, r.digest))
                });
                tx.work
                    .worker_environment_removals
                    .retain(|r| r.worker_id != id || unknown.contains(&r.environment));
            }
        }
        Ok(())
    }
}
