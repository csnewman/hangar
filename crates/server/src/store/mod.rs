//! Where the server keeps its state: Postgres for a deployment, or SQLite
//! for one server on one machine, behind one set of transactions.
//!
//! Each domain's reads and writes are methods on `Tx`, written for both
//! backends (users.rs and the like beside this), each in its own SQL; the
//! rows are `rows.rs`'s, the Postgres tables' as `migrations/` makes them,
//! which `migrations-sqlite/` mirrors. A row is read the same way from
//! either (`Get`).

pub mod audit;
mod catalog;
mod edge;
pub mod environments;
mod hosts;
mod json;
pub mod keys;
mod links;
mod lite;
mod packs;
mod pg;
mod registry;
pub mod report;
pub mod rows;
pub mod signals;
mod signin;
mod users;
pub mod workers;

use std::sync::Arc;

use sqlx::postgres::PgRow;
use sqlx::sqlite::SqliteRow;
use sqlx::{ColumnIndex, Row};

use crate::config::Storage;
use signals::{Signal, Signals};

/// A failure of the store itself, not of what was asked of it.
#[derive(Debug)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<sqlx::Error> for Error {
    fn from(e: sqlx::Error) -> Error {
        Error(format!("the database: {e}"))
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// The server's state, and whoever waits on its changes.
#[derive(Clone)]
pub struct Store {
    backend: Kind,
    pub signals: Arc<Signals>,
}

#[derive(Clone)]
enum Kind {
    Pg(sqlx::PgPool),
    Lite(Arc<lite::Lite>),
}

/// How far a reader of environments had read: the Postgres snapshot it
/// read from, or the SQLite store's revisions up to one. A client keeps
/// it as a token and comes back with it, to be sent only what it has not
/// seen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mark {
    /// The transactions finished before xmin, and those before xmax but
    /// for the ones still running then.
    Snapshot {
        xmin: u64,
        xmax: u64,
        running: Vec<u64>,
    },
    Revision(i64),
}

impl Mark {
    /// Reads a token: "p:" and a Postgres snapshot (XMIN:XMAX:XIP,...), or
    /// "r:" and a revision ("j:" being a JSON state file's, whose
    /// revisions its import keeps).
    pub fn parse(s: &str) -> Option<Mark> {
        if let Some(n) = s.strip_prefix("r:").or_else(|| s.strip_prefix("j:")) {
            return n.parse().ok().map(Mark::Revision);
        }
        let mut parts = s.strip_prefix("p:")?.splitn(3, ':');
        let xmin = parts.next()?.parse().ok()?;
        let xmax = parts.next()?.parse().ok()?;
        let running = parts
            .next()?
            .split(',')
            .filter(|x| !x.is_empty())
            .map(str::parse)
            .collect::<std::result::Result<_, _>>()
            .ok()?;
        Some(Mark::Snapshot {
            xmin,
            xmax,
            running,
        })
    }

    pub fn token(&self) -> String {
        match self {
            Mark::Snapshot {
                xmin,
                xmax,
                running,
            } => {
                let running: Vec<String> = running.iter().map(u64::to_string).collect();
                format!("p:{xmin}:{xmax}:{}", running.join(","))
            }
            Mark::Revision(n) => format!("r:{n}"),
        }
    }

    /// Whether a reader that had read to this mark has seen e as it is.
    pub fn saw(&self, e: &rows::Environment) -> bool {
        match self {
            Mark::Snapshot {
                xmin,
                xmax,
                running,
            } => e
                .changed_by
                .is_some_and(|x| x < *xmin || (x < *xmax && !running.contains(&x))),
            Mark::Revision(n) => e.revision <= *n,
        }
    }

    /// Whether this mark, a client's, can have been made by this store,
    /// whose current mark is `now`: one from another store, or one past
    /// the current, could say a client saw what it never did.
    pub fn before(&self, now: &Mark) -> bool {
        match (self, now) {
            (Mark::Snapshot { xmax: a, .. }, Mark::Snapshot { xmax: b, .. }) => a <= b,
            (Mark::Revision(a), Mark::Revision(b)) => a <= b,
            _ => false,
        }
    }
}

