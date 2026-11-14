//! Workers and what they are asked to run: each read and write for both
//! backends, as internal/workers does it.

use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::postgres::PgRow;
use uuid::Uuid;

use super::rows::{Environment, ImageRemoval, Worker};
use super::{Backend, Result, Tx};

pub(super) const WORKER_COLUMNS: &str = "id, name, credential_hash, labels, cpus, memory_mib, unknown, stats, \
     images, gpu, desired_version, last_seen_at, revoked_at, created_at, monitor, suspends";

pub const ENVIRONMENT_COLUMNS: &str = "id, short_id, owner_id, name, template_id, template_name, \
     template_revision, spec, image, image_digest, image_pin_want, image_drop_rollback, image_update, \
     image_rollback, ports_public, cpus, memory_mib, desired, worker_id, phase, reason, stats, progress, \
     created_at, updated_at";

pub(super) fn worker(r: &PgRow) -> Result<Worker> {
    Ok(Worker {
        id: r.try_get("id")?,
        name: r.try_get("name")?,
        credential_hash: r.try_get("credential_hash")?,
        labels: r.try_get("labels")?,
        cpus: r.try_get("cpus")?,
        memory_mib: r.try_get("memory_mib")?,
        unknown: r.try_get("unknown")?,
        stats: r.try_get("stats")?,
        images: r.try_get("images")?,
        gpu: r.try_get("gpu")?,
        desired_version: r.try_get("desired_version")?,
        last_seen_at: r.try_get("last_seen_at")?,
        revoked_at: r.try_get("revoked_at")?,
        created_at: r.try_get("created_at")?,
        monitor: r.try_get("monitor")?,
        suspends: r.try_get("suspends")?,
    })
}

