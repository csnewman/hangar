//! Where the server keeps its state: Postgres for a deployment, or a JSON
//! file for one server on one machine, behind one set of transactions.
//!
//! Each domain's reads and writes are methods on `Tx`, written for both
//! backends (users.rs and the like beside this); the rows are `rows.rs`'s,
//! the Postgres tables' as `internal/db/migrations` makes them, which the
//! JSON file holds as they are.

pub mod audit;
mod catalog;
pub mod environments;
mod hosts;
mod json;
mod pg;
pub mod report;
pub mod rows;
pub mod signals;
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
            Backend::Json(tx) => (*tx).commit()?,
        }
        for s in self.signalled {
            self.signals.deliver(s);
        }
        Ok(())
    }

    /// Notes something changed, to signal once this commits.
    pub fn signal(&mut self, s: Signal) {
        if !self.signalled.contains(&s) {
            self.signalled.push(s);
        }
    }
}
