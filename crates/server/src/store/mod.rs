//! Where the server keeps its state: Postgres for a deployment, or a JSON
//! file for one server on one machine, behind one set of transactions.
//!
//! Each domain's reads and writes are methods on `Tx`, written for both
//! backends (users.rs and the like beside this); the rows are `rows.rs`'s,
//! the Postgres tables' as `internal/db/migrations` makes them, which the
//! JSON file holds as they are.

mod json;
mod pg;
pub mod report;
pub mod rows;
mod users;
pub mod workers;

use std::sync::Arc;

use crate::config::Storage;

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

/// The server's state, and the workers waiting on what they are asked to
/// run.
#[derive(Clone)]
pub struct Store {
    backend: Kind,
    pub waiters: Arc<Waiters>,
}

#[derive(Clone)]
enum Kind {
    Pg(sqlx::PgPool),
    Json(Arc<json::File>),
}

/// One transaction: what it reads is as of its start, and what it writes is
/// kept only once it commits; committing wakes the workers whose desired
/// sets it changed.
pub struct Tx {
    pub(super) inner: Backend,
    bumped: Vec<uuid::Uuid>,
    waiters: Arc<Waiters>,
}

pub(super) enum Backend {
    Pg(sqlx::Transaction<'static, sqlx::Postgres>),
    Json(json::Tx),
}

/// Workers waiting for their desired set to change, each woken by a commit
/// that bumped it.
#[derive(Default)]
pub struct Waiters(
    std::sync::Mutex<std::collections::HashMap<uuid::Uuid, Arc<tokio::sync::Notify>>>,
);

impl Waiters {
    /// What wakes for the worker with id.
    pub fn watch(&self, id: uuid::Uuid) -> Arc<tokio::sync::Notify> {
        self.0.lock().unwrap().entry(id).or_default().clone()
    }

    fn wake(&self, id: uuid::Uuid) {
        if let Some(n) = self.0.lock().unwrap().get(&id) {
            n.notify_waiters();
        }
    }
}

impl Store {
    /// Opens the store storage names, bringing a database's schema up to
    /// date.
    pub async fn open(storage: &Storage) -> Result<Store> {
        let backend = match storage {
            Storage::Postgres(url) => Kind::Pg(pg::open(url).await?),
            Storage::Json(path) => Kind::Json(Arc::new(json::File::open(path)?)),
        };
        Ok(Store {
            backend,
            waiters: Arc::default(),
        })
    }

    /// Starts a transaction.
    pub async fn begin(&self) -> Result<Tx> {
        let inner = match &self.backend {
            Kind::Pg(pool) => Backend::Pg(pool.begin().await?),
            Kind::Json(file) => Backend::Json(file.begin().await),
        };
        Ok(Tx {
            inner,
            bumped: Vec::new(),
            waiters: self.waiters.clone(),
        })
    }
}

impl Tx {
    /// Keeps what the transaction wrote, and wakes the workers it bumped.
    pub async fn commit(self) -> Result<()> {
        match self.inner {
            Backend::Pg(tx) => tx.commit().await?,
            Backend::Json(tx) => tx.commit()?,
        }
        for id in self.bumped {
            self.waiters.wake(id);
        }
        Ok(())
    }

    /// Notes a worker whose desired set changed, to wake on commit.
    pub(super) fn bumped(&mut self, id: uuid::Uuid) {
        self.bumped.push(id);
    }
}
