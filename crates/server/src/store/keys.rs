//! SSH keys, sign-in keys and the server's own keys, for both backends.

use sqlx::Row;
use uuid::Uuid;

use super::rows::{LoginKey, ServerKey, SshKey, User};
use super::{Backend, Result, Tx};

/// What became of a sign-in key to be added.
pub enum Added {
    Done,
    /// The person has the key already.
    Taken,
}

fn ssh_key(r: &sqlx::postgres::PgRow) -> Result<SshKey> {
    Ok(SshKey {
        id: r.try_get("id")?,
        user_id: r.try_get("user_id")?,
        name: r.try_get("name")?,
        public_key: r.try_get("public_key")?,
        private_key: r.try_get("private_key")?,
        created_at: r.try_get("created_at")?,
    })
}

fn login_key(r: &sqlx::postgres::PgRow) -> Result<LoginKey> {
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
            Backend::Json(tx) => {
                let mut out: Vec<SshKey> = tx
                    .work
                    .ssh_keys
                    .iter()
                    .filter(|k| k.user_id == user)
                    .cloned()
                    .collect();
                out.sort_by_key(|k| k.created_at);
                Ok(out)
            }
        }
    }

    pub async fn insert_ssh_key(&mut self, k: &SshKey) -> Result<()> {
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
            Backend::Json(tx) => tx.work.ssh_keys.push(k.clone()),
        }
        Ok(())
    }

    /// Deletes one of a person's SSH keys, giving its name.
    pub async fn delete_ssh_key(&mut self, user: Uuid, id: Uuid) -> Result<Option<String>> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query_scalar(
                "DELETE FROM ssh_keys WHERE user_id = $1 AND id = $2 RETURNING name",
            )
            .bind(user)
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?),
            Backend::Json(tx) => {
                let at = tx
                    .work
                    .ssh_keys
                    .iter()
                    .position(|k| k.user_id == user && k.id == id);
                Ok(at.map(|i| tx.work.ssh_keys.remove(i).name))
            }
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
            Backend::Json(tx) => {
                let mut out: Vec<LoginKey> = tx
                    .work
                    .ssh_login_keys
                    .iter()
                    .filter(|k| k.user_id == user)
                    .cloned()
                    .collect();
                out.sort_by_key(|k| k.created_at);
                Ok(out)
            }
        }
    }

    pub async fn insert_login_key(&mut self, k: &LoginKey) -> Result<Added> {
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
            Backend::Json(tx) => {
                let keys = &mut tx.work.ssh_login_keys;
                if keys
                    .iter()
                    .any(|o| o.user_id == k.user_id && o.fingerprint == k.fingerprint)
                {
                    return Ok(Added::Taken);
                }
                keys.push(k.clone());
                Ok(Added::Done)
            }
        }
    }

    /// Deletes one of a person's sign-in keys, giving its name.
    pub async fn delete_login_key(&mut self, user: Uuid, id: Uuid) -> Result<Option<String>> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query_scalar(
                "DELETE FROM ssh_login_keys WHERE user_id = $1 AND id = $2 RETURNING name",
            )
            .bind(user)
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?),
            Backend::Json(tx) => {
                let at = tx
                    .work
                    .ssh_login_keys
                    .iter()
                    .position(|k| k.user_id == user && k.id == id);
                Ok(at.map(|i| tx.work.ssh_login_keys.remove(i).name))
            }
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
            Backend::Json(tx) => {
                let w = &tx.work;
                let mut keys: Vec<&LoginKey> = w
                    .ssh_login_keys
                    .iter()
                    .filter(|k| k.fingerprint == fingerprint)
                    .collect();
                keys.sort_by_key(|k| k.created_at);
                Ok(keys
                    .into_iter()
                    .filter_map(|k| w.users.iter().find(|u| u.id == k.user_id))
                    .filter(|u| u.disabled_at.is_none() && u.kind == "person")
                    .cloned()
                    .collect())
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
            Backend::Json(tx) => Ok(tx.work.server_keys.iter().find(|k| k.name == name).cloned()),
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
            Backend::Json(tx) => {
                let keys = &mut tx.work.server_keys;
                keys.retain(|o| o.name != k.name);
                keys.push(k.clone());
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
            Backend::Json(tx) => {
                let keys = &mut tx.work.server_keys;
                if let Some(k) = keys.iter().find(|k| k.name == fresh.name) {
                    return Ok(k.clone());
                }
                keys.push(fresh.clone());
                Ok(fresh.clone())
            }
        }
    }
}
