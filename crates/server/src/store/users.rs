//! Users, their sessions and access tokens, and the audit log: each read
//! and write for both backends.

use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

use super::rows::{AccessToken, AuditEvent, Session, User};
use super::{Backend, Result, Tx};

const USER_COLUMNS: &str =
    "id, username, display_name, password_hash, is_admin, kind, disabled_at, created_at, theme";

fn user(r: &impl super::Get) -> Result<User> {
    Ok(User {
        id: r.try_get("id")?,
        username: r.try_get("username")?,
        display_name: r.try_get("display_name")?,
        password_hash: r.try_get("password_hash")?,
        is_admin: r.try_get("is_admin")?,
        kind: r.try_get("kind")?,
        disabled_at: r.try_get("disabled_at")?,
        created_at: r.try_get("created_at")?,
        theme: r.try_get("theme")?,
    })
}

fn token(r: &impl super::Get) -> Result<AccessToken> {
    Ok(AccessToken {
        id: r.try_get("id")?,
        user_id: r.try_get("user_id")?,
        name: r.try_get("name")?,
        token_hash: r.try_get("token_hash")?,
        created_at: r.try_get("created_at")?,
        last_used_at: r.try_get("last_used_at")?,
        expires_at: r.try_get("expires_at")?,
    })
}

