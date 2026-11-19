//! Environments as their owners and placement change them, their tasks
//! and their coding agents' sessions: each read and write for both
//! backends.

use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

use super::rows::{AgentSession, Environment, Task, Worker};
use super::signals::Signal;
use super::workers::{
    ENVIRONMENT_COLUMNS, LITE_ENVIRONMENT_COLUMNS, WORKER_COLUMNS, environment, worker,
};
use super::{Backend, Result, Tx};

/// Which environments to read.
#[derive(Clone, Copy)]
pub enum Only {
    All,
    Owner(Uuid),
    Id(Uuid),
}

/// What became of an environment to be inserted.
#[derive(Debug, PartialEq, Eq)]
pub enum Inserted {
    Done,
    /// Another environment has its short ID.
    ShortIdTaken,
    /// Its owner has another of the same name, in any case.
    NameTaken,
}

/// What a worker offers and what it has left, not counting one
/// environment.
pub struct Room {
    pub name: String,
    pub free_cpus: i64,
    pub free_memory_mib: i64,
}

const OCCUPYING: &str =
    "(desired = 'running' OR phase IN ('starting', 'running', 'stopping', 'suspending'))";

const TASK_COLUMNS: &str = "id, environment_id, text, set_by, set_at, done_at";

const AGENT_COLUMNS: &str =
    "environment_id, id, agent, terminal, state, step, message, since, seen_at";

fn task(r: &impl super::Get) -> Result<Task> {
    Ok(Task {
        id: r.try_get("id")?,
        environment_id: r.try_get("environment_id")?,
        text: r.try_get("text")?,
        set_by: r.try_get("set_by")?,
        set_at: r.try_get("set_at")?,
        done_at: r.try_get("done_at")?,
    })
}

fn agent(r: &impl super::Get) -> Result<AgentSession> {
    Ok(AgentSession {
        environment_id: r.try_get("environment_id")?,
        id: r.try_get("id")?,
        agent: r.try_get("agent")?,
        terminal: r.try_get("terminal")?,
        state: r.try_get("state")?,
        step: r.try_get("step")?,
        message: r.try_get("message")?,
        since: r.try_get("since")?,
        seen_at: r.try_get("seen_at")?,
    })
}

