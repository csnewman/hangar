//! What the internet-facing edge keeps, for both backends: the zone's
//! records made on demand (`dns_records`), and certificate management's
//! storage and locks (`tls_storage`, `tls_locks`).

use chrono::{DateTime, Utc};
use sqlx::Row;

use super::rows::DnsRecord;
use super::{Backend, Result, Tx};

impl Tx {
    /// Adds a record, or sets the TTL of the one with its name, type and
    /// value.
    pub async fn add_dns_record(&mut self, r: &DnsRecord) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query(
                    "INSERT INTO dns_records (name, type, value, ttl, created_at) VALUES ($1, $2, $3, $4, $5)
                     ON CONFLICT (name, type, value) DO UPDATE SET ttl = EXCLUDED.ttl",
                )
                .bind(&r.name)
                .bind(&r.r#type)
                .bind(&r.value)
                .bind(r.ttl)
                .bind(r.created_at)
                .execute(&mut **tx)
                .await?;
            }
            Backend::Lite(tx) => {
                sqlx::query(
                    "INSERT INTO dns_records (name, type, value, ttl, created_at) VALUES ($1, $2, $3, $4, $5)
                     ON CONFLICT (name, type, value) DO UPDATE SET ttl = EXCLUDED.ttl",
                )
                .bind(&r.name)
                .bind(&r.r#type)
                .bind(&r.value)
                .bind(r.ttl)
                .bind(r.created_at)
                .execute(&mut **tx)
                .await?;
            }
        }
        Ok(())
    }

    pub async fn remove_dns_record(&mut self, name: &str, typ: &str, value: &str) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query("DELETE FROM dns_records WHERE name = $1 AND type = $2 AND value = $3")
                    .bind(name)
                    .bind(typ)
                    .bind(value)
                    .execute(&mut **tx)
                    .await?;
            }
            Backend::Lite(tx) => {
                sqlx::query("DELETE FROM dns_records WHERE name = $1 AND type = $2 AND value = $3")
                    .bind(name)
                    .bind(typ)
                    .bind(value)
                    .execute(&mut **tx)
                    .await?;
            }
        }
        Ok(())
    }

    /// The records of one name and type, oldest first.
    pub async fn dns_records(&mut self, name: &str, typ: &str) -> Result<Vec<DnsRecord>> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                let rows = sqlx::query(
                    "SELECT name, type, value, ttl, created_at FROM dns_records
                     WHERE name = $1 AND type = $2 ORDER BY created_at",
                )
                .bind(name)
                .bind(typ)
                .fetch_all(&mut **tx)
                .await?;
                rows.iter()
                    .map(|r| {
                        Ok(DnsRecord {
                            name: r.try_get("name")?,
                            r#type: r.try_get("type")?,
                            value: r.try_get("value")?,
                            ttl: r.try_get("ttl")?,
                            created_at: r.try_get("created_at")?,
                        })
                    })
                    .collect()
            }
            Backend::Lite(tx) => {
                let rows = sqlx::query(
                    "SELECT name, type, value, ttl, created_at FROM dns_records
                     WHERE name = $1 AND type = $2 ORDER BY created_at",
                )
                .bind(name)
                .bind(typ)
                .fetch_all(&mut **tx)
                .await?;
                rows.iter()
                    .map(|r| {
                        Ok(DnsRecord {
                            name: r.try_get("name")?,
                            r#type: r.try_get("type")?,
                            value: r.try_get("value")?,
                            ttl: r.try_get("ttl")?,
                            created_at: r.try_get("created_at")?,
                        })
                    })
                    .collect()
            }
        }
    }

    /// Deletes the records made before a time, giving how many.
    pub async fn prune_dns_records(&mut self, before: DateTime<Utc>) -> Result<u64> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query("DELETE FROM dns_records WHERE created_at < $1")
                .bind(before)
                .execute(&mut **tx)
                .await?
                .rows_affected()),
            Backend::Lite(tx) => Ok(sqlx::query("DELETE FROM dns_records WHERE created_at < $1")
                .bind(before)
                .execute(&mut **tx)
                .await?
                .rows_affected()),
        }
    }

    pub async fn tls_load(&mut self, key: &str) -> Result<Option<Vec<u8>>> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query_scalar(
                "SELECT value FROM tls_storage WHERE key = $1",
            )
            .bind(key)
            .fetch_optional(&mut **tx)
            .await?),
            Backend::Lite(tx) => Ok(sqlx::query_scalar(
                "SELECT value FROM tls_storage WHERE key = $1",
            )
            .bind(key)
            .fetch_optional(&mut **tx)
            .await?),
        }
    }

    pub async fn tls_store(&mut self, key: &str, value: &[u8], now: DateTime<Utc>) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query(
                    "INSERT INTO tls_storage (key, value, modified) VALUES ($1, $2, $3)
                     ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, modified = EXCLUDED.modified",
                )
                .bind(key)
                .bind(value)
                .bind(now)
                .execute(&mut **tx)
                .await?;
            }
            Backend::Lite(tx) => {
                sqlx::query(
                    "INSERT INTO tls_storage (key, value, modified) VALUES ($1, $2, $3)
                     ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, modified = EXCLUDED.modified",
                )
                .bind(key)
                .bind(value)
                .bind(now)
                .execute(&mut **tx)
                .await?;
            }
        }
        Ok(())
    }

    /// Takes a lock until a time, if nobody holds it or its holder's lease
    /// has run out, saying whether it was taken.
    pub async fn tls_lock(
        &mut self,
        name: &str,
        until: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query(
                "INSERT INTO tls_locks (name, expires_at) VALUES ($1, $2)
                 ON CONFLICT (name) DO UPDATE SET expires_at = EXCLUDED.expires_at
                 WHERE tls_locks.expires_at < $3",
            )
            .bind(name)
            .bind(until)
            .bind(now)
            .execute(&mut **tx)
            .await?
            .rows_affected()
                == 1),
            Backend::Lite(tx) => Ok(sqlx::query(
                "INSERT INTO tls_locks (name, expires_at) VALUES ($1, $2)
                 ON CONFLICT (name) DO UPDATE SET expires_at = EXCLUDED.expires_at
                 WHERE tls_locks.expires_at < $3",
            )
            .bind(name)
            .bind(until)
            .bind(now)
            .execute(&mut **tx)
            .await?
            .rows_affected()
                == 1),
        }
    }

    /// Extends a held lock's lease, saying whether it was still held.
    pub async fn tls_renew_lock(&mut self, name: &str, until: DateTime<Utc>) -> Result<bool> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query(
                "UPDATE tls_locks SET expires_at = $2 WHERE name = $1",
            )
            .bind(name)
            .bind(until)
            .execute(&mut **tx)
            .await?
            .rows_affected()
                == 1),
            Backend::Lite(tx) => Ok(sqlx::query(
                "UPDATE tls_locks SET expires_at = $2 WHERE name = $1",
            )
            .bind(name)
            .bind(until)
            .execute(&mut **tx)
            .await?
            .rows_affected()
                == 1),
        }
    }

    pub async fn tls_unlock(&mut self, name: &str) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query("DELETE FROM tls_locks WHERE name = $1")
                    .bind(name)
                    .execute(&mut **tx)
                    .await?;
            }
            Backend::Lite(tx) => {
                sqlx::query("DELETE FROM tls_locks WHERE name = $1")
                    .bind(name)
                    .execute(&mut **tx)
                    .await?;
            }
        }
        Ok(())
    }
}