/// One transaction: what it reads is as of its start, and what it writes is
/// kept only once it commits; committing signals what it changed.
pub struct Tx {
    pub(super) inner: Backend,
    signalled: Vec<Signal>,
    signals: Arc<Signals>,
    /// Whether it may write: all of Postgres's do, SQLite's readers do not.
    writes: bool,
}

pub(super) enum Backend {
    Pg(sqlx::Transaction<'static, sqlx::Postgres>),
    Lite(sqlx::Transaction<'static, sqlx::Sqlite>),
}

impl Store {
    /// Opens the store storage names, bringing a database's schema up to
    /// date.
    pub async fn open(storage: &Storage) -> Result<Store> {
        let signals = Arc::<Signals>::default();
        let backend = match storage {
            Storage::Postgres(url) => {
                let pool = pg::open(url).await?;
                tokio::spawn(signals::listen(pool.clone(), signals.clone()));
                Kind::Pg(pool)
            }
            Storage::Sqlite(path) => Kind::Lite(Arc::new(lite::open(path).await?)),
        };
        Ok(Store { backend, signals })
    }

    /// Starts a transaction.
    pub async fn begin(&self) -> Result<Tx> {
        let inner = match &self.backend {
            Kind::Pg(pool) => Backend::Pg(pool.begin().await?),
            Kind::Lite(db) => Backend::Lite(db.writer.begin().await?),
        };
        Ok(self.tx(inner, true))
    }

    /// Starts a transaction that only reads. In SQLite it reads beside the
    /// one that writes, from the database as last committed, on a
    /// connection that refuses writes; in Postgres it is `begin`'s.
    pub async fn read(&self) -> Result<Tx> {
        Ok(match &self.backend {
            Kind::Pg(pool) => self.tx(Backend::Pg(pool.begin().await?), true),
            Kind::Lite(db) => self.tx(Backend::Lite(db.readers.begin().await?), false),
        })
    }

    /// A transaction that writes, for one that has read what it decides
    /// on: the same one where it may write (always, in Postgres), else a
    /// new one, which what it writes must suit whatever changed between.
    pub async fn writer(&self, tx: Tx) -> Result<Tx> {
        if tx.writes {
            return Ok(tx);
        }
        drop(tx);
        self.begin().await
    }

    fn tx(&self, inner: Backend, writes: bool) -> Tx {
        Tx {
            inner,
            signalled: Vec::new(),
            signals: self.signals.clone(),
            writes,
        }
    }
}

impl Tx {
    /// Keeps what the transaction wrote, and signals what it changed, to
    /// other servers sharing a database too: environments' changes reach
    /// them by the database's own triggers. In SQLite each environment
    /// signalled takes the next revision as it commits.
    pub async fn commit(self) -> Result<()> {
        match self.inner {
            Backend::Pg(mut tx) => {
                for s in &self.signalled {
                    let (channel, payload) = match s {
                        Signal::Worker(id) => (signals::WORKER_CHANNEL, id.to_string()),
                        Signal::Placement => (signals::PLACEMENT_CHANNEL, String::new()),
                        Signal::Shared => (signals::PACKS_CHANNEL, String::new()),
                        Signal::Records => (signals::RECORDS_CHANNEL, String::new()),
                        Signal::Environment(_) => continue,
                    };
                    sqlx::query("SELECT pg_notify($1, $2)")
                        .bind(channel)
                        .bind(payload)
                        .execute(&mut *tx)
                        .await?;
                }
                tx.commit().await?
            }
            Backend::Lite(mut tx) => {
                for s in &self.signalled {
                    if let Signal::Environment(id) = s {
                        lite::revise(&mut tx, *id).await?;
                    }
                }
                tx.commit().await?
            }
        }
        for s in self.signalled {
            self.signals.deliver(s);
        }
        Ok(())
    }

    /// Where a read begun after this would have read to, for a client to
    /// come back with (`Mark`).
    pub async fn mark(&mut self) -> Result<Mark> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                let s: String = sqlx::query_scalar("SELECT pg_current_snapshot()::text")
                    .fetch_one(&mut **tx)
                    .await?;
                Mark::parse(&format!("p:{s}"))
                    .ok_or_else(|| Error(format!("the snapshot {s} does not read")))
            }
            Backend::Lite(tx) => lite::mark(tx).await,
        }
    }

    /// Notes something changed, to signal once this commits.
    pub fn signal(&mut self, s: Signal) {
        if !self.signalled.contains(&s) {
            self.signalled.push(s);
        }
    }
}