impl Tx {
    /// The environments only names, oldest first.
    pub async fn environments(&mut self, only: Only) -> Result<Vec<Environment>> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                let (cond, id) = match only {
                    Only::All => ("true", None),
                    Only::Owner(id) => ("owner_id = $1", Some(id)),
                    Only::Id(id) => ("id = $1", Some(id)),
                };
                let q = format!(
                    "SELECT {ENVIRONMENT_COLUMNS} FROM environments WHERE {cond} ORDER BY created_at"
                );
                let mut q = sqlx::query(&q);
                if let Some(id) = id {
                    q = q.bind(id);
                }
                q.fetch_all(&mut **tx)
                    .await?
                    .iter()
                    .map(environment)
                    .collect()
            }
            Backend::Lite(tx) => {
                let (cond, id) = match only {
                    Only::All => ("true", None),
                    Only::Owner(id) => ("owner_id = $1", Some(id)),
                    Only::Id(id) => ("id = $1", Some(id)),
                };
                let q = format!(
                    "SELECT {LITE_ENVIRONMENT_COLUMNS} FROM environments WHERE {cond} ORDER BY created_at"
                );
                let mut q = sqlx::query(&q);
                if let Some(id) = id {
                    q = q.bind(id);
                }
                q.fetch_all(&mut **tx)
                    .await?
                    .iter()
                    .map(environment)
                    .collect()
            }
        }
    }

    /// Holds an environment's row until the transaction ends.
    pub async fn lock_environment(&mut self, id: Uuid) -> Result<()> {
        if let Backend::Pg(tx) = &mut self.inner {
            sqlx::query("SELECT 1 FROM environments WHERE id = $1 FOR UPDATE")
                .bind(id)
                .execute(&mut **tx)
                .await?;
        }
        Ok(())
    }

    pub async fn insert_environment(&mut self, e: &Environment) -> Result<Inserted> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                let r = sqlx::query(
                    "INSERT INTO environments (id, short_id, owner_id, name, template_id, template_name,
                        template_revision, spec, image, cpus, memory_mib, desired, phase, reason,
                        created_at, updated_at)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $15)
                     ON CONFLICT (short_id) DO NOTHING RETURNING id",
                )
                .bind(e.id)
                .bind(&e.short_id)
                .bind(e.owner_id)
                .bind(&e.name)
                .bind(e.template_id)
                .bind(&e.template_name)
                .bind(e.template_revision)
                .bind(&e.spec)
                .bind(&e.image)
                .bind(e.cpus)
                .bind(e.memory_mib)
                .bind(&e.desired)
                .bind(&e.phase)
                .bind(&e.reason)
                .bind(e.created_at)
                .fetch_optional(&mut **tx)
                .await;
                match r {
                    Ok(Some(_)) => {}
                    Ok(None) => return Ok(Inserted::ShortIdTaken),
                    Err(sqlx::Error::Database(d))
                        if d.constraint() == Some("environments_owner_name") =>
                    {
                        return Ok(Inserted::NameTaken);
                    }
                    Err(err) => return Err(err.into()),
                }
            }
            Backend::Lite(tx) => {
                let r = sqlx::query(
                    "INSERT INTO environments (id, short_id, owner_id, name, template_id, template_name,
                        template_revision, spec, image, cpus, memory_mib, desired, phase, reason,
                        created_at, updated_at)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $15)
                     ON CONFLICT (short_id) DO NOTHING RETURNING id",
                )
                .bind(e.id)
                .bind(&e.short_id)
                .bind(e.owner_id)
                .bind(&e.name)
                .bind(e.template_id)
                .bind(&e.template_name)
                .bind(e.template_revision)
                .bind(&e.spec)
                .bind(&e.image)
                .bind(e.cpus)
                .bind(e.memory_mib)
                .bind(&e.desired)
                .bind(&e.phase)
                .bind(&e.reason)
                .bind(e.created_at)
                .fetch_optional(&mut **tx)
                .await;
                match r {
                    Ok(Some(_)) => {}
                    Ok(None) => return Ok(Inserted::ShortIdTaken),
                    Err(err) if super::lite::violates(&err, "environments_owner_name", &[]) => {
                        return Ok(Inserted::NameTaken);
                    }
                    Err(err) => return Err(err.into()),
                }
            }
        }
        self.signal(Signal::Environment(e.id));
        Ok(Inserted::Done)
    }

    /// Writes what the server decides of an environment: its spec and
    /// size, what it should be doing, where it runs, the change to its
    /// image asked for, and whether its web servers are public. What its
    /// worker reports is left as it is.
    pub async fn save_environment(&mut self, e: &Environment) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query(
                    "UPDATE environments SET spec = $2, image = $3, cpus = $4, memory_mib = $5,
                        template_revision = $6, desired = $7, worker_id = $8, phase = $9, reason = $10,
                        image_pin_want = $11, image_drop_rollback = $12, ports_public = $13, updated_at = $14
                     WHERE id = $1",
                )
                .bind(e.id)
                .bind(&e.spec)
                .bind(&e.image)
                .bind(e.cpus)
                .bind(e.memory_mib)
                .bind(e.template_revision)
                .bind(&e.desired)
                .bind(e.worker_id)
                .bind(&e.phase)
                .bind(&e.reason)
                .bind(&e.image_pin_want)
                .bind(e.image_drop_rollback)
                .bind(e.ports_public)
                .bind(e.updated_at)
                .execute(&mut **tx)
                .await?;
            }
            Backend::Lite(tx) => {
                sqlx::query(
                    "UPDATE environments SET spec = $2, image = $3, cpus = $4, memory_mib = $5,
                        template_revision = $6, desired = $7, worker_id = $8, phase = $9, reason = $10,
                        image_pin_want = $11, image_drop_rollback = $12, ports_public = $13, updated_at = $14
                     WHERE id = $1",
                )
                .bind(e.id)
                .bind(&e.spec)
                .bind(&e.image)
                .bind(e.cpus)
                .bind(e.memory_mib)
                .bind(e.template_revision)
                .bind(&e.desired)
                .bind(e.worker_id)
                .bind(&e.phase)
                .bind(&e.reason)
                .bind(&e.image_pin_want)
                .bind(e.image_drop_rollback)
                .bind(e.ports_public)
                .bind(e.updated_at)
                .execute(&mut **tx)
                .await?;
            }
        }
        self.signal(Signal::Environment(e.id));
        Ok(())
    }

    pub async fn delete_environment(&mut self, id: Uuid) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query("DELETE FROM environments WHERE id = $1")
                    .bind(id)
                    .execute(&mut **tx)
                    .await?;
            }
            Backend::Lite(tx) => {
                sqlx::query("DELETE FROM environments WHERE id = $1")
                    .bind(id)
                    .execute(&mut **tx)
                    .await?;
            }
        }
        self.signal(Signal::Environment(id));
        Ok(())
    }

    /// The environments taking their workers' CPUs and memory: each one's
    /// worker, ID, CPUs and memory.
    pub async fn occupying(&mut self) -> Result<Vec<(Uuid, Uuid, i64, i64)>> {
        match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(&format!(
                "SELECT worker_id, id, cpus::bigint, memory_mib::bigint FROM environments
                 WHERE worker_id IS NOT NULL AND {OCCUPYING}"
            ))
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(|r| Ok((r.try_get(0)?, r.try_get(1)?, r.try_get(2)?, r.try_get(3)?)))
            .collect(),
            Backend::Lite(tx) => sqlx::query(&format!(
                "SELECT worker_id, id, cpus, memory_mib FROM environments
                 WHERE worker_id IS NOT NULL AND {OCCUPYING}"
            ))
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(|r| Ok((r.try_get(0)?, r.try_get(1)?, r.try_get(2)?, r.try_get(3)?)))
            .collect(),
        }
    }

    /// A worker's room, not counting the environment except, its row held
    /// until the transaction ends so no one else spends the same room.
    pub async fn room_for(&mut self, worker: Uuid, except: Uuid) -> Result<Option<Room>> {
        let row: Option<(String, i64, i64)> = match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(
                "SELECT name, cpus::bigint, memory_mib::bigint FROM workers WHERE id = $1 FOR UPDATE",
            )
            .bind(worker)
            .fetch_optional(&mut **tx)
            .await?
            .map(|r| Ok::<_, sqlx::Error>((r.try_get(0)?, r.try_get(1)?, r.try_get(2)?)))
            .transpose()?,
            Backend::Lite(tx) => sqlx::query(
                "SELECT name, cpus, memory_mib FROM workers WHERE id = $1",
            )
            .bind(worker)
            .fetch_optional(&mut **tx)
            .await?
            .map(|r| Ok::<_, sqlx::Error>((r.try_get(0)?, r.try_get(1)?, r.try_get(2)?)))
            .transpose()?,
        };
        let Some((name, cpus, mem)) = row else {
            return Ok(None);
        };
        let (used_cpus, used_mem) = self
            .occupying()
            .await?
            .iter()
            .filter(|(w, e, _, _)| *w == worker && *e != except)
            .fold((0, 0), |(c, m), (_, _, ec, em)| (c + ec, m + em));
        Ok(Some(Room {
            name,
            free_cpus: cpus - used_cpus,
            free_memory_mib: mem - used_mem,
        }))
    }

    /// The tasks not done of the environments envs.
    pub async fn current_tasks(&mut self, envs: &[Uuid]) -> Result<Vec<Task>> {
        match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(&format!(
                "SELECT {TASK_COLUMNS} FROM environment_tasks
                 WHERE environment_id = ANY($1) AND done_at IS NULL"
            ))
            .bind(envs)
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(task)
            .collect(),
            Backend::Lite(tx) => sqlx::query(&format!(
                "SELECT {TASK_COLUMNS} FROM environment_tasks
                 WHERE environment_id IN (SELECT unhex(replace(value, '-', '')) FROM json_each($1))
                    AND done_at IS NULL"
            ))
            .bind(sqlx::types::Json(envs))
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(task)
            .collect(),
        }
    }

    /// An environment's tasks, newest first: the one not done, then those
    /// done.
    pub async fn tasks_of(&mut self, env: Uuid) -> Result<Vec<Task>> {
        match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(&format!(
                "SELECT {TASK_COLUMNS} FROM environment_tasks WHERE environment_id = $1
                 ORDER BY set_at DESC, done_at DESC NULLS FIRST"
            ))
            .bind(env)
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(task)
            .collect(),
            Backend::Lite(tx) => sqlx::query(&format!(
                "SELECT {TASK_COLUMNS} FROM environment_tasks WHERE environment_id = $1
                 ORDER BY set_at DESC, done_at DESC NULLS FIRST"
            ))
            .bind(env)
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(task)
            .collect(),
        }
    }

    pub async fn insert_task(&mut self, t: &Task) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                hold_environment(tx, t.environment_id).await?;
                sqlx::query(
                    "INSERT INTO environment_tasks (id, environment_id, text, set_by, set_at)
                     VALUES ($1, $2, $3, $4, $5)",
                )
                .bind(t.id)
                .bind(t.environment_id)
                .bind(&t.text)
                .bind(&t.set_by)
                .bind(t.set_at)
                .execute(&mut **tx)
                .await?;
            }
            Backend::Lite(tx) => {
                sqlx::query(
                    "INSERT INTO environment_tasks (id, environment_id, text, set_by, set_at)
                     VALUES ($1, $2, $3, $4, $5)",
                )
                .bind(t.id)
                .bind(t.environment_id)
                .bind(&t.text)
                .bind(&t.set_by)
                .bind(t.set_at)
                .execute(&mut **tx)
                .await?;
            }
        }
        self.signal(Signal::Environment(t.environment_id));
        Ok(())
    }

    /// Marks an environment's task done, at now.
    pub async fn end_task(&mut self, env: Uuid, now: DateTime<Utc>) -> Result<()> {
        let ended = match &mut self.inner {
            Backend::Pg(tx) => {
                hold_environment(tx, env).await?;
                sqlx::query(
                    "UPDATE environment_tasks SET done_at = $2
                     WHERE environment_id = $1 AND done_at IS NULL",
                )
                .bind(env)
                .bind(now)
                .execute(&mut **tx)
                .await?
                .rows_affected()
                    > 0
            }
            Backend::Lite(tx) => {
                sqlx::query(
                    "UPDATE environment_tasks SET done_at = $2
                     WHERE environment_id = $1 AND done_at IS NULL",
                )
                .bind(env)
                .bind(now)
                .execute(&mut **tx)
                .await?
                .rows_affected()
                    > 0
            }
        };
        if ended {
            self.signal(Signal::Environment(env));
        }
        Ok(())
    }

    /// The coding agents' sessions in the environments envs, by when each
    /// took its state.
    pub async fn agent_sessions(&mut self, envs: &[Uuid]) -> Result<Vec<AgentSession>> {
        match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(&format!(
                "SELECT {AGENT_COLUMNS} FROM agent_sessions WHERE environment_id = ANY($1) ORDER BY since"
            ))
            .bind(envs)
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(agent)
            .collect(),
            Backend::Lite(tx) => sqlx::query(&format!(
                "SELECT {AGENT_COLUMNS} FROM agent_sessions WHERE environment_id IN (SELECT unhex(replace(value, '-', '')) FROM json_each($1))
                 ORDER BY since"
            ))
            .bind(sqlx::types::Json(envs))
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(agent)
            .collect()
        }
    }

    /// Keeps the coding agents' sessions an environment's guest reports,
    /// all of them: one left out is gone. Whether its owner has seen each
    /// is kept, and an unchanged one is left as it is, so it signals no
    /// change.
    pub async fn report_agents(&mut self, env: Uuid, sessions: &[AgentSession]) -> Result<()> {
        let ids: Vec<&str> = sessions.iter().map(|a| a.id.as_str()).collect();
        let mut changed = false;
        match &mut self.inner {
            Backend::Pg(tx) => {
                hold_environment(tx, env).await?;
                sqlx::query(
                    "DELETE FROM agent_sessions WHERE environment_id = $1 AND NOT (id = ANY($2))",
                )
                .bind(env)
                .bind(&ids)
                .execute(&mut **tx)
                .await?;
                for a in sessions {
                    sqlx::query(
                        "INSERT INTO agent_sessions AS a (environment_id, id, agent, terminal, state, step, message, since)
                         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                         ON CONFLICT (environment_id, id) DO UPDATE SET agent = $3, terminal = $4, state = $5, step = $6,
                            message = $7, since = $8
                         WHERE (a.agent, a.terminal, a.state, a.step, a.message, a.since)
                            IS DISTINCT FROM ($3, $4, $5, $6, $7, $8)",
                    )
                    .bind(env)
                    .bind(&a.id)
                    .bind(&a.agent)
                    .bind(&a.terminal)
                    .bind(&a.state)
                    .bind(&a.step)
                    .bind(&a.message)
                    .bind(a.since)
                    .execute(&mut **tx)
                    .await?;
                }
            }
            Backend::Lite(tx) => {
                changed |= sqlx::query(
                    "DELETE FROM agent_sessions WHERE environment_id = $1
                     AND id NOT IN (SELECT value FROM json_each($2))",
                )
                .bind(env)
                .bind(sqlx::types::Json(&ids))
                .execute(&mut **tx)
                .await?
                .rows_affected()
                    > 0;
                for a in sessions {
                    changed |= sqlx::query(
                        "INSERT INTO agent_sessions AS a (environment_id, id, agent, terminal, state, step, message, since)
                         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                         ON CONFLICT (environment_id, id) DO UPDATE SET agent = $3, terminal = $4, state = $5, step = $6,
                            message = $7, since = $8
                         WHERE a.agent IS NOT $3 OR a.terminal IS NOT $4 OR a.state IS NOT $5
                            OR a.step IS NOT $6 OR a.message IS NOT $7 OR a.since IS NOT $8",
                    )
                    .bind(env)
                    .bind(&a.id)
                    .bind(&a.agent)
                    .bind(&a.terminal)
                    .bind(&a.state)
                    .bind(&a.step)
                    .bind(&a.message)
                    .bind(a.since)
                    .execute(&mut **tx)
                    .await?
                    .rows_affected()
                        > 0;
                }
            }
        }
        if changed {
            self.signal(Signal::Environment(env));
        }
        Ok(())
    }

    /// Notes that an environment's owner has looked at one of its agents'
    /// sessions at now, or at the session's own time if later; false if
    /// there is no such session.
    pub async fn see_agent(&mut self, env: Uuid, id: &str, now: DateTime<Utc>) -> Result<bool> {
        let seen = match &mut self.inner {
            Backend::Pg(tx) => {
                hold_environment(tx, env).await?;
                sqlx::query(
                    "UPDATE agent_sessions SET seen_at = greatest($3, since)
                     WHERE environment_id = $1 AND id = $2",
                )
                .bind(env)
                .bind(id)
                .bind(now)
                .execute(&mut **tx)
                .await?
                .rows_affected()
                    > 0
            }
            Backend::Lite(tx) => {
                sqlx::query(
                    "UPDATE agent_sessions SET seen_at = max($3, since)
                     WHERE environment_id = $1 AND id = $2",
                )
                .bind(env)
                .bind(id)
                .bind(now)
                .execute(&mut **tx)
                .await?
                .rows_affected()
                    > 0
            }
        };
        if seen {
            self.signal(Signal::Environment(env));
        }
        Ok(seen)
    }

    /// Environments that want to run and have no worker, oldest first, at
    /// most limit, held until the transaction ends; with Postgres, those
    /// another server holds are left to it.
    pub async fn placement_queue(&mut self, limit: i64) -> Result<Vec<Environment>> {
        match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(&format!(
                "SELECT {ENVIRONMENT_COLUMNS} FROM environments
                 WHERE worker_id IS NULL AND desired = 'running'
                 ORDER BY created_at LIMIT $1 FOR UPDATE SKIP LOCKED"
            ))
            .bind(limit)
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(environment)
            .collect(),
            Backend::Lite(tx) => sqlx::query(&format!(
                "SELECT {LITE_ENVIRONMENT_COLUMNS} FROM environments
                 WHERE worker_id IS NULL AND desired = 'running'
                 ORDER BY created_at LIMIT $1"
            ))
            .bind(limit)
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(environment)
            .collect(),
        }
    }

    /// The workers seen since and not revoked, by ID, their rows held until
    /// the transaction ends.
    pub async fn lock_online_workers(&mut self, since: DateTime<Utc>) -> Result<Vec<Worker>> {
        match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(&format!(
                "SELECT {WORKER_COLUMNS} FROM workers
                 WHERE revoked_at IS NULL AND last_seen_at > $1 ORDER BY id FOR UPDATE"
            ))
            .bind(since)
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(worker)
            .collect(),
            Backend::Lite(tx) => sqlx::query(&format!(
                "SELECT {WORKER_COLUMNS} FROM workers
                 WHERE revoked_at IS NULL AND last_seen_at > $1 ORDER BY id"
            ))
            .bind(since)
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(worker)
            .collect(),
        }
    }
}

/// Holds an environment's row until the transaction ends. A change to its
/// task or agents raises its revision (migration 0020's trigger), so each
/// writer of them holds the environment first, as everything else that
/// changes the two does, and no two take them in opposite orders.
async fn hold_environment(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    env: Uuid,
) -> Result<()> {
    sqlx::query("SELECT 1 FROM environments WHERE id = $1 FOR UPDATE")
        .bind(env)
        .execute(&mut **tx)
        .await?;
    Ok(())
}
