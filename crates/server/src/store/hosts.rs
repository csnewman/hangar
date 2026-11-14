//! Sessions on environments' own hosts (`editor_sessions`): tickets a
//! Hangar session is given, and the cookies they are redeemed for, for
//! both backends.

use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

use super::rows::{Environment, HostSession};
use super::workers::{ENVIRONMENT_COLUMNS, environment};
use super::{Backend, Result, Tx};

impl Tx {
    pub async fn insert_host_session(&mut self, s: &HostSession) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query(
                    "INSERT INTO editor_sessions (token_hash, session_hash, environment_id, ticket, expires_at)
                     VALUES ($1, $2, $3, $4, $5)",
                )
                .bind(&s.token_hash)
                .bind(&s.session_hash)
                .bind(s.environment_id)
                .bind(s.ticket)
                .bind(s.expires_at)
                .execute(&mut **tx)
                .await?;
            }
            Backend::Json(tx) => tx.work.editor_sessions.push(s.clone()),
        }
        Ok(())
    }

    /// Takes an unexpired ticket for env, giving the Hangar session it was
    /// issued to, whose earlier cookies for env go with it.
    pub async fn redeem_host_ticket(
        &mut self,
        hash: &[u8],
        env: Uuid,
        now: DateTime<Utc>,
    ) -> Result<Option<Vec<u8>>> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                let session: Option<Vec<u8>> = sqlx::query_scalar(
                    "DELETE FROM editor_sessions
                     WHERE token_hash = $1 AND ticket AND environment_id = $2 AND expires_at > $3
                     RETURNING session_hash",
                )
                .bind(hash)
                .bind(env)
                .bind(now)
                .fetch_optional(&mut **tx)
                .await?;
                if let Some(s) = &session {
                    sqlx::query(
                        "DELETE FROM editor_sessions
                         WHERE session_hash = $1 AND environment_id = $2 AND NOT ticket",
                    )
                    .bind(s)
                    .bind(env)
                    .execute(&mut **tx)
                    .await?;
                }
                Ok(session)
            }
            Backend::Json(tx) => {
                let w = &mut tx.work.editor_sessions;
                let Some(at) = w.iter().position(|s| {
                    s.token_hash == hash
                        && s.ticket
                        && s.environment_id == env
                        && s.expires_at > now
                }) else {
                    return Ok(None);
                };
                let session = w.remove(at).session_hash;
                w.retain(|s| s.session_hash != session || s.environment_id != env || s.ticket);
                Ok(Some(session))
            }
        }
    }

    /// The environment an unexpired cookie for env reaches, with when the
    /// cookie expires, while the Hangar session behind it lasts and its user
    /// is not disabled.
    pub async fn host_cookie(
        &mut self,
        hash: &[u8],
        env: Uuid,
        now: DateTime<Utc>,
    ) -> Result<Option<(Environment, DateTime<Utc>)>> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                let cols = ENVIRONMENT_COLUMNS
                    .split(", ")
                    .map(|c| format!("e.{}", c.trim()))
                    .collect::<Vec<_>>()
                    .join(", ");
                let row = sqlx::query(&format!(
                    "SELECT {cols}, es.expires_at AS cookie_expires
                     FROM editor_sessions es
                     JOIN sessions s ON s.token_hash = es.session_hash
                     JOIN users u ON u.id = s.user_id
                     JOIN environments e ON e.id = es.environment_id
                     WHERE es.token_hash = $1 AND NOT es.ticket AND es.environment_id = $2
                        AND es.expires_at > $3 AND s.expires_at > $3 AND u.disabled_at IS NULL"
                ))
                .bind(hash)
                .bind(env)
                .bind(now)
                .fetch_optional(&mut **tx)
                .await?;
                row.map(|r| Ok((environment(&r)?, r.try_get("cookie_expires")?)))
                    .transpose()
            }
            Backend::Json(tx) => {
                let w = &tx.work;
                let Some(c) = w.editor_sessions.iter().find(|s| {
                    s.token_hash == hash
                        && !s.ticket
                        && s.environment_id == env
                        && s.expires_at > now
                }) else {
                    return Ok(None);
                };
                let live = w
                    .sessions
                    .iter()
                    .find(|s| s.token_hash == c.session_hash && s.expires_at > now)
                    .and_then(|s| w.users.iter().find(|u| u.id == s.user_id))
                    .is_some_and(|u| u.disabled_at.is_none());
                if !live {
                    return Ok(None);
                }
                Ok(w.environments
                    .iter()
                    .find(|e| e.id == env)
                    .map(|e| (e.clone(), c.expires_at)))
            }
        }
    }

    /// Pushes back when a cookie expires.
    pub async fn extend_host_cookie(&mut self, hash: &[u8], until: DateTime<Utc>) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query("UPDATE editor_sessions SET expires_at = $2 WHERE token_hash = $1")
                    .bind(hash)
                    .bind(until)
                    .execute(&mut **tx)
                    .await?;
            }
            Backend::Json(tx) => {
                for s in tx
                    .work
                    .editor_sessions
                    .iter_mut()
                    .filter(|s| s.token_hash == hash)
                {
                    s.expires_at = until;
                }
            }
        }
        Ok(())
    }

    /// The environment with a short ID, whatever it is doing.
    pub async fn environment_by_short_id(&mut self, short: &str) -> Result<Option<Environment>> {
        match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(&format!(
                "SELECT {ENVIRONMENT_COLUMNS} FROM environments WHERE short_id = $1"
            ))
            .bind(short)
            .fetch_optional(&mut **tx)
            .await?
            .map(|r| environment(&r))
            .transpose(),
            Backend::Json(tx) => Ok(tx
                .work
                .environments
                .iter()
                .find(|e| e.short_id == short)
                .cloned()),
        }
    }
}
