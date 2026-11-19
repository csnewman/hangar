//! The SQLite backend, for one server on one machine: a database file
//! with its write-ahead log, synced at every commit. One connection
//! writes, so one transaction that writes at a time, which is what gives
//! environments their revisions in commit order (`revise`); transactions
//! that only read (`Store::read`) take connections of their own, which
//! refuse writes, and read beside it from the database as last committed.
//! A lock file beside it keeps a second server off it.
//!
//! A new database is made beside the file under another name and renamed
//! into place once it has its schema and its first rows: the rows a new
//! Hangar starts with, or those of a JSON state file of the same name
//! (`state.json` for `state.db`), which is then renamed `.imported`.

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};
use sqlx::types::Json;
use uuid::Uuid;

use super::json::State;
use super::{Error, Mark, Result};

/// The migrations, in order: each file's version is its name's number.
const MIGRATIONS: &[(i64, &str)] = &[(1, include_str!("../../migrations-sqlite/0001_init.sql"))];

/// How many transactions read at once.
const READERS: u32 = 8;

/// The database, open.
pub struct Lite {
    /// The one connection that writes.
    pub writer: SqlitePool,
    /// Connections that only read.
    pub readers: SqlitePool,
    /// Held while the server runs, so a second one fails to start.
    _lock: std::fs::File,
}

/// Where a server told path keeps its database: path itself, or for a
/// path naming a JSON state file, the same name ending `.db`.
pub fn database_path(path: &Path) -> PathBuf {
    if path.extension().is_some_and(|e| e == "json") {
        path.with_extension("db")
    } else {
        path.to_path_buf()
    }
}

/// Opens the database for path (`database_path`), making it if there is
/// none, and brings its schema up to date.
pub async fn open(path: &Path) -> Result<Lite> {
    let db = database_path(path);
    let lock_path = db.with_extension("lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| Error(format!("{}: {e}", lock_path.display())))?;
    lock.try_lock().map_err(|_| {
        Error(format!(
            "{} is held by another server: one server keeps a state file",
            db.display()
        ))
    })?;
    if !db.exists() {
        make(&db).await?;
    }
    let writer = connect(&db, Use::Write).await?;
    migrate(&writer).await?;
    let readers = connect(&db, Use::Read).await?;
    Ok(Lite {
        writer,
        readers,
        _lock: lock,
    })
}

/// What a pool's connections are for.
#[derive(PartialEq)]
enum Use {
    /// The database's one writer.
    Write,
    /// Reading alone, beside the writer.
    Read,
    /// Making a new database: its first rows, foreign keys not yet
    /// checked, and no write-ahead log, so it is one file once closed.
    Make,
}

/// A pool of connections to the database at path, for a use: the
/// writer's one, foreign keys enforced and the write-ahead log synced at
/// every commit; or READERS refusing writes. A transaction waits for a
/// connection however long it takes, as each takes its turn.
async fn connect(path: &Path, using: Use) -> Result<SqlitePool> {
    let url = format!("sqlite://{}", path.display());
    let mut options = SqliteConnectOptions::from_str(&url)
        .map_err(|e| Error(format!("{}: {e}", path.display())))?
        .busy_timeout(Duration::from_secs(30));
    options = match using {
        Use::Write | Use::Make => options
            .create_if_missing(true)
            .foreign_keys(using == Use::Write)
            .journal_mode(if using == Use::Write {
                SqliteJournalMode::Wal
            } else {
                SqliteJournalMode::Delete
            })
            .synchronous(SqliteSynchronous::Full),
        Use::Read => options.pragma("query_only", "ON"),
    };
    let connections = if using == Use::Read { READERS } else { 1 };
    SqlitePoolOptions::new()
        .max_connections(connections)
        .min_connections(1)
        .acquire_timeout(Duration::from_secs(24 * 60 * 60))
        .idle_timeout(None)
        .max_lifetime(None)
        .connect_with(options)
        .await
        .map_err(|e| Error(format!("{}: {e}", path.display())))
}

/// Applies each migration the database has not had, each in a
/// transaction of its own.
async fn migrate(pool: &SqlitePool) -> Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version    INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL) STRICT",
    )
    .execute(pool)
    .await?;
    let current: i64 =
        sqlx::query_scalar("SELECT coalesce(max(version), 0) FROM schema_migrations")
            .fetch_one(pool)
            .await?;
    for (version, sql) in MIGRATIONS.iter().filter(|(v, _)| *v > current) {
        let mut tx = pool.begin().await?;
        sqlx::raw_sql(sql)
            .execute(&mut *tx)
            .await
            .map_err(|e| Error(format!("migration {version}: {e}")))?;
        sqlx::query("INSERT INTO schema_migrations (version, applied_at) VALUES ($1, $2)")
            .bind(version)
            .bind(chrono::Utc::now())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }
    Ok(())
}

