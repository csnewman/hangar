//! Apps' sign-in grants (`sign_in_grants`), for both backends.

use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

use super::rows::SignInGrant;
use super::{Backend, Result, Tx};

const COLUMNS: &str =
    "id, device_hash, user_code, name, created_at, expires_at, approved_by, denied";

fn grant(r: &impl super::Get) -> Result<SignInGrant> {
    Ok(SignInGrant {
        id: r.try_get("id")?,
        device_hash: r.try_get("device_hash")?,
        user_code: r.try_get("user_code")?,
        name: r.try_get("name")?,
        created_at: r.try_get("created_at")?,
        expires_at: r.try_get("expires_at")?,
        approved_by: r.try_get("approved_by")?,
        denied: r.try_get("denied")?,
    })
}

impl Tx {
    /// Deletes the grants that have expired.
    pub async fn prune_sign_in_grants(&mut self, now: DateTime<Utc>) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query("DELETE FROM sign_in_grants WHERE expires_at < $1")
                    .bind(now)
                    .execute(&mut **tx)
                    .await?;
            }
            Backend::Lite(tx) => {
                sqlx::query("DELETE FROM sign_in_grants WHERE expires_at < $1")
                    .bind(now)
                    .execute(&mut **tx)
                    .await?;
            }
        }
        Ok(())
    }

    /// Adds a grant, unless its user code is taken, saying whether it was.
    pub async fn insert_sign_in_grant(&mut self, g: &SignInGrant) -> Result<bool> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query(
                "INSERT INTO sign_in_grants (id, device_hash, user_code, name, created_at, expires_at)
                 VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (user_code) DO NOTHING",
            )
            .bind(g.id)
            .bind(&g.device_hash)
            .bind(&g.user_code)
            .bind(&g.name)
            .bind(g.created_at)
            .bind(g.expires_at)
            .execute(&mut **tx)
            .await?
            .rows_affected()
                == 1),
            Backend::Lite(tx) => Ok(sqlx::query(
                "INSERT INTO sign_in_grants (id, device_hash, user_code, name, created_at, expires_at)
                 VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (user_code) DO NOTHING",
            )
            .bind(g.id)
            .bind(&g.device_hash)
            .bind(&g.user_code)
            .bind(&g.name)
            .bind(g.created_at)
            .bind(g.expires_at)
            .execute(&mut **tx)
            .await?
            .rows_affected()
                == 1)
        }
    }

    /// The grant waiting under a user code: not yet decided, nor expired.
    pub async fn waiting_sign_in_grant(
        &mut self,
        code: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<SignInGrant>> {
        match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(&format!(
                "SELECT {COLUMNS} FROM sign_in_grants
                 WHERE user_code = $1 AND expires_at > $2 AND approved_by IS NULL AND NOT denied"
            ))
            .bind(code)
            .bind(now)
            .fetch_optional(&mut **tx)
            .await?
            .map(|r| grant(&r))
            .transpose(),
            Backend::Lite(tx) => sqlx::query(&format!(
                "SELECT {COLUMNS} FROM sign_in_grants
                 WHERE user_code = $1 AND expires_at > $2 AND approved_by IS NULL AND NOT denied"
            ))
            .bind(code)
            .bind(now)
            .fetch_optional(&mut **tx)
            .await?
            .map(|r| grant(&r))
            .transpose(),
        }
    }

    /// Approves, as user, or refuses the grant waiting under a user code,
    /// giving its ID and name.
    pub async fn decide_sign_in_grant(
        &mut self,
        code: &str,
        user: Uuid,
        refuse: bool,
        now: DateTime<Utc>,
    ) -> Result<Option<(Uuid, String)>> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                let row = sqlx::query(
                    "UPDATE sign_in_grants
                     SET approved_by = CASE WHEN $3 THEN NULL ELSE $2::uuid END, denied = $3
                     WHERE user_code = $1 AND expires_at > $4 AND approved_by IS NULL AND NOT denied
                     RETURNING id, name",
                )
                .bind(code)
                .bind(user)
                .bind(refuse)
                .bind(now)
                .fetch_optional(&mut **tx)
                .await?;
                row.map(|r| Ok((r.try_get(0)?, r.try_get(1)?))).transpose()
            }
            Backend::Lite(tx) => {
                let row = sqlx::query(
                    "UPDATE sign_in_grants
                     SET approved_by = CASE WHEN $3 THEN NULL ELSE $2 END, denied = $3
                     WHERE user_code = $1 AND expires_at > $4 AND approved_by IS NULL AND NOT denied
                     RETURNING id, name",
                )
                .bind(code)
                .bind(user)
                .bind(refuse)
                .bind(now)
                .fetch_optional(&mut **tx)
                .await?;
                row.map(|r| Ok((r.try_get(0)?, r.try_get(1)?))).transpose()
            }
        }
    }

    /// The unexpired grant whose device code hashes to hash, held for the
    /// rest of the transaction.
    pub async fn sign_in_grant_for_device(
        &mut self,
        hash: &[u8],
        now: DateTime<Utc>,
    ) -> Result<Option<SignInGrant>> {
        match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(&format!(
                "SELECT {COLUMNS} FROM sign_in_grants
                 WHERE device_hash = $1 AND expires_at > $2 FOR UPDATE"
            ))
            .bind(hash)
            .bind(now)
            .fetch_optional(&mut **tx)
            .await?
            .map(|r| grant(&r))
            .transpose(),
            Backend::Lite(tx) => sqlx::query(&format!(
                "SELECT {COLUMNS} FROM sign_in_grants
                 WHERE device_hash = $1 AND expires_at > $2"
            ))
            .bind(hash)
            .bind(now)
            .fetch_optional(&mut **tx)
            .await?
            .map(|r| grant(&r))
            .transpose(),
        }
    }

    pub async fn delete_sign_in_grant(&mut self, id: Uuid) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query("DELETE FROM sign_in_grants WHERE id = $1")
                    .bind(id)
                    .execute(&mut **tx)
                    .await?;
            }
            Backend::Lite(tx) => {
                sqlx::query("DELETE FROM sign_in_grants WHERE id = $1")
                    .bind(id)
                    .execute(&mut **tx)
                    .await?;
            }
        }
        Ok(())
    }
}
