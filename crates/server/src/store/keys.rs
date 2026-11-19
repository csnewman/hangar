//! SSH keys, sign-in keys and the server's own keys, for both backends.

use sqlx::Row;
use uuid::Uuid;

use super::rows::{LoginKey, ServerKey, SshKey, User};
use super::signals::Signal;
use super::{Backend, Result, Tx};

/// What became of a sign-in key to be added.
pub enum Added {
    Done,
    /// The person has the key already.
    Taken,
}

fn ssh_key(r: &impl super::Get) -> Result<SshKey> {
    Ok(SshKey {
        id: r.try_get("id")?,
        user_id: r.try_get("user_id")?,
        name: r.try_get("name")?,
        public_key: r.try_get("public_key")?,
        private_key: r.try_get("private_key")?,
        created_at: r.try_get("created_at")?,
    })
}

fn login_key(r: &impl super::Get) -> Result<LoginKey> {
    Ok(LoginKey {
        id: r.try_get("id")?,
        user_id: r.try_get("user_id")?,
        name: r.try_get("name")?,
        public_key: r.try_get("public_key")?,
        fingerprint: r.try_get("fingerprint")?,
        created_at: r.try_get("created_at")?,
    })
}

impl Tx {
    /// A person's SSH keys, oldest first.
    pub async fn ssh_keys(&mut self, user: Uuid) -> Result<Vec<SshKey>> {
        match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(
                "SELECT id, user_id, name, public_key, private_key, created_at FROM ssh_keys
                 WHERE user_id = $1 ORDER BY created_at",
            )
            .bind(user)
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(ssh_key)
            .collect(),
            Backend::Lite(tx) => sqlx::query(
                "SELECT id, user_id, name, public_key, private_key, created_at FROM ssh_keys
                 WHERE user_id = $1 ORDER BY created_at",
            )
            .bind(user)
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(ssh_key)
            .collect(),
        }
    }

    pub async fn insert_ssh_key(&mut self, k: &SshKey) -> Result<()> {
        self.signal(Signal::Records);
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query(
                    "INSERT INTO ssh_keys (id, user_id, name, public_key, private_key, created_at)
                     VALUES ($1, $2, $3, $4, $5, $6)",
                )
                .bind(k.id)
                .bind(k.user_id)
                .bind(&k.name)
                .bind(&k.public_key)
                .bind(&k.private_key)
                .bind(k.created_at)
                .execute(&mut **tx)
                .await?;
            }
            Backend::Lite(tx) => {
                sqlx::query(
                    "INSERT INTO ssh_keys (id, user_id, name, public_key, private_key, created_at)
                     VALUES ($1, $2, $3, $4, $5, $6)",
                )
                .bind(k.id)
                .bind(k.user_id)
                .bind(&k.name)
                .bind(&k.public_key)
                .bind(&k.private_key)
                .bind(k.created_at)
                .execute(&mut **tx)
                .await?;
            }
        }
        Ok(())
    }

    /// Deletes one of a person's SSH keys, giving its name.
    pub async fn delete_ssh_key(&mut self, user: Uuid, id: Uuid) -> Result<Option<String>> {
        self.signal(Signal::Records);
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query_scalar(
                "DELETE FROM ssh_keys WHERE user_id = $1 AND id = $2 RETURNING name",
            )
            .bind(user)
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?),
            Backend::Lite(tx) => Ok(sqlx::query_scalar(
                "DELETE FROM ssh_keys WHERE user_id = $1 AND id = $2 RETURNING name",
            )
            .bind(user)
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?),
        }
    }

    /// A person's sign-in keys, oldest first.
    pub async fn login_keys(&mut self, user: Uuid) -> Result<Vec<LoginKey>> {
        match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(
                "SELECT id, user_id, name, public_key, fingerprint, created_at FROM ssh_login_keys
                 WHERE user_id = $1 ORDER BY created_at",
            )
            .bind(user)
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(login_key)
            .collect(),
            Backend::Lite(tx) => sqlx::query(
                "SELECT id, user_id, name, public_key, fingerprint, created_at FROM ssh_login_keys
                 WHERE user_id = $1 ORDER BY created_at",
            )
            .bind(user)
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(login_key)
            .collect(),
        }
    }

    pub async fn insert_login_key(&mut self, k: &LoginKey) -> Result<Added> {
        self.signal(Signal::Records);
        match &mut self.inner {
            Backend::Pg(tx) => {
                let done = sqlx::query(
                    "INSERT INTO ssh_login_keys (id, user_id, name, public_key, fingerprint, created_at)
                     VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (user_id, fingerprint) DO NOTHING",
                )
                .bind(k.id)
                .bind(k.user_id)
                .bind(&k.name)
                .bind(&k.public_key)
                .bind(&k.fingerprint)
                .bind(k.created_at)
                .execute(&mut **tx)
                .await?
                .rows_affected();
                Ok(if done == 1 { Added::Done } else { Added::Taken })
            }
            Backend::Lite(tx) => {
                let done = sqlx::query(
                    "INSERT INTO ssh_login_keys (id, user_id, name, public_key, fingerprint, created_at)
                     VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (user_id, fingerprint) DO NOTHING",
                )
                .bind(k.id)
                .bind(k.user_id)
                .bind(&k.name)
                .bind(&k.public_key)
                .bind(&k.fingerprint)
                .bind(k.created_at)
                .execute(&mut **tx)
                .await?
                .rows_affected();
                Ok(if done == 1 { Added::Done } else { Added::Taken })
            }
        }
    }

    /// Deletes one of a person's sign-in keys, giving its name.
    pub async fn delete_login_key(&mut self, user: Uuid, id: Uuid) -> Result<Option<String>> {
        self.signal(Signal::Records);
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query_scalar(
                "DELETE FROM ssh_login_keys WHERE user_id = $1 AND id = $2 RETURNING name",
            )
            .bind(user)
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?),
            Backend::Lite(tx) => Ok(sqlx::query_scalar(
                "DELETE FROM ssh_login_keys WHERE user_id = $1 AND id = $2 RETURNING name",
            )
            .bind(user)
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?),
        }
    }

    /// The enabled people who sign in with the key of a fingerprint, by
    /// when they added it.
    pub async fn login_key_owners(&mut self, fingerprint: &str) -> Result<Vec<User>> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                let ids: Vec<Uuid> = sqlx::query_scalar(
                    "SELECT u.id FROM ssh_login_keys k JOIN users u ON u.id = k.user_id
                     WHERE k.fingerprint = $1 AND u.disabled_at IS NULL AND u.kind = 'person'
                     ORDER BY k.created_at",
                )
                .bind(fingerprint)
                .fetch_all(&mut **tx)
                .await?;
                let mut out = Vec::new();
                for id in ids {
                    if let Some(u) = self.user(id).await? {
                        out.push(u);
                    }
                }
                Ok(out)
            }
            Backend::Lite(tx) => {
                let ids: Vec<Uuid> = sqlx::query_scalar(
                    "SELECT u.id FROM ssh_login_keys k JOIN users u ON u.id = k.user_id
                     WHERE k.fingerprint = $1 AND u.disabled_at IS NULL AND u.kind = 'person'
                     ORDER BY k.created_at",
                )
                .bind(fingerprint)
                .fetch_all(&mut **tx)
                .await?;
                let mut out = Vec::new();
                for id in ids {
                    if let Some(u) = self.user(id).await? {
                        out.push(u);
                    }
                }
                Ok(out)
            }
        }
    }

    /// The server's key of a name, if it has one.
    pub async fn find_server_key(&mut self, name: &str) -> Result<Option<ServerKey>> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                let r = sqlx::query(
                    "SELECT name, value, sealed, created_at FROM server_keys WHERE name = $1",
                )
                .bind(name)
                .fetch_optional(&mut **tx)
                .await?;
                r.map(|r| {
                    Ok(ServerKey {
                        name: r.try_get("name")?,
                        value: r.try_get("value")?,
                        sealed: r.try_get("sealed")?,
                        created_at: r.try_get("created_at")?,
                    })
                })
                .transpose()
            }
            Backend::Lite(tx) => {
                let r = sqlx::query(
                    "SELECT name, value, sealed, created_at FROM server_keys WHERE name = $1",
                )
                .bind(name)
                .fetch_optional(&mut **tx)
                .await?;
                r.map(|r| {
                    Ok(ServerKey {
                        name: r.try_get("name")?,
                        value: r.try_get("value")?,
                        sealed: r.try_get("sealed")?,
                        created_at: r.try_get("created_at")?,
                    })
                })
                .transpose()
            }
        }
    }

    /// Sets the server's key of a name, made or replaced.
    pub async fn put_server_key(&mut self, k: &ServerKey) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query(
                    "INSERT INTO server_keys (name, value, sealed, created_at) VALUES ($1, $2, $3, $4)
                     ON CONFLICT (name) DO UPDATE SET value = EXCLUDED.value, sealed = EXCLUDED.sealed,
                     created_at = EXCLUDED.created_at",
                )
                .bind(&k.name)
                .bind(&k.value)
                .bind(k.sealed)
                .bind(k.created_at)
                .execute(&mut **tx)
                .await?;
            }
            Backend::Lite(tx) => {
                sqlx::query(
                    "INSERT INTO server_keys (name, value, sealed, created_at) VALUES ($1, $2, $3, $4)
                     ON CONFLICT (name) DO UPDATE SET value = EXCLUDED.value, sealed = EXCLUDED.sealed,
                     created_at = EXCLUDED.created_at",
                )
                .bind(&k.name)
                .bind(&k.value)
                .bind(k.sealed)
                .bind(k.created_at)
                .execute(&mut **tx)
                .await?;
            }
        }
        Ok(())
    }

    /// The server's key of a name, made with value as given when there is
    /// none yet: whichever server gets there first makes it.
    pub async fn server_key(&mut self, fresh: &ServerKey) -> Result<ServerKey> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query(
                    "INSERT INTO server_keys (name, value, sealed, created_at) VALUES ($1, $2, $3, $4)
                     ON CONFLICT (name) DO NOTHING",
                )
                .bind(&fresh.name)
                .bind(&fresh.value)
                .bind(fresh.sealed)
                .bind(fresh.created_at)
                .execute(&mut **tx)
                .await?;
                let r = sqlx::query(
                    "SELECT name, value, sealed, created_at FROM server_keys WHERE name = $1",
                )
                .bind(&fresh.name)
                .fetch_one(&mut **tx)
                .await?;
                Ok(ServerKey {
                    name: r.try_get("name")?,
                    value: r.try_get("value")?,
                    sealed: r.try_get("sealed")?,
                    created_at: r.try_get("created_at")?,
                })
            }
            Backend::Lite(tx) => {
                sqlx::query(
                    "INSERT INTO server_keys (name, value, sealed, created_at) VALUES ($1, $2, $3, $4)
                     ON CONFLICT (name) DO NOTHING",
                )
                .bind(&fresh.name)
                .bind(&fresh.value)
                .bind(fresh.sealed)
                .bind(fresh.created_at)
                .execute(&mut **tx)
                .await?;
                let r = sqlx::query(
                    "SELECT name, value, sealed, created_at FROM server_keys WHERE name = $1",
                )
                .bind(&fresh.name)
                .fetch_one(&mut **tx)
                .await?;
                Ok(ServerKey {
                    name: r.try_get("name")?,
                    value: r.try_get("value")?,
                    sealed: r.try_get("sealed")?,
                    created_at: r.try_get("created_at")?,
                })
            }
        }
    }
}
