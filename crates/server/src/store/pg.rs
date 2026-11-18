//! The Postgres backend: a pool, and the schema brought up to date with
//! `migrations/`, each one applied recorded in `schema_migrations`.

use sqlx::postgres::PgPoolOptions;

use super::{Error, Result};

/// The migrations, in order: each file's version is its name's number.
const MIGRATIONS: &[(i32, &str)] = &[
    (1, include_str!("../../migrations/0001_init.sql")),
    (
        2,
        include_str!("../../migrations/0002_profile_versions.sql"),
    ),
    (3, include_str!("../../migrations/0003_profile_blobs.sql")),
    (
        4,
        include_str!("../../migrations/0004_drop_profile_data.sql"),
    ),
    (
        5,
        include_str!("../../migrations/0005_profile_trusted_only.sql"),
    ),
    (6, include_str!("../../migrations/0006_file_sets.sql")),
    (7, include_str!("../../migrations/0007_file_trust.sql")),
    (8, include_str!("../../migrations/0008_access.sql")),
    (9, include_str!("../../migrations/0009_packs.sql")),
    (10, include_str!("../../migrations/0010_pack_squash.sql")),
    (
        11,
        include_str!("../../migrations/0011_profile_code_settings.sql"),
    ),
    (12, include_str!("../../migrations/0012_user_theme.sql")),
    (13, include_str!("../../migrations/0013_worker_monitor.sql")),
    (14, include_str!("../../migrations/0014_drop_file_sets.sql")),
    (
        15,
        include_str!("../../migrations/0015_worker_suspends.sql"),
    ),
    (
        16,
        include_str!("../../migrations/0016_tasks_and_agents.sql"),
    ),
    (17, include_str!("../../migrations/0017_sign_in_grants.sql")),
    (
        18,
        include_str!("../../migrations/0018_worker_features.sql"),
    ),
    (
        19,
        include_str!("../../migrations/0019_environment_guest_agent.sql"),
    ),
    (
        20,
        include_str!("../../migrations/0020_environment_revisions.sql"),
    ),
    (21, include_str!("../../migrations/0021_links.sql")),
    (22, include_str!("../../migrations/0022_link_records.sql")),
    (23, include_str!("../../migrations/0023_link_sending.sql")),
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

/// The advisory lock migrations are applied under, so of two servers
/// starting together neither applies one the other is applying.
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