/// Makes the database at db: its schema and first rows, under another
/// name, renamed into place once whole.
async fn make(db: &Path) -> Result<()> {
    let io = |p: &Path, e: std::io::Error| Error(format!("{}: {e}", p.display()));
    let json = db.with_extension("json");
    let state = match std::fs::read(&json) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|e| Error(format!("{}: {e}", json.display())))?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::new(),
        Err(e) => return Err(io(&json, e)),
    };
    let making = PathBuf::from(format!("{}.new", db.display()));
    for leftover in ["", "-journal", "-wal", "-shm"] {
        let p = PathBuf::from(format!("{}{leftover}", making.display()));
        match std::fs::remove_file(&p) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io(&p, e)),
        }
    }
    let pool = connect(&making, Use::Make).await?;
    migrate(&pool).await?;
    import(&pool, &state).await?;
    pool.close().await;
    std::fs::rename(&making, db).map_err(|e| io(db, e))?;
    if let Some(dir) = db.parent() {
        let dir = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        };
        std::fs::File::open(dir)
            .and_then(|d| d.sync_all())
            .map_err(|e| io(dir, e))?;
    }
    if json.exists() {
        let kept = PathBuf::from(format!("{}.imported", json.display()));
        std::fs::rename(&json, &kept).map_err(|e| io(&json, e))?;
    }
    Ok(())
}