impl Tx {
    /// Every person, by name, ignoring case.
    pub async fn people(&mut self) -> Result<Vec<User>> {
        match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(&format!(
                "SELECT {USER_COLUMNS} FROM users WHERE kind = 'person' ORDER BY lower(username)"
            ))
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(user)
            .collect(),
            Backend::Lite(tx) => sqlx::query(&format!(
                "SELECT {USER_COLUMNS} FROM users WHERE kind = 'person' ORDER BY lower(username)"
            ))
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(user)
            .collect(),
        }
    }

    /// Holds every active administrator's row until the transaction ends, so
    /// two changes cannot each leave the other the last.
    pub async fn lock_admins(&mut self) -> Result<()> {
        if let Backend::Pg(tx) = &mut self.inner {
            sqlx::query(
                "SELECT 1 FROM users WHERE is_admin AND disabled_at IS NULL ORDER BY id FOR UPDATE",
            )
            .execute(&mut **tx)
            .await?;
        }
        Ok(())
    }

    /// How many administrators are not disabled.
    pub async fn active_admins(&mut self) -> Result<i64> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query_scalar(
                "SELECT count(*) FROM users WHERE is_admin AND disabled_at IS NULL",
            )
            .fetch_one(&mut **tx)
            .await?),
            Backend::Lite(tx) => Ok(sqlx::query_scalar(
                "SELECT count(*) FROM users WHERE is_admin AND disabled_at IS NULL",
            )
            .fetch_one(&mut **tx)
            .await?),
        }
    }

    /// Whether a team has the slug name, ignoring case.
    pub async fn team_slug_taken(&mut self, name: &str) -> Result<bool> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM teams WHERE lower(slug) = lower($1))",
            )
            .bind(name)
            .fetch_one(&mut **tx)
            .await?),
            Backend::Lite(tx) => Ok(sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM teams WHERE lower(slug) = lower($1))",
            )
            .bind(name)
            .fetch_one(&mut **tx)
            .await?),
        }
    }

    /// How many environments a user owns.
    pub async fn owned_environments(&mut self, user_id: Uuid) -> Result<i64> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query_scalar(
                "SELECT count(*) FROM environments WHERE owner_id = $1",
            )
            .bind(user_id)
            .fetch_one(&mut **tx)
            .await?),
            Backend::Lite(tx) => Ok(sqlx::query_scalar(
                "SELECT count(*) FROM environments WHERE owner_id = $1",
            )
            .bind(user_id)
            .fetch_one(&mut **tx)
            .await?),
        }
    }

    /// How many image repositories a user owns.
    pub async fn owned_image_repositories(&mut self, user_id: Uuid) -> Result<i64> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query_scalar(
                "SELECT count(*) FROM image_repositories WHERE owner_id = $1",
            )
            .bind(user_id)
            .fetch_one(&mut **tx)
            .await?),
            Backend::Lite(tx) => Ok(sqlx::query_scalar(
                "SELECT count(*) FROM image_repositories WHERE owner_id = $1",
            )
            .bind(user_id)
            .fetch_one(&mut **tx)
            .await?),
        }
    }

    /// Changes a person: each field given, the rest as they are; disabled
    /// keeps the time they were first disabled. None if there is no such
    /// person.
    pub async fn update_user(
        &mut self,
        id: Uuid,
        display_name: Option<&str>,
        admin: Option<bool>,
        disabled: Option<bool>,
        password_hash: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<Option<User>> {
        match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(&format!(
                "UPDATE users SET
                    display_name = coalesce($2, display_name),
                    is_admin = coalesce($3, is_admin),
                    disabled_at = CASE WHEN $4::boolean IS NULL THEN disabled_at
                                       WHEN $4 THEN coalesce(disabled_at, $6)
                                       ELSE NULL END,
                    password_hash = coalesce($5, password_hash)
                 WHERE id = $1 AND kind = 'person'
                 RETURNING {USER_COLUMNS}"
            ))
            .bind(id)
            .bind(display_name)
            .bind(admin)
            .bind(disabled)
            .bind(password_hash)
            .bind(now)
            .fetch_optional(&mut **tx)
            .await?
            .map(|r| user(&r))
            .transpose(),
            Backend::Lite(tx) => sqlx::query(&format!(
                "UPDATE users SET
                    display_name = coalesce($2, display_name),
                    is_admin = coalesce($3, is_admin),
                    disabled_at = CASE WHEN $4 IS NULL THEN disabled_at
                                       WHEN $4 THEN coalesce(disabled_at, $6)
                                       ELSE NULL END,
                    password_hash = coalesce($5, password_hash)
                 WHERE id = $1 AND kind = 'person'
                 RETURNING {USER_COLUMNS}"
            ))
            .bind(id)
            .bind(display_name)
            .bind(admin)
            .bind(disabled)
            .bind(password_hash)
            .bind(now)
            .fetch_optional(&mut **tx)
            .await?
            .map(|r| user(&r))
            .transpose(),
        }
    }

    /// Deletes a person, and with them their sessions and tokens; gives
    /// their name, or None if there is no such person.
    pub async fn delete_user(&mut self, id: Uuid) -> Result<Option<String>> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query_scalar(
                "DELETE FROM users WHERE id = $1 AND kind = 'person' RETURNING username",
            )
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?),
            Backend::Lite(tx) => Ok(sqlx::query_scalar(
                "DELETE FROM users WHERE id = $1 AND kind = 'person' RETURNING username",
            )
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?),
        }
    }

    /// Ends every session of a user's but the one whose token hashes to
    /// keep.
    pub async fn delete_sessions(&mut self, user_id: Uuid, keep: Option<&[u8]>) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query(
                    "DELETE FROM sessions WHERE user_id = $1 AND token_hash IS DISTINCT FROM $2",
                )
                .bind(user_id)
                .bind(keep)
                .execute(&mut **tx)
                .await?;
            }
            Backend::Lite(tx) => {
                sqlx::query(
                    "DELETE FROM sessions WHERE user_id = $1 AND token_hash IS DISTINCT FROM $2",
                )
                .bind(user_id)
                .bind(keep)
                .execute(&mut **tx)
                .await?;
            }
        }
        Ok(())
    }

    /// The person named name, ignoring case.
    pub async fn person_by_name(&mut self, name: &str) -> Result<Option<User>> {
        match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(&format!(
                "SELECT {USER_COLUMNS} FROM users WHERE lower(username) = lower($1) AND kind = 'person'"
            ))
            .bind(name)
            .fetch_optional(&mut **tx)
            .await?
            .map(|r| user(&r))
            .transpose(),
            Backend::Lite(tx) => sqlx::query(&format!(
                "SELECT {USER_COLUMNS} FROM users WHERE lower(username) = lower($1) AND kind = 'person'"
            ))
            .bind(name)
            .fetch_optional(&mut **tx)
            .await?
            .map(|r| user(&r))
            .transpose(),
        }
    }

    /// Sets a user's theme, saying whether there is such a user.
    pub async fn set_theme(&mut self, id: Uuid, theme: &str) -> Result<bool> {
        self.signal(super::signals::Signal::Records);
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query("UPDATE users SET theme = $2 WHERE id = $1")
                .bind(id)
                .bind(theme)
                .execute(&mut **tx)
                .await?
                .rows_affected()
                == 1),
            Backend::Lite(tx) => Ok(sqlx::query("UPDATE users SET theme = $2 WHERE id = $1")
                .bind(id)
                .bind(theme)
                .execute(&mut **tx)
                .await?
                .rows_affected()
                == 1),
        }
    }

    /// The user with id.
    pub async fn user(&mut self, id: Uuid) -> Result<Option<User>> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query(&format!("SELECT {USER_COLUMNS} FROM users WHERE id = $1"))
                    .bind(id)
                    .fetch_optional(&mut **tx)
                    .await?
                    .map(|r| user(&r))
                    .transpose()
            }
            Backend::Lite(tx) => {
                sqlx::query(&format!("SELECT {USER_COLUMNS} FROM users WHERE id = $1"))
                    .bind(id)
                    .fetch_optional(&mut **tx)
                    .await?
                    .map(|r| user(&r))
                    .transpose()
            }
        }
    }

    pub async fn insert_user(&mut self, u: &User) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query(&format!(
                    "INSERT INTO users ({USER_COLUMNS}) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"
                ))
                .bind(u.id)
                .bind(&u.username)
                .bind(&u.display_name)
                .bind(&u.password_hash)
                .bind(u.is_admin)
                .bind(&u.kind)
                .bind(u.disabled_at)
                .bind(u.created_at)
                .bind(&u.theme)
                .execute(&mut **tx)
                .await?;
            }
            Backend::Lite(tx) => {
                sqlx::query(&format!(
                    "INSERT INTO users ({USER_COLUMNS}) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"
                ))
                .bind(u.id)
                .bind(&u.username)
                .bind(&u.display_name)
                .bind(&u.password_hash)
                .bind(u.is_admin)
                .bind(&u.kind)
                .bind(u.disabled_at)
                .bind(u.created_at)
                .bind(&u.theme)
                .execute(&mut **tx)
                .await?;
            }
        }
        Ok(())
    }

    pub async fn insert_session(&mut self, s: &Session) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query(
                    "INSERT INTO sessions (token_hash, user_id, created_at, last_seen_at, expires_at)
                     VALUES ($1, $2, $3, $4, $5)",
                )
                .bind(&s.token_hash)
                .bind(s.user_id)
                .bind(s.created_at)
                .bind(s.last_seen_at)
                .bind(s.expires_at)
                .execute(&mut **tx)
                .await?;
            }
            Backend::Lite(tx) => {
                sqlx::query(
                    "INSERT INTO sessions (token_hash, user_id, created_at, last_seen_at, expires_at)
                     VALUES ($1, $2, $3, $4, $5)",
                )
                .bind(&s.token_hash)
                .bind(s.user_id)
                .bind(s.created_at)
                .bind(s.last_seen_at)
                .bind(s.expires_at)
                .execute(&mut **tx)
                .await?;
            }
        }
        Ok(())
    }

    /// The session whose token hashes to hash, and its user, while it has
    /// not expired and its user is not disabled.
    pub async fn session(
        &mut self,
        hash: &[u8],
        now: DateTime<Utc>,
    ) -> Result<Option<(User, Session)>> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                let Some(r) = sqlx::query(&format!(
                    "SELECT {}, s.token_hash, s.user_id, s.created_at AS s_created_at, s.last_seen_at, s.expires_at
                     FROM sessions s JOIN users u ON u.id = s.user_id
                     WHERE s.token_hash = $1 AND s.expires_at > $2 AND u.disabled_at IS NULL",
                    USER_COLUMNS
                        .split(", ")
                        .map(|c| format!("u.{c}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
                .bind(hash)
                .bind(now)
                .fetch_optional(&mut **tx)
                .await?
                else {
                    return Ok(None);
                };
                let s = Session {
                    token_hash: r.try_get("token_hash")?,
                    user_id: r.try_get("user_id")?,
                    created_at: r.try_get("s_created_at")?,
                    last_seen_at: r.try_get("last_seen_at")?,
                    expires_at: r.try_get("expires_at")?,
                };
                Ok(Some((user(&r)?, s)))
            }
            Backend::Lite(tx) => {
                let Some(r) = sqlx::query(&format!(
                    "SELECT {}, s.token_hash, s.user_id, s.created_at AS s_created_at, s.last_seen_at, s.expires_at
                     FROM sessions s JOIN users u ON u.id = s.user_id
                     WHERE s.token_hash = $1 AND s.expires_at > $2 AND u.disabled_at IS NULL",
                    USER_COLUMNS
                        .split(", ")
                        .map(|c| format!("u.{c}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
                .bind(hash)
                .bind(now)
                .fetch_optional(&mut **tx)
                .await?
                else {
                    return Ok(None);
                };
                let s = Session {
                    token_hash: r.try_get("token_hash")?,
                    user_id: r.try_get("user_id")?,
                    created_at: r.try_get("s_created_at")?,
                    last_seen_at: r.try_get("last_seen_at")?,
                    expires_at: r.try_get("expires_at")?,
                };
                Ok(Some((user(&r)?, s)))
            }
        }
    }

    /// Marks a session seen at `now`, lasting until `expires`.
    pub async fn touch_session(
        &mut self,
        hash: &[u8],
        now: DateTime<Utc>,
        expires: DateTime<Utc>,
    ) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query(
                    "UPDATE sessions SET last_seen_at = $2, expires_at = $3 WHERE token_hash = $1",
                )
                .bind(hash)
                .bind(now)
                .bind(expires)
                .execute(&mut **tx)
                .await?;
            }
            Backend::Lite(tx) => {
                sqlx::query(
                    "UPDATE sessions SET last_seen_at = $2, expires_at = $3 WHERE token_hash = $1",
                )
                .bind(hash)
                .bind(now)
                .bind(expires)
                .execute(&mut **tx)
                .await?;
            }
        }
        Ok(())
    }

    /// Ends the session whose token hashes to hash, giving whose it was.
    pub async fn delete_session(&mut self, hash: &[u8]) -> Result<Option<Uuid>> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query_scalar(
                "DELETE FROM sessions WHERE token_hash = $1 RETURNING user_id",
            )
            .bind(hash)
            .fetch_optional(&mut **tx)
            .await?),
            Backend::Lite(tx) => Ok(sqlx::query_scalar(
                "DELETE FROM sessions WHERE token_hash = $1 RETURNING user_id",
            )
            .bind(hash)
            .fetch_optional(&mut **tx)
            .await?),
        }
    }

    pub async fn insert_token(&mut self, t: &AccessToken) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query(
                    "INSERT INTO access_tokens (id, user_id, name, token_hash, created_at, last_used_at, expires_at)
                     VALUES ($1, $2, $3, $4, $5, $6, $7)",
                )
                .bind(t.id)
                .bind(t.user_id)
                .bind(&t.name)
                .bind(&t.token_hash)
                .bind(t.created_at)
                .bind(t.last_used_at)
                .bind(t.expires_at)
                .execute(&mut **tx)
                .await?;
            }
            Backend::Lite(tx) => {
                sqlx::query(
                    "INSERT INTO access_tokens (id, user_id, name, token_hash, created_at, last_used_at, expires_at)
                     VALUES ($1, $2, $3, $4, $5, $6, $7)",
                )
                .bind(t.id)
                .bind(t.user_id)
                .bind(&t.name)
                .bind(&t.token_hash)
                .bind(t.created_at)
                .bind(t.last_used_at)
                .bind(t.expires_at)
                .execute(&mut **tx)
                .await?;
            }
        }
        Ok(())
    }

    /// The access token that hashes to hash, and its user, while it has not
    /// expired and its user is not disabled.
    pub async fn token(
        &mut self,
        hash: &[u8],
        now: DateTime<Utc>,
    ) -> Result<Option<(User, AccessToken)>> {
        let t = match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(
                "SELECT id, user_id, name, token_hash, created_at, last_used_at, expires_at
                 FROM access_tokens WHERE token_hash = $1 AND (expires_at IS NULL OR expires_at > $2)",
            )
            .bind(hash)
            .bind(now)
            .fetch_optional(&mut **tx)
            .await?
            .map(|r| token(&r))
            .transpose()?,
            Backend::Lite(tx) => sqlx::query(
                "SELECT id, user_id, name, token_hash, created_at, last_used_at, expires_at
                 FROM access_tokens WHERE token_hash = $1 AND (expires_at IS NULL OR expires_at > $2)",
            )
            .bind(hash)
            .bind(now)
            .fetch_optional(&mut **tx)
            .await?
            .map(|r| token(&r))
            .transpose()?,
        };
        let Some(t) = t else { return Ok(None) };
        Ok(self
            .user(t.user_id)
            .await?
            .filter(|u| u.disabled_at.is_none())
            .map(|u| (u, t)))
    }

    /// Marks a token used at `now`.
    pub async fn touch_token(&mut self, id: Uuid, now: DateTime<Utc>) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query("UPDATE access_tokens SET last_used_at = $2 WHERE id = $1")
                    .bind(id)
                    .bind(now)
                    .execute(&mut **tx)
                    .await?;
            }
            Backend::Lite(tx) => {
                sqlx::query("UPDATE access_tokens SET last_used_at = $2 WHERE id = $1")
                    .bind(id)
                    .bind(now)
                    .execute(&mut **tx)
                    .await?;
            }
        }
        Ok(())
    }

    /// A user's access tokens, newest first.
    pub async fn tokens_of(&mut self, user_id: Uuid) -> Result<Vec<AccessToken>> {
        match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(
                "SELECT id, user_id, name, token_hash, created_at, last_used_at, expires_at
                 FROM access_tokens WHERE user_id = $1 ORDER BY created_at DESC",
            )
            .bind(user_id)
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(token)
            .collect(),
            Backend::Lite(tx) => sqlx::query(
                "SELECT id, user_id, name, token_hash, created_at, last_used_at, expires_at
                 FROM access_tokens WHERE user_id = $1 ORDER BY created_at DESC",
            )
            .bind(user_id)
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(token)
            .collect(),
        }
    }

    /// Revokes a user's access token, giving it if there was one.
    pub async fn delete_token(&mut self, user_id: Uuid, id: Uuid) -> Result<Option<AccessToken>> {
        match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(
                "DELETE FROM access_tokens WHERE user_id = $1 AND id = $2
                 RETURNING id, user_id, name, token_hash, created_at, last_used_at, expires_at",
            )
            .bind(user_id)
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?
            .map(|r| token(&r))
            .transpose(),
            Backend::Lite(tx) => sqlx::query(
                "DELETE FROM access_tokens WHERE user_id = $1 AND id = $2
                 RETURNING id, user_id, name, token_hash, created_at, last_used_at, expires_at",
            )
            .bind(user_id)
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?
            .map(|r| token(&r))
            .transpose(),
        }
    }

    /// Records an audit event; its ID is the store's to give.
    pub async fn record(&mut self, e: AuditEvent) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query(
                    "INSERT INTO audit_events
                     (at, actor_user, actor_worker, actor_name, source_ip, action, target_type, target_id, target_name, subjects, details)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
                )
                .bind(e.at)
                .bind(e.actor_user)
                .bind(e.actor_worker)
                .bind(&e.actor_name)
                .bind(&e.source_ip)
                .bind(&e.action)
                .bind(&e.target_type)
                .bind(&e.target_id)
                .bind(&e.target_name)
                .bind(&e.subjects)
                .bind(&e.details)
                .execute(&mut **tx)
                .await?;
            }
            Backend::Lite(tx) => {
                sqlx::query(
                    "INSERT INTO audit_events
                     (at, actor_user, actor_worker, actor_name, source_ip, action, target_type, target_id, target_name, subjects, details)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
                )
                .bind(e.at)
                .bind(e.actor_user)
                .bind(e.actor_worker)
                .bind(&e.actor_name)
                .bind(&e.source_ip)
                .bind(&e.action)
                .bind(&e.target_type)
                .bind(&e.target_id)
                .bind(&e.target_name)
                .bind(sqlx::types::Json(&e.subjects))
                .bind(&e.details)
                .execute(&mut **tx)
                .await?;
            }
        }
        Ok(())
    }
}
