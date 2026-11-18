//! Packs, their copies, and what environments use of them, for both
//! backends: the rows as they are, which the packs module reads whole.

use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

use super::json::State;
use super::rows::{Conflict, Copy, EnvironmentCopy, Pack, PackDefault, PackPath};
use super::signals::Signal;
use super::{Backend, Result, Tx};

const PACK_COLUMNS: &str = "id, name, description, owner_id, team_id, builtin, attach, squash, \
     created_at, updated_at, same_name, merged";
const COPY_COLUMNS: &str = "id, pack_id, owner_id, team_id, personal, name, created_at, updated_at";

fn pack(r: &sqlx::postgres::PgRow) -> Result<Pack> {
    Ok(Pack {
        id: r.try_get("id")?,
        name: r.try_get("name")?,
        description: r.try_get("description")?,
        owner_id: r.try_get("owner_id")?,
        team_id: r.try_get("team_id")?,
        builtin: r.try_get("builtin")?,
        attach: r.try_get("attach")?,
        squash: r.try_get("squash")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
        same_name: r.try_get("same_name")?,
        merged: r.try_get("merged")?,
    })
}

fn copy(r: &sqlx::postgres::PgRow) -> Result<Copy> {
    Ok(Copy {
        id: r.try_get("id")?,
        pack_id: r.try_get("pack_id")?,
        owner_id: r.try_get("owner_id")?,
        team_id: r.try_get("team_id")?,
        personal: r.try_get("personal")?,
        name: r.try_get("name")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

/// Removes a copy from the JSON state, with what Postgres deletes with it.
pub(super) fn json_delete_copy(w: &mut State, id: Uuid) {
    w.copies.retain(|c| c.id != id);
    w.pack_defaults.retain(|d| d.copy_id != id);
    w.environment_copies.retain(|e| e.copy_id != id);
    w.environment_routed.retain(|r| r.copy_id != id);
    w.environment_conflicts.retain(|c| c.copy_id != id);
}

/// Removes a pack from the JSON state, with what Postgres deletes with it.
pub(super) fn json_delete_pack(w: &mut State, id: Uuid) {
    w.packs.retain(|k| k.id != id);
    w.pack_paths.retain(|p| p.pack_id != id);
    w.pack_defaults.retain(|d| d.pack_id != id);
    w.environment_copies.retain(|e| e.pack_id != id);
    let copies: Vec<Uuid> = w
        .copies
        .iter()
        .filter(|c| c.pack_id == id)
        .map(|c| c.id)
        .collect();
    for c in copies {
        json_delete_copy(w, c);
    }
}

/// Removes what a person owns of packs from the JSON state, as Postgres
/// does when they are deleted.
pub(super) fn json_delete_owner(w: &mut State, user: Uuid) {
    let packs: Vec<Uuid> = w
        .packs
        .iter()
        .filter(|k| k.owner_id == Some(user))
        .map(|k| k.id)
        .collect();
    for k in packs {
        json_delete_pack(w, k);
    }
    let copies: Vec<Uuid> = w
        .copies
        .iter()
        .filter(|c| c.owner_id == Some(user))
        .map(|c| c.id)
        .collect();
    for c in copies {
        json_delete_copy(w, c);
    }
    w.pack_defaults.retain(|d| d.user_id != user);
}

impl Tx {
    pub async fn packs(&mut self) -> Result<Vec<Pack>> {
        match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(&format!("SELECT {PACK_COLUMNS} FROM packs"))
                .fetch_all(&mut **tx)
                .await?
                .iter()
                .map(pack)
                .collect(),
            Backend::Json(tx) => Ok(tx.work.packs.clone()),
        }
    }

    pub async fn pack_paths(&mut self) -> Result<Vec<PackPath>> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query("SELECT pack_id, path, sensitive, send_every FROM pack_paths")
                    .fetch_all(&mut **tx)
                    .await?
                    .iter()
                    .map(|r| {
                        Ok(PackPath {
                            pack_id: r.try_get(0)?,
                            path: r.try_get(1)?,
                            sensitive: r.try_get(2)?,
                            send_every: r.try_get(3)?,
                        })
                    })
                    .collect()
            }
            Backend::Json(tx) => Ok(tx.work.pack_paths.clone()),
        }
    }

    pub async fn copies(&mut self) -> Result<Vec<Copy>> {
        match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(&format!("SELECT {COPY_COLUMNS} FROM copies"))
                .fetch_all(&mut **tx)
                .await?
                .iter()
                .map(copy)
                .collect(),
            Backend::Json(tx) => Ok(tx.work.copies.clone()),
        }
    }

    pub async fn pack_defaults(&mut self, user: Uuid) -> Result<Vec<PackDefault>> {
        match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(
                "SELECT user_id, pack_id, copy_id FROM pack_defaults WHERE user_id = $1",
            )
            .bind(user)
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(|r| {
                Ok(PackDefault {
                    user_id: r.try_get(0)?,
                    pack_id: r.try_get(1)?,
                    copy_id: r.try_get(2)?,
                })
            })
            .collect(),
            Backend::Json(tx) => Ok(tx
                .work
                .pack_defaults
                .iter()
                .filter(|d| d.user_id == user)
                .cloned()
                .collect()),
        }
    }

    pub async fn environment_copies(&mut self, env: Uuid) -> Result<Vec<EnvironmentCopy>> {
        match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(
                "SELECT environment_id, pack_id, copy_id, chosen FROM environment_copies WHERE environment_id = $1",
            )
            .bind(env)
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(|r| {
                Ok(EnvironmentCopy {
                    environment_id: r.try_get(0)?,
                    pack_id: r.try_get(1)?,
                    copy_id: r.try_get(2)?,
                    chosen: r.try_get(3)?,
                })
            })
            .collect(),
            Backend::Json(tx) => Ok(tx
                .work
                .environment_copies
                .iter()
                .filter(|e| e.environment_id == env)
                .cloned()
                .collect()),
        }
    }

    /// The copies an environment's agent routes to.
    pub async fn environment_routed(&mut self, env: Uuid) -> Result<Vec<Uuid>> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query_scalar(
                "SELECT copy_id FROM environment_routed WHERE environment_id = $1",
            )
            .bind(env)
            .fetch_all(&mut **tx)
            .await?),
            Backend::Json(tx) => Ok(tx
                .work
                .environment_routed
                .iter()
                .filter(|r| r.environment_id == env)
                .map(|r| r.copy_id)
                .collect()),
        }
    }

    /// An environment's conflicts, by path and copy.
    pub async fn conflicts(&mut self, env: Uuid) -> Result<Vec<Conflict>> {
        let mut out = match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(
                "SELECT environment_id, copy_id, path, found_at, resolution FROM environment_conflicts
                 WHERE environment_id = $1",
            )
            .bind(env)
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(|r| {
                Ok(Conflict {
                    environment_id: r.try_get(0)?,
                    copy_id: r.try_get(1)?,
                    path: r.try_get(2)?,
                    found_at: r.try_get(3)?,
                    resolution: r.try_get(4)?,
                })
            })
            .collect::<Result<Vec<_>>>()?,
            Backend::Json(tx) => tx
                .work
                .environment_conflicts
                .iter()
                .filter(|c| c.environment_id == env)
                .cloned()
                .collect(),
        };
        out.sort_by(|a, b| (&a.path, a.copy_id).cmp(&(&b.path, b.copy_id)));
        Ok(out)
    }

    pub async fn lock_pack(&mut self, id: Uuid) -> Result<()> {
        if let Backend::Pg(tx) = &mut self.inner {
            sqlx::query("SELECT 1 FROM packs WHERE id = $1 FOR UPDATE")
                .bind(id)
                .execute(&mut **tx)
                .await?;
        }
        Ok(())
    }

    /// Writes a pack, made or changed; false when its owner has another of
    /// the same name.
    pub async fn put_pack(&mut self, k: &Pack, new: bool) -> Result<bool> {
        self.signal(Signal::Records);
        let done = match &mut self.inner {
            Backend::Pg(tx) => {
                let q = if new {
                    "INSERT INTO packs (id, name, description, owner_id, team_id, builtin, attach, squash, created_at, updated_at, same_name, merged)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)"
                } else {
                    "UPDATE packs SET name = $2, description = $3, attach = $7, squash = $8, updated_at = $10,
                        same_name = $11, merged = $12
                     WHERE id = $1 AND (owner_id IS NOT DISTINCT FROM $4) AND (team_id IS NOT DISTINCT FROM $5)
                        AND builtin = $6 AND created_at = $9"
                };
                let r = sqlx::query(q)
                    .bind(k.id)
                    .bind(&k.name)
                    .bind(&k.description)
                    .bind(k.owner_id)
                    .bind(k.team_id)
                    .bind(k.builtin)
                    .bind(&k.attach)
                    .bind(&k.squash)
                    .bind(k.created_at)
                    .bind(k.updated_at)
                    .bind(k.same_name)
                    .bind(&k.merged)
                    .execute(&mut **tx)
                    .await;
                match r {
                    Ok(_) => true,
                    Err(sqlx::Error::Database(d)) if d.constraint() == Some("packs_name") => false,
                    Err(e) => return Err(e.into()),
                }
            }
            Backend::Json(tx) => {
                let w = &mut tx.work;
                let holder = |x: &Pack| x.owner_id.or(x.team_id);
                if w.packs.iter().any(|o| {
                    o.id != k.id
                        && holder(o) == holder(k)
                        && o.name.to_lowercase() == k.name.to_lowercase()
                }) {
                    false
                } else {
                    match w.packs.iter_mut().find(|o| o.id == k.id) {
                        Some(o) => *o = k.clone(),
                        None => w.packs.push(k.clone()),
                    }
                    true
                }
            }
        };
        if done {
            self.signal(Signal::Shared);
        }
        Ok(done)
    }

    pub async fn set_pack_paths(&mut self, pack: Uuid, paths: &[PackPath]) -> Result<()> {
        self.signal(Signal::Records);
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query("DELETE FROM pack_paths WHERE pack_id = $1")
                    .bind(pack)
                    .execute(&mut **tx)
                    .await?;
                let ps: Vec<&str> = paths.iter().map(|p| p.path.as_str()).collect();
                let ss: Vec<bool> = paths.iter().map(|p| p.sensitive).collect();
                let es: Vec<Option<i32>> = paths.iter().map(|p| p.send_every).collect();
                sqlx::query(
                    "INSERT INTO pack_paths (pack_id, path, sensitive, send_every)
                     SELECT $1, p, s, e FROM unnest($2::text[], $3::boolean[], $4::integer[]) AS x (p, s, e)",
                )
                .bind(pack)
                .bind(&ps)
                .bind(&ss)
                .bind(&es)
                .execute(&mut **tx)
                .await?;
            }
            Backend::Json(tx) => {
                tx.work.pack_paths.retain(|p| p.pack_id != pack);
                tx.work.pack_paths.extend(paths.iter().cloned());
            }
        }
        self.signal(Signal::Shared);
        Ok(())
    }

    pub async fn delete_pack(&mut self, id: Uuid) -> Result<()> {
        self.signal(Signal::Records);
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query("DELETE FROM packs WHERE id = $1")
                    .bind(id)
                    .execute(&mut **tx)
                    .await?;
            }
            Backend::Json(tx) => json_delete_pack(&mut tx.work, id),
        }
        self.signal(Signal::Shared);
        Ok(())
    }

    /// Writes a copy; false for a person's own of a pack they have one of
    /// already.
    pub async fn insert_copy(&mut self, c: &Copy) -> Result<bool> {
        if !c.personal {
            self.signal(Signal::Records);
        }
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query(
                "INSERT INTO copies (id, pack_id, owner_id, team_id, personal, name, created_at, updated_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8) ON CONFLICT DO NOTHING",
            )
            .bind(c.id)
            .bind(c.pack_id)
            .bind(c.owner_id)
            .bind(c.team_id)
            .bind(c.personal)
            .bind(&c.name)
            .bind(c.created_at)
            .bind(c.updated_at)
            .execute(&mut **tx)
            .await?
            .rows_affected()
                == 1),
            Backend::Json(tx) => {
                let w = &mut tx.work;
                if c.personal
                    && w.copies
                        .iter()
                        .any(|o| o.personal && o.pack_id == c.pack_id && o.owner_id == c.owner_id)
                {
                    return Ok(false);
                }
                w.copies.push(c.clone());
                Ok(true)
            }
        }
    }

    pub async fn rename_copy(&mut self, id: Uuid, name: &str, now: DateTime<Utc>) -> Result<()> {
        self.signal(Signal::Records);
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query("UPDATE copies SET name = $2, updated_at = $3 WHERE id = $1")
                    .bind(id)
                    .bind(name)
                    .bind(now)
                    .execute(&mut **tx)
                    .await?;
            }
            Backend::Json(tx) => {
                for c in tx.work.copies.iter_mut().filter(|c| c.id == id) {
                    c.name = name.to_string();
                    c.updated_at = now;
                }
            }
        }
        Ok(())
    }

    /// Moves a shared copy to another pack, as merging its pack into
    /// that one does.
    pub async fn set_copy_pack(&mut self, id: Uuid, pack: Uuid, now: DateTime<Utc>) -> Result<()> {
        self.signal(Signal::Records);
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query(
                    "UPDATE copies SET pack_id = $2, updated_at = $3 WHERE id = $1 AND NOT personal",
                )
                .bind(id)
                .bind(pack)
                .bind(now)
                .execute(&mut **tx)
                .await?;
            }
            Backend::Json(tx) => {
                for c in tx
                    .work
                    .copies
                    .iter_mut()
                    .filter(|c| c.id == id && !c.personal)
                {
                    c.pack_id = pack;
                    c.updated_at = now;
                }
            }
        }
        self.signal(Signal::Shared);
        Ok(())
    }

    pub async fn delete_copy(&mut self, id: Uuid) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query("DELETE FROM copies WHERE id = $1")
                    .bind(id)
                    .execute(&mut **tx)
                    .await?;
            }
            Backend::Json(tx) => json_delete_copy(&mut tx.work, id),
        }
        self.signal(Signal::Shared);
        self.signal(Signal::Records);
        Ok(())
    }

    /// Sets the copy a person uses of a pack by default, or with none
    /// returns them to their own.
    pub async fn set_pack_default(
        &mut self,
        user: Uuid,
        pack: Uuid,
        copy: Option<Uuid>,
    ) -> Result<()> {
        self.signal(Signal::Records);
        match &mut self.inner {
            Backend::Pg(tx) => {
                match copy {
                    Some(c) => sqlx::query(
                        "INSERT INTO pack_defaults (user_id, pack_id, copy_id) VALUES ($1, $2, $3)
                         ON CONFLICT (user_id, pack_id) DO UPDATE SET copy_id = EXCLUDED.copy_id",
                    )
                    .bind(user)
                    .bind(pack)
                    .bind(c),
                    None => {
                        sqlx::query("DELETE FROM pack_defaults WHERE user_id = $1 AND pack_id = $2")
                            .bind(user)
                            .bind(pack)
                    }
                }
                .execute(&mut **tx)
                .await?;
            }
            Backend::Json(tx) => {
                let w = &mut tx.work.pack_defaults;
                w.retain(|d| d.user_id != user || d.pack_id != pack);
                if let Some(c) = copy {
                    w.push(PackDefault {
                        user_id: user,
                        pack_id: pack,
                        copy_id: c,
                    });
                }
            }
        }
        Ok(())
    }

    /// Sets the copy an environment uses of a pack, or with none returns
    /// it to the rules.
    pub async fn set_environment_copy(
        &mut self,
        env: Uuid,
        pack: Uuid,
        copy: Option<(Uuid, bool)>,
    ) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                match copy {
                    Some((c, chosen)) => sqlx::query(
                        "INSERT INTO environment_copies (environment_id, pack_id, copy_id, chosen) VALUES ($1, $2, $3, $4)
                         ON CONFLICT (environment_id, pack_id) DO UPDATE SET copy_id = EXCLUDED.copy_id, chosen = EXCLUDED.chosen",
                    )
                    .bind(env)
                    .bind(pack)
                    .bind(c)
                    .bind(chosen),
                    None => sqlx::query(
                        "DELETE FROM environment_copies WHERE environment_id = $1 AND pack_id = $2",
                    )
                    .bind(env)
                    .bind(pack),
                }
                .execute(&mut **tx)
                .await?;
            }
            Backend::Json(tx) => {
                let w = &mut tx.work.environment_copies;
                w.retain(|e| e.environment_id != env || e.pack_id != pack);
                if let Some((c, chosen)) = copy {
                    w.push(EnvironmentCopy {
                        environment_id: env,
                        pack_id: pack,
                        copy_id: c,
                        chosen,
                    });
                }
            }
        }
        Ok(())
    }

    /// Records the copies an environment's agent routes to, those that
    /// exist.
    pub async fn set_routed(&mut self, env: Uuid, copies: &[Uuid]) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query("DELETE FROM environment_routed WHERE environment_id = $1")
                    .bind(env)
                    .execute(&mut **tx)
                    .await?;
                sqlx::query(
                    "INSERT INTO environment_routed (environment_id, copy_id)
                     SELECT $1, c.id FROM copies c WHERE c.id = ANY($2::uuid[]) ON CONFLICT DO NOTHING",
                )
                .bind(env)
                .bind(copies)
                .execute(&mut **tx)
                .await?;
            }
            Backend::Json(tx) => {
                let w = &mut tx.work;
                w.environment_routed.retain(|r| r.environment_id != env);
                for c in copies {
                    if w.copies.iter().any(|x| x.id == *c)
                        && !w
                            .environment_routed
                            .iter()
                            .any(|r| r.environment_id == env && r.copy_id == *c)
                    {
                        w.environment_routed.push(super::rows::EnvironmentRouted {
                            environment_id: env,
                            copy_id: *c,
                        });
                    }
                }
            }
        }
        Ok(())
    }

    /// Records a conflict an environment's agent found, for a copy that
    /// exists.
    pub async fn add_conflict(
        &mut self,
        env: Uuid,
        copy: Uuid,
        path: &str,
        now: DateTime<Utc>,
    ) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query(
                    "INSERT INTO environment_conflicts (environment_id, copy_id, path, found_at)
                     SELECT $1, id, $3, $4 FROM copies WHERE id = $2 ON CONFLICT DO NOTHING",
                )
                .bind(env)
                .bind(copy)
                .bind(path)
                .bind(now)
                .execute(&mut **tx)
                .await?;
            }
            Backend::Json(tx) => {
                let w = &mut tx.work;
                let known = w.copies.iter().any(|c| c.id == copy)
                    && w.environments.iter().any(|e| e.id == env);
                let had = w
                    .environment_conflicts
                    .iter()
                    .any(|c| c.environment_id == env && c.copy_id == copy && c.path == path);
                if known && !had {
                    w.environment_conflicts.push(Conflict {
                        environment_id: env,
                        copy_id: copy,
                        path: path.to_string(),
                        found_at: now,
                        resolution: None,
                    });
                }
            }
        }
        Ok(())
    }

    /// Sets how a conflict is resolved; false if there is no such conflict.
    pub async fn resolve_conflict(
        &mut self,
        env: Uuid,
        copy: Uuid,
        path: &str,
        resolution: &str,
    ) -> Result<bool> {
        let done = match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query(
                    "UPDATE environment_conflicts SET resolution = $4
                     WHERE environment_id = $1 AND copy_id = $2 AND path = $3",
                )
                .bind(env)
                .bind(copy)
                .bind(path)
                .bind(resolution)
                .execute(&mut **tx)
                .await?
                .rows_affected()
                    > 0
            }
            Backend::Json(tx) => {
                let mut done = false;
                for c in tx
                    .work
                    .environment_conflicts
                    .iter_mut()
                    .filter(|c| c.environment_id == env && c.copy_id == copy && c.path == path)
                {
                    c.resolution = Some(resolution.to_string());
                    done = true;
                }
                done
            }
        };
        if done {
            self.signal(Signal::Shared);
        }
        Ok(done)
    }

    pub async fn delete_conflict(&mut self, env: Uuid, copy: Uuid, path: &str) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query(
                    "DELETE FROM environment_conflicts WHERE environment_id = $1 AND copy_id = $2 AND path = $3",
                )
                .bind(env)
                .bind(copy)
                .bind(path)
                .execute(&mut **tx)
                .await?;
            }
            Backend::Json(tx) => tx
                .work
                .environment_conflicts
                .retain(|c| !(c.environment_id == env && c.copy_id == copy && c.path == path)),
        }
        Ok(())
    }
}