/// Writes every row of a state into a new database, whose foreign keys
/// are not enforced as it is written. A JSON file kept few of Postgres's
/// constraints, so a row that breaks one of the others is left out; then
/// a reference to a row that is gone is cleared where Postgres would clear
/// it, and the rows that hold any other are deleted, as Postgres's
/// cascades would have deleted them. What is left out is logged.
async fn import(pool: &SqlitePool, s: &State) -> Result<()> {
    let mut tx = pool.begin().await?;
    let c = &mut *tx;
    macro_rules! put {
        ($rows:expr, $sql:literal, |$r:ident| $($bind:expr),+ $(,)?) => {
            let sql = $sql.replacen("INSERT INTO", "INSERT OR IGNORE INTO", 1);
            let mut kept = 0;
            for $r in &$rows {
                kept += sqlx::query(&sql)$(.bind($bind))+.execute(&mut *c).await?.rows_affected();
            }
            let left = $rows.len() as u64 - kept;
            if left > 0 {
                let table = $sql["INSERT INTO ".len()..].split(' ').next().unwrap_or("");
                eprintln!(
                    "hangar-server: importing the state file: {left} {table} rows break the database's constraints and are left out"
                );
            }
        };
    }
    put!(
        s.users,
        "INSERT INTO users (id, username, display_name, password_hash, is_admin, kind, disabled_at, created_at, theme)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        |r| r.id, &r.username, &r.display_name, &r.password_hash, r.is_admin, &r.kind,
            r.disabled_at, r.created_at, &r.theme
    );
    put!(
        s.sessions,
        "INSERT INTO sessions (token_hash, user_id, created_at, last_seen_at, expires_at)
         VALUES ($1, $2, $3, $4, $5)",
        |r| &r.token_hash,
        r.user_id,
        r.created_at,
        r.last_seen_at,
        r.expires_at
    );
    put!(
        s.access_tokens,
        "INSERT INTO access_tokens (id, user_id, name, token_hash, created_at, last_used_at, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
        |r| r.id, r.user_id, &r.name, &r.token_hash, r.created_at, r.last_used_at, r.expires_at
    );
    put!(
        s.workers,
        "INSERT INTO workers (id, name, credential_hash, labels, cpus, memory_mib, unknown, stats, images,
            gpu, desired_version, last_seen_at, revoked_at, created_at, monitor, version, features)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)",
        |r| r.id, &r.name, &r.credential_hash, &r.labels, r.cpus, r.memory_mib, &r.unknown,
            &r.stats, &r.images, &r.gpu, r.desired_version, r.last_seen_at, r.revoked_at,
            r.created_at, &r.monitor, &r.version, Json(&r.features)
    );
    put!(
        s.audit_events,
        "INSERT INTO audit_events (id, at, actor_user, actor_worker, actor_name, source_ip, action,
            target_type, target_id, target_name, subjects, details)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        |r| r.id,
        r.at,
        r.actor_user,
        r.actor_worker,
        &r.actor_name,
        &r.source_ip,
        &r.action,
        &r.target_type,
        &r.target_id,
        &r.target_name,
        Json(&r.subjects),
        &r.details
    );
    put!(
        s.teams,
        "INSERT INTO teams (id, slug, name, description, created_at) VALUES ($1, $2, $3, $4, $5)",
        |r| r.id,
        &r.slug,
        &r.name,
        &r.description,
        r.created_at
    );
    put!(
        s.team_members,
        "INSERT INTO team_members (team_id, user_id, role) VALUES ($1, $2, $3)",
        |r| r.team_id,
        r.user_id,
        &r.role
    );
    put!(
        s.templates,
        "INSERT INTO templates (id, owner_id, team_id, name, description, visibility, spec, revision,
            created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        |r| r.id, r.owner_id, r.team_id, &r.name, &r.description, &r.visibility, &r.spec,
            r.revision, r.created_at, r.updated_at
    );
    put!(
        s.template_collaborators,
        "INSERT INTO template_collaborators (template_id, user_id) VALUES ($1, $2)",
        |r| r.template_id,
        r.who
    );
    put!(
        s.template_team_collaborators,
        "INSERT INTO template_team_collaborators (template_id, team_id) VALUES ($1, $2)",
        |r| r.template_id,
        r.who
    );
    put!(
        s.environments,
        "INSERT INTO environments (id, short_id, owner_id, name, template_id, template_name,
            template_revision, spec, image, image_digest, image_pin_want, image_drop_rollback,
            image_update, image_rollback, ports_public, cpus, memory_mib, desired, worker_id, phase,
            reason, stats, progress, created_at, updated_at, guest_agent, revision)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18,
            $19, $20, $21, $22, $23, $24, $25, $26, $27)",
        |r| r.id,
        &r.short_id,
        r.owner_id,
        &r.name,
        r.template_id,
        &r.template_name,
        r.template_revision,
        &r.spec,
        &r.image,
        &r.image_digest,
        &r.image_pin_want,
        r.image_drop_rollback,
        &r.image_update,
        &r.image_rollback,
        r.ports_public,
        r.cpus,
        r.memory_mib,
        &r.desired,
        r.worker_id,
        &r.phase,
        &r.reason,
        &r.stats,
        &r.progress,
        r.created_at,
        r.updated_at,
        &r.guest_agent,
        r.revision
    );
    put!(
        s.worker_image_removals,
        "INSERT INTO worker_image_removals (worker_id, ref, digest) VALUES ($1, $2, $3)",
        |r| r.worker_id,
        &r.r#ref,
        &r.digest
    );
    put!(
        s.worker_environment_removals,
        "INSERT INTO worker_environment_removals (worker_id, environment) VALUES ($1, $2)",
        |r| r.worker_id,
        &r.environment
    );
    put!(
        s.environment_tasks,
        "INSERT INTO environment_tasks (id, environment_id, text, set_by, set_at, done_at)
         VALUES ($1, $2, $3, $4, $5, $6)",
        |r| r.id,
        r.environment_id,
        &r.text,
        &r.set_by,
        r.set_at,
        r.done_at
    );
    put!(
        s.agent_sessions,
        "INSERT INTO agent_sessions (environment_id, id, agent, terminal, state, step, message, since, seen_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        |r| r.environment_id, &r.id, &r.agent, &r.terminal, &r.state, &r.step, &r.message,
            r.since, r.seen_at
    );
    put!(
        s.editor_sessions,
        "INSERT INTO editor_sessions (token_hash, session_hash, environment_id, ticket, expires_at)
         VALUES ($1, $2, $3, $4, $5)",
        |r| &r.token_hash,
        &r.session_hash,
        r.environment_id,
        r.ticket,
        r.expires_at
    );
    put!(
        s.ssh_keys,
        "INSERT INTO ssh_keys (id, user_id, name, public_key, private_key, created_at)
         VALUES ($1, $2, $3, $4, $5, $6)",
        |r| r.id,
        r.user_id,
        &r.name,
        &r.public_key,
        &r.private_key,
        r.created_at
    );
    put!(
        s.ssh_login_keys,
        "INSERT INTO ssh_login_keys (id, user_id, name, public_key, fingerprint, created_at)
         VALUES ($1, $2, $3, $4, $5, $6)",
        |r| r.id,
        r.user_id,
        &r.name,
        &r.public_key,
        &r.fingerprint,
        r.created_at
    );
    put!(
        s.server_keys,
        "INSERT INTO server_keys (name, value, sealed, created_at) VALUES ($1, $2, $3, $4)",
        |r| &r.name,
        &r.value,
        r.sealed,
        r.created_at
    );
    put!(
        s.packs,
        "INSERT INTO packs (id, name, description, owner_id, team_id, builtin, attach, created_at,
            updated_at, squash, same_name, merged, places, away)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
        |r| r.id,
        &r.name,
        &r.description,
        r.owner_id,
        r.team_id,
        r.builtin,
        &r.attach,
        r.created_at,
        r.updated_at,
        &r.squash,
        r.same_name,
        Json(&r.merged),
        Json(&r.places),
        r.away
    );
    put!(
        s.pack_paths,
        "INSERT INTO pack_paths (pack_id, path, sensitive, send_every) VALUES ($1, $2, $3, $4)",
        |r| r.pack_id,
        &r.path,
        r.sensitive,
        r.send_every
    );
    put!(
        s.copies,
        "INSERT INTO copies (id, pack_id, owner_id, team_id, personal, name, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        |r| r.id, r.pack_id, r.owner_id, r.team_id, r.personal, &r.name, r.created_at, r.updated_at
    );
    put!(
        s.pack_defaults,
        "INSERT INTO pack_defaults (user_id, pack_id, copy_id) VALUES ($1, $2, $3)",
        |r| r.user_id,
        r.pack_id,
        r.copy_id
    );
    put!(
        s.environment_copies,
        "INSERT INTO environment_copies (environment_id, pack_id, copy_id, chosen) VALUES ($1, $2, $3, $4)",
        |r| r.environment_id,
        r.pack_id,
        r.copy_id,
        r.chosen
    );
    put!(
        s.environment_routed,
        "INSERT INTO environment_routed (environment_id, copy_id) VALUES ($1, $2)",
        |r| r.environment_id,
        r.copy_id
    );
    put!(
        s.environment_conflicts,
        "INSERT INTO environment_conflicts (environment_id, copy_id, path, found_at, resolution)
         VALUES ($1, $2, $3, $4, $5)",
        |r| r.environment_id,
        r.copy_id,
        &r.path,
        r.found_at,
        &r.resolution
    );
    put!(
        s.image_repositories,
        "INSERT INTO image_repositories (id, path, owner_id, team_id, visibility, description,
            created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        |r| r.id,
        &r.path,
        r.owner_id,
        r.team_id,
        &r.visibility,
        &r.description,
        r.created_at,
        r.updated_at
    );
    put!(
        s.image_repository_collaborators,
        "INSERT INTO image_repository_collaborators (repository_id, user_id) VALUES ($1, $2)",
        |r| r.repository_id,
        r.who
    );
    put!(
        s.image_repository_team_collaborators,
        "INSERT INTO image_repository_team_collaborators (repository_id, team_id) VALUES ($1, $2)",
        |r| r.repository_id,
        r.who
    );
    put!(
        s.registry_blobs,
        "INSERT INTO registry_blobs (digest, size, created_at) VALUES ($1, $2, $3)",
        |r| &r.digest,
        r.size,
        r.created_at
    );
    put!(
        s.image_repository_blobs,
        "INSERT INTO image_repository_blobs (repository_id, digest, created_at) VALUES ($1, $2, $3)",
        |r| r.repository_id,
        &r.digest,
        r.created_at
    );
    put!(
        s.image_manifests,
        "INSERT INTO image_manifests (repository_id, digest, media_type, artifact_type, subject,
            annotations, content, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        |r| r.repository_id,
        &r.digest,
        &r.media_type,
        &r.artifact_type,
        &r.subject,
        &r.annotations,
        &r.content,
        r.created_at
    );
    put!(
        s.image_manifest_refs,
        "INSERT INTO image_manifest_refs (repository_id, digest, ref) VALUES ($1, $2, $3)",
        |r| r.repository_id,
        &r.digest,
        &r.r#ref
    );
    put!(
        s.image_tags,
        "INSERT INTO image_tags (repository_id, tag, digest, updated_at) VALUES ($1, $2, $3, $4)",
        |r| r.repository_id,
        &r.tag,
        &r.digest,
        r.updated_at
    );
    put!(
        s.registry_uploads,
        "INSERT INTO registry_uploads (id, repository_id, created_at) VALUES ($1, $2, $3)",
        |r| r.id,
        r.repository_id,
        r.created_at
    );
    put!(
        s.dns_records,
        "INSERT INTO dns_records (name, type, value, ttl, created_at) VALUES ($1, $2, $3, $4, $5)",
        |r| &r.name,
        &r.r#type,
        &r.value,
        r.ttl,
        r.created_at
    );
    put!(
        s.tls_storage,
        "INSERT INTO tls_storage (key, value, modified) VALUES ($1, $2, $3)",
        |r| &r.key,
        &r.value,
        r.modified
    );
    put!(
        s.tls_locks,
        "INSERT INTO tls_locks (name, expires_at) VALUES ($1, $2)",
        |r| &r.name,
        r.expires_at
    );
    put!(
        s.sign_in_grants,
        "INSERT INTO sign_in_grants (id, device_hash, user_code, name, created_at, expires_at,
            approved_by, denied)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        |r| r.id,
        &r.device_hash,
        &r.user_code,
        &r.name,
        r.created_at,
        r.expires_at,
        r.approved_by,
        r.denied
    );
    put!(
        s.link_persons,
        "INSERT INTO link_persons (user_id, person_id) VALUES ($1, $2)",
        |r| r.user_id,
        r.person_id
    );
    put!(
        s.links,
        "INSERT INTO links (user_id, place_id, name, public_key, url, created_at, reached_at,
            reach_error, heard_at, synced_at, sync_error)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        |r| r.user_id,
        r.place_id,
        &r.name,
        &r.public_key,
        &r.url,
        r.created_at,
        r.reached_at,
        &r.reach_error,
        r.heard_at,
        r.synced_at,
        &r.sync_error
    );
    put!(
        s.link_records,
        "INSERT INTO link_records (user_id, kind, record_id, hlc, origin, digest, deleted)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
        |r| r.user_id,
        &r.kind,
        &r.record_id,
        r.hlc,
        r.origin,
        &r.digest,
        r.deleted
    );
    put!(
        s.link_settings,
        "INSERT INTO link_settings (user_id, bandwidth) VALUES ($1, $2)",
        |r| r.user_id,
        r.bandwidth
    );
    sqlx::query("UPDATE environment_revisions SET last = $1")
        .bind(s.last_revision)
        .execute(&mut *c)
        .await?;
    let next_audit = s
        .audit_events
        .iter()
        .map(|e| e.id + 1)
        .max()
        .unwrap_or(1)
        .max(s.next_audit_id);
    sqlx::query("DELETE FROM sqlite_sequence WHERE name = 'audit_events'")
        .execute(&mut *c)
        .await?;
    sqlx::query("INSERT INTO sqlite_sequence (name, seq) VALUES ('audit_events', $1)")
        .bind(next_audit - 1)
        .execute(&mut *c)
        .await?;
    // References Postgres sets to NULL when what they name is deleted, and
    // those to rows whose deletion it would refuse.
    for (table, column, parent) in [
        ("audit_events", "actor_user", "users"),
        ("audit_events", "actor_worker", "workers"),
        ("environments", "template_id", "templates"),
        ("environments", "worker_id", "workers"),
    ] {
        sqlx::query(&format!(
            "UPDATE {table} SET {column} = NULL
             WHERE {column} IS NOT NULL AND {column} NOT IN (SELECT id FROM {parent})"
        ))
        .execute(&mut *c)
        .await?;
    }
    let mut deleted = std::collections::BTreeMap::<String, u64>::new();
    loop {
        let broken: Vec<(String, i64)> =
            sqlx::query_as("SELECT \"table\", rowid FROM pragma_foreign_key_check")
                .fetch_all(&mut *c)
                .await?;
        if broken.is_empty() {
            break;
        }
        for (table, rowid) in broken {
            let n = sqlx::query(&format!("DELETE FROM \"{table}\" WHERE rowid = $1"))
                .bind(rowid)
                .execute(&mut *c)
                .await?
                .rows_affected();
            *deleted.entry(table).or_default() += n;
        }
    }
    for (table, n) in deleted {
        eprintln!(
            "hangar-server: importing the state file: {n} {table} rows name rows that are gone and are left out"
        );
    }
    tx.commit().await?;
    Ok(())
}

