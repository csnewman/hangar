//! Where the server keeps its state: Postgres for a deployment, or a JSON
//! file for one server on one machine, behind one set of transactions.
//!
//! Each domain's reads and writes are methods on `Tx`, written for both
//! backends (users.rs and the like beside this); the rows are `rows.rs`'s,
//! the Postgres tables' as `migrations/` makes them, which the
//! JSON file holds as they are.

pub mod audit;
mod catalog;
mod edge;
pub mod environments;
mod hosts;
mod json;
pub mod keys;
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
    Json(Arc<json::File>),
}

/// How far a reader of environments had read: the Postgres snapshot it
/// read from, or the JSON file's revisions up to one. A client keeps it as
/// a token and comes back with it, to be sent only what it has not seen.
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
    /// "j:" and a revision.
    pub fn parse(s: &str) -> Option<Mark> {
        if let Some(n) = s.strip_prefix("j:") {
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
            Mark::Revision(n) => format!("j:{n}"),
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
}

pub(super) enum Backend {
    Pg(sqlx::Transaction<'static, sqlx::Postgres>),
    Json(Box<json::Tx>),
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
            Storage::Json(path) => Kind::Json(Arc::new(json::File::open(path)?)),
        };
        Ok(Store { backend, signals })
    }

    /// Starts a transaction.
    pub async fn begin(&self) -> Result<Tx> {
        let inner = match &self.backend {
            Kind::Pg(pool) => Backend::Pg(pool.begin().await?),
            Kind::Json(file) => Backend::Json(Box::new(file.begin().await)),
        };
        Ok(Tx {
            inner,
            signalled: Vec::new(),
            signals: self.signals.clone(),
        })
    }
}

impl Tx {
    /// Keeps what the transaction wrote, and signals what it changed, to
    /// other servers sharing a database too: environments' changes reach
    /// them by the database's own triggers.
    pub async fn commit(self) -> Result<()> {
        match self.inner {
            Backend::Pg(mut tx) => {
                for s in &self.signalled {
                    let (channel, payload) = match s {
                        Signal::Worker(id) => (signals::WORKER_CHANNEL, id.to_string()),
                        Signal::Placement => (signals::PLACEMENT_CHANNEL, String::new()),
                        Signal::Shared => (signals::PACKS_CHANNEL, String::new()),
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
            Backend::Json(mut tx) => {
                for s in &self.signalled {
                    if let Signal::Environment(id) = s {
                        tx.revise(*id);
                    }
                }
                (*tx).commit()?
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
            Backend::Json(tx) => Ok(Mark::Revision(tx.work.last_revision)),
        }
    }

    /// Notes something changed, to signal once this commits.
    pub fn signal(&mut self, s: Signal) {
        if !self.signalled.contains(&s) {
            self.signalled.push(s);
        }
    }
}