/// A row of either backend, read by column name or position, as sqlx's
/// `Row::try_get`; the store's row functions take one, so one function
/// reads a row from Postgres and SQLite alike.
pub(super) trait Get {
    fn try_get<T: Field, I: ColumnIndex<PgRow> + ColumnIndex<SqliteRow>>(
        &self,
        index: I,
    ) -> std::result::Result<T, sqlx::Error>;
}

impl Get for PgRow {
    fn try_get<T: Field, I: ColumnIndex<PgRow> + ColumnIndex<SqliteRow>>(
        &self,
        index: I,
    ) -> std::result::Result<T, sqlx::Error> {
        T::pg(self, index)
    }
}

impl Get for SqliteRow {
    fn try_get<T: Field, I: ColumnIndex<PgRow> + ColumnIndex<SqliteRow>>(
        &self,
        index: I,
    ) -> std::result::Result<T, sqlx::Error> {
        T::lite(self, index)
    }
}

/// A column's type, as each backend keeps it: in Postgres as itself, in
/// SQLite as sqlx keeps it there, but for arrays, which are JSON arrays.
pub(super) trait Field: Sized {
    fn pg<I: ColumnIndex<PgRow>>(r: &PgRow, i: I) -> std::result::Result<Self, sqlx::Error>;
    fn lite<I: ColumnIndex<SqliteRow>>(
        r: &SqliteRow,
        i: I,
    ) -> std::result::Result<Self, sqlx::Error>;
}

macro_rules! fields {
    ($($t:ty),* $(,)?) => {$(
        impl Field for $t {
            fn pg<I: ColumnIndex<PgRow>>(r: &PgRow, i: I) -> std::result::Result<Self, sqlx::Error> {
                Row::try_get(r, i)
            }
            fn lite<I: ColumnIndex<SqliteRow>>(
                r: &SqliteRow,
                i: I,
            ) -> std::result::Result<Self, sqlx::Error> {
                Row::try_get(r, i)
            }
        }
    )*};
}

fields!(
    bool,
    i32,
    i64,
    Option<i32>,
    Option<i64>,
    String,
    Option<String>,
    Vec<u8>,
    Option<Vec<u8>>,
    uuid::Uuid,
    Option<uuid::Uuid>,
    serde_json::Value,
    Option<serde_json::Value>,
    chrono::DateTime<chrono::Utc>,
    Option<chrono::DateTime<chrono::Utc>>,
);

macro_rules! arrays {
    ($($t:ty),* $(,)?) => {$(
        impl Field for Vec<$t> {
            fn pg<I: ColumnIndex<PgRow>>(r: &PgRow, i: I) -> std::result::Result<Self, sqlx::Error> {
                Row::try_get(r, i)
            }
            fn lite<I: ColumnIndex<SqliteRow>>(
                r: &SqliteRow,
                i: I,
            ) -> std::result::Result<Self, sqlx::Error> {
                Row::try_get::<sqlx::types::Json<Vec<$t>>, _>(r, i).map(|j| j.0)
            }
        }
    )*};
}

arrays!(String, uuid::Uuid);