/// Gives an environment the next revision, as a change to it commits.
pub async fn revise(tx: &mut sqlx::SqliteConnection, id: Uuid) -> Result<()> {
    let r: i64 =
        sqlx::query_scalar("UPDATE environment_revisions SET last = last + 1 RETURNING last")
            .fetch_one(&mut *tx)
            .await?;
    sqlx::query("UPDATE environments SET revision = $1 WHERE id = $2")
        .bind(r)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

/// Where a read begun at this point would read to: the last revision
/// given.
pub async fn mark(tx: &mut sqlx::SqliteConnection) -> Result<Mark> {
    let r: i64 = sqlx::query_scalar("SELECT last FROM environment_revisions")
        .fetch_one(&mut *tx)
        .await?;
    Ok(Mark::Revision(r))
}

/// Whether e is a database error for breaking the unique index or
/// constraint named, or the one on the columns given (`table.column`):
/// SQLite names an index on expressions, and otherwise its columns.
pub fn violates(e: &sqlx::Error, index: &str, columns: &[&str]) -> bool {
    let sqlx::Error::Database(d) = e else {
        return false;
    };
    if !d.is_unique_violation() {
        return false;
    }
    let m = d.message();
    let Some(what) = m.strip_prefix("UNIQUE constraint failed: ") else {
        return false;
    };
    what == format!("index '{index}'") || (!columns.is_empty() && what == columns.join(", "))
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    use super::super::Get;
    use super::super::rows::*;
    use super::*;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// A state with a row in every table, references to rows that are
    /// gone (an audit event's actor, an environment's template, a default's
    /// pack and copy), and a pack's path twice.
    fn state() -> State {
        let at = Utc.with_ymd_and_hms(2026, 11, 19, 10, 0, 0).unwrap();
        let (person, worker, env, team, template, pack, copy, repo) =
            (id(1), id(2), id(3), id(4), id(5), id(6), id(7), id(8));
        let mut s = State::new();
        s.users.push(User {
            id: person,
            username: "ada".into(),
            display_name: "Ada".into(),
            password_hash: Some("hash".into()),
            is_admin: true,
            kind: "person".into(),
            disabled_at: None,
            created_at: at,
            theme: "dark".into(),
        });
        s.sessions.push(Session {
            token_hash: vec![1; 32],
            user_id: person,
            created_at: at,
            last_seen_at: at,
            expires_at: at,
        });
        s.access_tokens.push(AccessToken {
            id: id(10),
            user_id: person,
            name: "cli".into(),
            token_hash: vec![2; 32],
            created_at: at,
            last_used_at: Some(at),
            expires_at: None,
        });
        s.workers.push(Worker {
            id: worker,
            name: "w1".into(),
            credential_hash: vec![3; 32],
            labels: json!({"runtime": "krun"}),
            cpus: 8,
            memory_mib: 16384,
            unknown: json!([]),
            stats: Some(json!({"load": 1})),
            images: json!([]),
            gpu: None,
            desired_version: 4,
            last_seen_at: Some(at),
            revoked_at: None,
            created_at: at,
            monitor: "krun".into(),
            version: "v1".into(),
            features: vec!["suspend".into(), "gpu".into()],
        });
        for (n, actor) in [(40, Some(person)), (41, Some(id(99)))] {
            s.audit_events.push(AuditEvent {
                id: n,
                at,
                actor_user: actor,
                actor_worker: None,
                actor_name: "ada".into(),
                source_ip: "127.0.0.1".into(),
                action: "user.create".into(),
                target_type: "user".into(),
                target_id: person.to_string(),
                target_name: "ada".into(),
                subjects: vec![format!("user:{person}")],
                details: json!({}),
            });
        }
        s.next_audit_id = 50;
        s.last_revision = 7;
        s.teams.push(Team {
            id: team,
            slug: "core".into(),
            name: "Core".into(),
            description: String::new(),
            created_at: at,
        });
        s.team_members.push(TeamMember {
            team_id: team,
            user_id: person,
            role: "admin".into(),
        });
        s.templates.push(Template {
            id: template,
            owner_id: person,
            team_id: Some(team),
            name: "dev".into(),
            description: String::new(),
            visibility: "shared".into(),
            spec: json!({"image": "base"}),
            revision: 2,
            created_at: at,
            updated_at: at,
        });
        s.template_collaborators.push(Collaborator {
            template_id: template,
            who: person,
        });
        s.template_team_collaborators.push(Collaborator {
            template_id: template,
            who: team,
        });
        s.environments.push(Environment {
            id: env,
            short_id: "abc123".into(),
            owner_id: person,
            name: "one".into(),
            template_id: Some(id(98)),
            template_name: "gone".into(),
            template_revision: 1,
            spec: json!({}),
            image: "base".into(),
            image_digest: String::new(),
            image_pin_want: None,
            image_drop_rollback: false,
            image_update: None,
            image_rollback: None,
            ports_public: false,
            cpus: 2,
            memory_mib: 2048,
            desired: "running".into(),
            worker_id: Some(worker),
            phase: "running".into(),
            reason: String::new(),
            stats: None,
            progress: None,
            guest_agent: None,
            revision: 6,
            changed_by: None,
            created_at: at,
            updated_at: at,
        });
        s.worker_image_removals.push(ImageRemoval {
            worker_id: worker,
            r#ref: "base".into(),
            digest: "sha256:1".into(),
        });
        s.worker_environment_removals.push(EnvironmentRemoval {
            worker_id: worker,
            environment: "old".into(),
        });
        s.environment_tasks.push(Task {
            id: id(11),
            environment_id: env,
            text: "work".into(),
            set_by: "agent".into(),
            set_at: at,
            done_at: None,
        });
        s.agent_sessions.push(AgentSession {
            environment_id: env,
            id: "s1".into(),
            agent: "claude".into(),
            terminal: String::new(),
            state: "working".into(),
            step: String::new(),
            message: String::new(),
            since: at,
            seen_at: None,
        });
        s.editor_sessions.push(HostSession {
            token_hash: vec![4; 32],
            session_hash: vec![1; 32],
            environment_id: env,
            ticket: false,
            expires_at: at,
        });
        s.ssh_keys.push(SshKey {
            id: id(12),
            user_id: person,
            name: "k".into(),
            public_key: "ssh-ed25519 A".into(),
            private_key: vec![5; 8],
            created_at: at,
        });
        s.ssh_login_keys.push(LoginKey {
            id: id(13),
            user_id: person,
            name: "k".into(),
            public_key: "ssh-ed25519 B".into(),
            fingerprint: "SHA256:x".into(),
            created_at: at,
        });
        s.server_keys.push(ServerKey {
            name: "link-place".into(),
            value: vec![6; 8],
            sealed: true,
            created_at: at,
        });
        s.packs.push(Pack {
            id: pack,
            name: "Dotfiles".into(),
            description: String::new(),
            owner_id: Some(person),
            team_id: None,
            builtin: false,
            attach: "owner".into(),
            squash: "all_squash".into(),
            created_at: at,
            updated_at: at,
            same_name: None,
            merged: vec![id(20)],
            places: vec![id(21), id(22)],
            away: false,
        });
        for _ in 0..2 {
            s.pack_paths.push(PackPath {
                pack_id: pack,
                path: "~/.vimrc".into(),
                sensitive: false,
                send_every: Some(5),
            });
        }
        s.copies.push(Copy {
            id: copy,
            pack_id: pack,
            owner_id: Some(person),
            team_id: None,
            personal: true,
            name: String::new(),
            created_at: at,
            updated_at: at,
        });
        s.pack_defaults.push(PackDefault {
            user_id: person,
            pack_id: pack,
            copy_id: copy,
        });
        s.pack_defaults.push(PackDefault {
            user_id: person,
            pack_id: id(96),
            copy_id: id(97),
        });
        s.environment_copies.push(EnvironmentCopy {
            environment_id: env,
            pack_id: pack,
            copy_id: copy,
            chosen: true,
        });
        s.environment_routed.push(EnvironmentRouted {
            environment_id: env,
            copy_id: copy,
        });
        s.environment_conflicts.push(Conflict {
            environment_id: env,
            copy_id: copy,
            path: "~/.vimrc".into(),
            found_at: at,
            resolution: Some("shared".into()),
        });
        s.image_repositories.push(ImageRepository {
            id: repo,
            path: "ada/tools".into(),
            owner_id: Some(person),
            team_id: None,
            visibility: "private".into(),
            description: String::new(),
            created_at: at,
            updated_at: at,
        });
        s.image_repository_collaborators
            .push(RepositoryCollaborator {
                repository_id: repo,
                who: person,
            });
        s.image_repository_team_collaborators
            .push(RepositoryCollaborator {
                repository_id: repo,
                who: team,
            });
        s.registry_blobs.push(RegistryBlob {
            digest: "sha256:b".into(),
            size: 10,
            created_at: at,
        });
        s.image_repository_blobs.push(RepositoryBlob {
            repository_id: repo,
            digest: "sha256:b".into(),
            created_at: at,
        });
        s.image_manifests.push(Manifest {
            repository_id: repo,
            digest: "sha256:m".into(),
            media_type: "application/vnd.oci.image.manifest.v1+json".into(),
            artifact_type: String::new(),
            subject: None,
            annotations: Some(json!({"a": "b"})),
            content: b"{}".to_vec(),
            created_at: at,
        });
        s.image_manifest_refs.push(ManifestRef {
            repository_id: repo,
            digest: "sha256:m".into(),
            r#ref: "sha256:b".into(),
        });
        s.image_tags.push(ImageTag {
            repository_id: repo,
            tag: "latest".into(),
            digest: "sha256:m".into(),
            updated_at: at,
        });
        s.registry_uploads.push(RegistryUpload {
            id: id(14),
            repository_id: repo,
            created_at: at,
        });
        s.dns_records.push(DnsRecord {
            name: "_acme".into(),
            r#type: "TXT".into(),
            value: "v".into(),
            ttl: 60,
            created_at: at,
        });
        s.tls_storage.push(TlsEntry {
            key: "k".into(),
            value: vec![7; 4],
            modified: at,
        });
        s.tls_locks.push(TlsLock {
            name: "l".into(),
            expires_at: at,
        });
        s.sign_in_grants.push(SignInGrant {
            id: id(15),
            device_hash: vec![8; 32],
            user_code: "ABCD-EFGH".into(),
            name: "laptop".into(),
            created_at: at,
            expires_at: at,
            approved_by: Some(person),
            denied: false,
        });
        s.link_persons.push(LinkPerson {
            user_id: person,
            person_id: id(16),
        });
        s.links.push(Link {
            user_id: person,
            place_id: id(17),
            name: "home".into(),
            public_key: "pk".into(),
            url: "https://home".into(),
            created_at: at,
            reached_at: Some(at),
            reach_error: String::new(),
            heard_at: None,
            synced_at: None,
            sync_error: String::new(),
        });
        s.link_records.push(LinkRecord {
            user_id: person,
            kind: "pack".into(),
            record_id: pack.to_string(),
            hlc: 9,
            origin: id(17),
            digest: "d".into(),
            deleted: false,
        });
        s.link_settings.push(LinkSettings {
            user_id: person,
            bandwidth: 1000,
        });
        s
    }

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hangar-lite-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    async fn count(pool: &SqlitePool, table: &str) -> i64 {
        sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_json_state_file_is_imported_whole() {
        let dir = scratch();
        let s = state();
        std::fs::write(dir.join("state.json"), serde_json::to_vec(&s).unwrap()).unwrap();
        let db = open(&dir.join("state.json")).await.unwrap();
        assert!(dir.join("state.db").exists());
        assert!(!dir.join("state.json").exists());
        assert!(dir.join("state.json.imported").exists());
        let p = &db.writer;
        for (table, n) in [
            ("users", 3),
            ("sessions", 1),
            ("access_tokens", 1),
            ("workers", 1),
            ("audit_events", 2),
            ("teams", 1),
            ("team_members", 1),
            ("templates", 1),
            ("template_collaborators", 1),
            ("template_team_collaborators", 1),
            ("environments", 1),
            ("worker_image_removals", 1),
            ("worker_environment_removals", 1),
            ("environment_tasks", 1),
            ("agent_sessions", 1),
            ("editor_sessions", 1),
            ("ssh_keys", 1),
            ("ssh_login_keys", 1),
            ("server_keys", 1),
            ("packs", 2),
            ("pack_paths", s.pack_paths.len() as i64 - 1),
            ("copies", 1),
            ("pack_defaults", 1),
            ("environment_copies", 1),
            ("environment_routed", 1),
            ("environment_conflicts", 1),
            ("image_repositories", 1),
            ("image_repository_collaborators", 1),
            ("image_repository_team_collaborators", 1),
            ("registry_blobs", 1),
            ("image_repository_blobs", 1),
            ("image_manifests", 1),
            ("image_manifest_refs", 1),
            ("image_tags", 1),
            ("registry_uploads", 1),
            ("dns_records", 1),
            ("tls_storage", 1),
            ("tls_locks", 1),
            ("sign_in_grants", 1),
            ("link_persons", 1),
            ("links", 1),
            ("link_records", 1),
            ("link_settings", 1),
        ] {
            assert_eq!(count(p, table).await, n, "{table}");
        }
        // References to what is gone: cleared, or the row deleted.
        let actors: Vec<Option<Uuid>> =
            sqlx::query_scalar("SELECT actor_user FROM audit_events ORDER BY id")
                .fetch_all(p)
                .await
                .unwrap();
        assert_eq!(actors, vec![Some(id(1)), None]);
        let template: Option<Uuid> = sqlx::query_scalar("SELECT template_id FROM environments")
            .fetch_one(p)
            .await
            .unwrap();
        assert_eq!(template, None);
        // Rows read back as they went in.
        let w = sqlx::query(&format!(
            "SELECT {} FROM workers",
            super::super::workers::WORKER_COLUMNS
        ))
        .fetch_one(p)
        .await
        .unwrap();
        let w = super::super::workers::worker(&w).unwrap();
        assert_eq!(w.features, vec!["suspend".to_string(), "gpu".into()]);
        assert_eq!(w.labels, json!({"runtime": "krun"}));
        assert_eq!(w.last_seen_at, s.workers[0].last_seen_at);
        let k = sqlx::query("SELECT merged, places FROM packs WHERE id = $1")
            .bind(id(6))
            .fetch_one(p)
            .await
            .unwrap();
        let places: Vec<Uuid> = Get::try_get(&k, "places").unwrap();
        assert_eq!(places, vec![id(21), id(22)]);
        // Revisions and audit IDs go on from the file's.
        let mut c = p.acquire().await.unwrap();
        assert_eq!(mark(&mut c).await.unwrap(), Mark::Revision(7));
        let next: i64 = sqlx::query_scalar(
            "INSERT INTO audit_events (actor_name, action, target_type, target_id, subjects)
             VALUES ('x', 'y', 'z', 'w', '[]') RETURNING id",
        )
        .fetch_one(&mut *c)
        .await
        .unwrap();
        assert_eq!(next, 50);
        drop(c);
        // A second server is refused while the first holds it.
        assert!(open(&dir.join("state.db")).await.is_err());
        db.writer.close().await;
        db.readers.close().await;
        drop(db);
        // Opened again, it is the same database: nothing imported twice.
        let again = open(&dir.join("state.db")).await.unwrap();
        assert_eq!(count(&again.writer, "users").await, 3);
        again.writer.close().await;
        again.readers.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn event() -> AuditEvent {
        AuditEvent {
            id: 0,
            at: Utc::now(),
            actor_user: None,
            actor_worker: None,
            actor_name: "ada".into(),
            source_ip: String::new(),
            action: "user.create".into(),
            target_type: "user".into(),
            target_id: "1".into(),
            target_name: String::new(),
            subjects: vec!["user:1".into()],
            details: json!({}),
        }
    }

    #[tokio::test]
    async fn reads_go_on_beside_a_write() {
        let dir = scratch();
        let store =
            super::super::Store::open(&crate::config::Storage::Sqlite(dir.join("state.db")))
                .await
                .unwrap();
        let q = super::super::audit::Query {
            subjects: &["user:1".to_string()],
            any_of: None,
            before: i64::MAX,
            limit: 10,
        };
        let mut w = store.begin().await.unwrap();
        w.record(event()).await.unwrap();
        // Reads while it is open: none waits, and none sees what it has
        // not committed.
        let mut reads = vec![];
        for _ in 0..READERS {
            let mut r = tokio::time::timeout(Duration::from_secs(5), store.read())
                .await
                .expect("a read waited for the writer")
                .unwrap();
            assert!(r.audit_events(&q).await.unwrap().is_empty());
            reads.push(r);
        }
        // A read refuses to write.
        assert!(reads[0].record(event()).await.is_err());
        drop(reads);
        w.commit().await.unwrap();
        let mut r = store.read().await.unwrap();
        assert_eq!(r.audit_events(&q).await.unwrap().len(), 1);
        // A read that finds it must write is given a writer.
        let mut w = store.writer(r).await.unwrap();
        w.record(event()).await.unwrap();
        w.commit().await.unwrap();
        let mut r = store.read().await.unwrap();
        assert_eq!(r.audit_events(&q).await.unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_new_database_has_the_first_rows() {
        let dir = scratch();
        let db = open(&dir.join("state.db")).await.unwrap();
        assert_eq!(count(&db.writer, "users").await, 2);
        assert_eq!(count(&db.writer, "packs").await, 1);
        assert!(count(&db.writer, "pack_paths").await > 0);
        db.writer.close().await;
        db.readers.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
