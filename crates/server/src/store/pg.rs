//! The Postgres backend: a pool, and the schema brought up to date with
//! `internal/db/migrations`, recorded in `schema_migrations` as the Go
//! server records it, so a database moves between the two either way.

use sqlx::postgres::PgPoolOptions;

use super::{Error, Result};

/// The migrations, in order: each file's version is its name's number.
const MIGRATIONS: &[(i32, &str)] = &[
    (
        1,
        include_str!("../../../../internal/db/migrations/0001_init.sql"),
    ),
    (
        2,
        include_str!("../../../../internal/db/migrations/0002_profile_versions.sql"),
    ),
    (
        3,
        include_str!("../../../../internal/db/migrations/0003_profile_blobs.sql"),
    ),
    (
        4,
        include_str!("../../../../internal/db/migrations/0004_drop_profile_data.sql"),
    ),
    (
        5,
        include_str!("../../../../internal/db/migrations/0005_profile_trusted_only.sql"),
    ),
    (
        6,
        include_str!("../../../../internal/db/migrations/0006_file_sets.sql"),
    ),
    (
        7,
        include_str!("../../../../internal/db/migrations/0007_file_trust.sql"),
    ),
    (
        8,
        include_str!("../../../../internal/db/migrations/0008_access.sql"),
    ),
    (
        9,
        include_str!("../../../../internal/db/migrations/0009_packs.sql"),
    ),
    (
        10,
        include_str!("../../../../internal/db/migrations/0010_pack_squash.sql"),
    ),
    (
        11,
        include_str!("../../../../internal/db/migrations/0011_profile_code_settings.sql"),
    ),
    (
        12,
        include_str!("../../../../internal/db/migrations/0012_user_theme.sql"),
    ),
    (
        13,
        include_str!("../../../../internal/db/migrations/0013_worker_monitor.sql"),
    ),
    (
        14,
        include_str!("../../../../internal/db/migrations/0014_drop_file_sets.sql"),
    ),
    (
        15,
        include_str!("../../../../internal/db/migrations/0015_worker_suspends.sql"),
    ),
    (
        16,
        include_str!("../../../../internal/db/migrations/0016_tasks_and_agents.sql"),
    ),
    (
        17,
        include_str!("../../../../internal/db/migrations/0017_sign_in_grants.sql"),
    ),
];

/// Connects to the database at url and brings its schema up to date.
pub async fn open(url: &str) -> Result<sqlx::PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(16)
        .connect(url)
        .await
        .map_err(|e| Error(format!("connecting to the database: {e}")))?;
    migrate(&pool).await?;
    Ok(pool)
}

/// The advisory lock migrations are applied under: the Go server's
/// (internal/db), so neither server applies one the other is applying.
const MIGRATE_LOCK: i64 = 0x68616e676172;

/// Applies each migration the database has not had, each in a transaction
/// of its own, under MIGRATE_LOCK.
async fn migrate(pool: &sqlx::PgPool) -> Result<()> {
    let mut conn = pool.acquire().await?;
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(MIGRATE_LOCK)
        .execute(&mut *conn)
        .await?;
    let out = async {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                version    integer PRIMARY KEY,
                applied_at timestamptz NOT NULL DEFAULT now())",
        )
        .execute(&mut *conn)
        .await?;
        let current: i32 =
            sqlx::query_scalar("SELECT coalesce(max(version), 0) FROM schema_migrations")
                .fetch_one(&mut *conn)
                .await?;
        for (version, sql) in MIGRATIONS.iter().filter(|(v, _)| *v > current) {
            let mut tx = sqlx::Connection::begin(&mut *conn).await?;
            sqlx::raw_sql(sql)
                .execute(&mut *tx)
                .await
                .map_err(|e| Error(format!("migration {version}: {e}")))?;
            sqlx::query("INSERT INTO schema_migrations (version) VALUES ($1)")
                .bind(version)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
        }
        Ok::<(), Error>(())
    }
    .await;
    let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(MIGRATE_LOCK)
        .execute(&mut *conn)
        .await;
    out
}