pub fn environment(r: &PgRow) -> Result<Environment> {
    Ok(Environment {
        id: r.try_get("id")?,
        short_id: r.try_get("short_id")?,
        owner_id: r.try_get("owner_id")?,
        name: r.try_get("name")?,
        template_id: r.try_get("template_id")?,
        template_name: r.try_get("template_name")?,
        template_revision: r.try_get("template_revision")?,
        spec: r.try_get("spec")?,
        image: r.try_get("image")?,
        image_digest: r.try_get("image_digest")?,
        image_pin_want: r.try_get("image_pin_want")?,
        image_drop_rollback: r.try_get("image_drop_rollback")?,
        image_update: r.try_get("image_update")?,
        image_rollback: r.try_get("image_rollback")?,
        ports_public: r.try_get("ports_public")?,
        cpus: r.try_get("cpus")?,
        memory_mib: r.try_get("memory_mib")?,
        desired: r.try_get("desired")?,
        worker_id: r.try_get("worker_id")?,
        phase: r.try_get("phase")?,
        reason: r.try_get("reason")?,
        stats: r.try_get("stats")?,
        progress: r.try_get("progress")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

/// A worker as its list shows it: its row, what its environments take of
/// it, and the image copies it is yet to delete.
pub struct Listed {
    pub worker: Worker,
    pub allocated_cpus: i64,
    pub allocated_memory_mib: i64,
    pub removals: Vec<ImageRemoval>,
}

impl Tx {
    /// Registers a worker; false if one has its name.
    pub async fn insert_worker(&mut self, w: &Worker) -> Result<bool> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                let r = sqlx::query(&format!(
                    "INSERT INTO workers ({WORKER_COLUMNS})
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)"
                ))
                .bind(w.id)
                .bind(&w.name)
                .bind(&w.credential_hash)
                .bind(&w.labels)
                .bind(w.cpus)
                .bind(w.memory_mib)
                .bind(&w.unknown)
                .bind(&w.stats)
                .bind(&w.images)
                .bind(&w.gpu)
                .bind(w.desired_version)
                .bind(w.last_seen_at)
                .bind(w.revoked_at)
                .bind(w.created_at)
                .bind(&w.monitor)
                .bind(w.suspends)
                .execute(&mut **tx)
                .await;
                match r {
                    Ok(_) => Ok(true),
                    Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Ok(false),
                    Err(e) => Err(e.into()),
                }
            }
            Backend::Json(tx) => {
                if tx.work.workers.iter().any(|x| x.name == w.name) {
                    return Ok(false);
                }
                tx.work.workers.push(w.clone());
                Ok(true)
            }
        }
    }

    /// The credential hash of a worker not revoked.
    pub async fn worker_credential(&mut self, id: Uuid) -> Result<Option<Vec<u8>>> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query_scalar(
                "SELECT credential_hash FROM workers WHERE id = $1 AND revoked_at IS NULL",
            )
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?),
            Backend::Json(tx) => Ok(tx
                .work
                .workers
                .iter()
                .find(|w| w.id == id && w.revoked_at.is_none())
                .map(|w| w.credential_hash.clone())),
        }
    }

    /// Marks a worker seen at `now`.
    pub async fn touch_worker(&mut self, id: Uuid, now: DateTime<Utc>) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query("UPDATE workers SET last_seen_at = $2 WHERE id = $1")
                    .bind(id)
                    .bind(now)
                    .execute(&mut **tx)
                    .await?;
            }
            Backend::Json(tx) => {
                for w in tx.work.workers.iter_mut().filter(|w| w.id == id) {
                    w.last_seen_at = Some(now);
                }
            }
        }
        Ok(())
    }

    /// Every worker, or only the one with id, by name.
    pub async fn workers(&mut self, only: Option<Uuid>) -> Result<Vec<Listed>> {
        let (ws, envs, removals) = match &mut self.inner {
            Backend::Pg(tx) => {
                let ws: Vec<Worker> = sqlx::query(&format!(
                    "SELECT {WORKER_COLUMNS} FROM workers WHERE $1::uuid IS NULL OR id = $1 ORDER BY name"
                ))
                .bind(only)
                .fetch_all(&mut **tx)
                .await?
                .iter()
                .map(worker)
                .collect::<Result<_>>()?;
                let used: Vec<(Uuid, i64, i64)> = sqlx::query(
                    "SELECT worker_id, coalesce(sum(cpus), 0)::bigint, coalesce(sum(memory_mib), 0)::bigint
                     FROM environments WHERE worker_id IS NOT NULL AND desired <> 'deleted' GROUP BY worker_id",
                )
                .fetch_all(&mut **tx)
                .await?
                .iter()
                .map(|r| Ok((r.try_get(0)?, r.try_get(1)?, r.try_get(2)?)))
                .collect::<Result<_>>()?;
                let removals: Vec<ImageRemoval> = sqlx::query(
                    "SELECT worker_id, ref, digest FROM worker_image_removals ORDER BY ref, digest",
                )
                .fetch_all(&mut **tx)
                .await?
                .iter()
                .map(|r| {
                    Ok(ImageRemoval {
                        worker_id: r.try_get(0)?,
                        r#ref: r.try_get(1)?,
                        digest: r.try_get(2)?,
                    })
                })
                .collect::<Result<_>>()?;
                (ws, used, removals)
            }
            Backend::Json(tx) => {
                let mut ws: Vec<Worker> = tx
                    .work
                    .workers
                    .iter()
                    .filter(|w| only.is_none_or(|id| w.id == id))
                    .cloned()
                    .collect();
                ws.sort_by(|a, b| a.name.cmp(&b.name));
                let used =
                    ws.iter()
                        .map(|w| {
                            let mine =
                                tx.work.environments.iter().filter(|e| {
                                    e.worker_id == Some(w.id) && e.desired != "deleted"
                                });
                            let (c, m) = mine.fold((0i64, 0i64), |(c, m), e| {
                                (c + e.cpus as i64, m + e.memory_mib as i64)
                            });
                            (w.id, c, m)
                        })
                        .collect();
                let mut removals = tx.work.worker_image_removals.clone();
                removals.sort_by(|a, b| (&a.r#ref, &a.digest).cmp(&(&b.r#ref, &b.digest)));
                (ws, used, removals)
            }
        };
        Ok(ws
            .into_iter()
            .map(|w| {
                let (c, m) = envs
                    .iter()
                    .find(|(id, _, _)| *id == w.id)
                    .map(|(_, c, m)| (*c, *m))
                    .unwrap_or((0, 0));
                let removals = removals
                    .iter()
                    .filter(|r| r.worker_id == w.id)
                    .cloned()
                    .collect();
                Listed {
                    worker: w,
                    allocated_cpus: c,
                    allocated_memory_mib: m,
                    removals,
                }
            })
            .collect())
    }

    /// Asks a worker for its desired set again: bumps its version, and wakes
    /// it once this commits.
    pub async fn bump(&mut self, id: Uuid) -> Result<()> {
        self.signal(super::signals::Signal::Worker(id));
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query(
                    "UPDATE workers SET desired_version = desired_version + 1 WHERE id = $1",
                )
                .bind(id)
                .execute(&mut **tx)
                .await?;
            }
            Backend::Json(tx) => {
                for w in tx.work.workers.iter_mut().filter(|w| w.id == id) {
                    w.desired_version += 1;
                }
            }
        }
        Ok(())
    }

    /// Revokes a worker not yet revoked, giving its name.
    pub async fn revoke_worker(&mut self, id: Uuid, now: DateTime<Utc>) -> Result<Option<String>> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query_scalar(
                "UPDATE workers SET revoked_at = $2 WHERE id = $1 AND revoked_at IS NULL RETURNING name",
            )
            .bind(id)
            .bind(now)
            .fetch_optional(&mut **tx)
            .await?),
            Backend::Json(tx) => Ok(tx
                .work
                .workers
                .iter_mut()
                .find(|w| w.id == id && w.revoked_at.is_none())
                .map(|w| {
                    w.revoked_at = Some(now);
                    w.name.clone()
                })),
        }
    }

    /// Whether a worker is revoked, held until the transaction ends; None if
    /// there is no such worker.
    pub async fn worker_revoked(&mut self, id: Uuid) -> Result<Option<bool>> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query_scalar(
                "SELECT revoked_at IS NOT NULL FROM workers WHERE id = $1 FOR UPDATE",
            )
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?),
            Backend::Json(tx) => Ok(tx
                .work
                .workers
                .iter()
                .find(|w| w.id == id)
                .map(|w| w.revoked_at.is_some())),
        }
    }

    /// How many environments a worker holds.
    pub async fn held_environments(&mut self, id: Uuid) -> Result<i64> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query_scalar(
                "SELECT count(*) FROM environments WHERE worker_id = $1",
            )
            .bind(id)
            .fetch_one(&mut **tx)
            .await?),
            Backend::Json(tx) => Ok(tx
                .work
                .environments
                .iter()
                .filter(|e| e.worker_id == Some(id))
                .count() as i64),
        }
    }

    /// Deletes a worker, giving its name.
    pub async fn delete_worker(&mut self, id: Uuid) -> Result<Option<String>> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query_scalar(
                "DELETE FROM workers WHERE id = $1 RETURNING name",
            )
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?),
            Backend::Json(tx) => {
                let Some(at) = tx.work.workers.iter().position(|w| w.id == id) else {
                    return Ok(None);
                };
                tx.work.worker_image_removals.retain(|r| r.worker_id != id);
                tx.work
                    .worker_environment_removals
                    .retain(|r| r.worker_id != id);
                Ok(Some(tx.work.workers.remove(at).name))
            }
        }
    }

    /// The version of a worker's desired set.
    pub async fn desired_version(&mut self, id: Uuid) -> Result<Option<i64>> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query_scalar(
                "SELECT desired_version FROM workers WHERE id = $1",
            )
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?),
            Backend::Json(tx) => Ok(tx
                .work
                .workers
                .iter()
                .find(|w| w.id == id)
                .map(|w| w.desired_version)),
        }
    }

    /// What a worker is asked to run: its version, its environments by age,
    /// the image copies to delete, and the environments to forget.
    pub async fn desired(
        &mut self,
        id: Uuid,
    ) -> Result<Option<(i64, Vec<Environment>, Vec<ImageRemoval>, Vec<String>)>> {
        let Some(version) = self.desired_version(id).await? else {
            return Ok(None);
        };
        match &mut self.inner {
            Backend::Pg(tx) => {
                let envs = sqlx::query(&format!(
                    "SELECT {ENVIRONMENT_COLUMNS} FROM environments WHERE worker_id = $1 ORDER BY created_at"
                ))
                .bind(id)
                .fetch_all(&mut **tx)
                .await?
                .iter()
                .map(environment)
                .collect::<Result<_>>()?;
                let images = sqlx::query(
                    "SELECT worker_id, ref, digest FROM worker_image_removals WHERE worker_id = $1 ORDER BY ref, digest",
                )
                .bind(id)
                .fetch_all(&mut **tx)
                .await?
                .iter()
                .map(|r| {
                    Ok(ImageRemoval {
                        worker_id: r.try_get(0)?,
                        r#ref: r.try_get(1)?,
                        digest: r.try_get(2)?,
                    })
                })
                .collect::<Result<_>>()?;
                let forget = sqlx::query_scalar(
                    "SELECT environment FROM worker_environment_removals WHERE worker_id = $1 ORDER BY environment",
                )
                .bind(id)
                .fetch_all(&mut **tx)
                .await?;
                Ok(Some((version, envs, images, forget)))
            }
            Backend::Json(tx) => {
                let mut envs: Vec<Environment> = tx
                    .work
                    .environments
                    .iter()
                    .filter(|e| e.worker_id == Some(id))
                    .cloned()
                    .collect();
                envs.sort_by_key(|e| e.created_at);
                let mut images: Vec<ImageRemoval> = tx
                    .work
                    .worker_image_removals
                    .iter()
                    .filter(|r| r.worker_id == id)
                    .cloned()
                    .collect();
                images.sort_by(|a, b| (&a.r#ref, &a.digest).cmp(&(&b.r#ref, &b.digest)));
                let mut forget: Vec<String> = tx
                    .work
                    .worker_environment_removals
                    .iter()
                    .filter(|r| r.worker_id == id)
                    .map(|r| r.environment.clone())
                    .collect();
                forget.sort();
                Ok(Some((version, envs, images, forget)))
            }
        }
    }
}
