//! Where the server keeps its state: Postgres for a deployment, or a JSON
//! file for one server on one machine, behind one set of transactions.
//!
//! Each domain's reads and writes are methods on `Tx`, written for both
//! backends (users.rs and the like beside this); the rows are `rows.rs`'s,
//! the Postgres tables' as `internal/db/migrations` makes them, which the
//! JSON file holds as they are.

mod json;
mod pg;
pub mod rows;
mod users;

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

/// The server's state.
#[derive(Clone)]
pub enum Store {
    Pg(sqlx::PgPool),
    Json(Arc<json::File>),
}

/// One transaction: what it reads is as of its start, and what it writes is
/// kept only once it commits.
pub enum Tx {
    Pg(sqlx::Transaction<'static, sqlx::Postgres>),
    Json(json::Tx),
}

impl Store {
    /// Opens the store storage names, bringing a database's schema up to
    /// date.
    pub async fn open(storage: &Storage) -> Result<Store> {
        match storage {
            Storage::Postgres(url) => Ok(Store::Pg(pg::open(url).await?)),
            Storage::Json(path) => Ok(Store::Json(Arc::new(json::File::open(path)?))),
        }
    }

    /// Starts a transaction.
    pub async fn begin(&self) -> Result<Tx> {
        match self {
            Store::Pg(pool) => Ok(Tx::Pg(pool.begin().await?)),
            Store::Json(file) => Ok(Tx::Json(file.begin().await)),
        }
    }
}

impl Tx {
    /// Keeps what the transaction wrote.
    pub async fn commit(self) -> Result<()> {
        match self {
            Tx::Pg(tx) => Ok(tx.commit().await?),
            Tx::Json(tx) => tx.commit(),
        }
    }
}
