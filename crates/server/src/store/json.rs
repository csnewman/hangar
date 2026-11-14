//! The JSON-file backend, for one server on one machine: the whole state in
//! memory, one transaction at a time, each on a copy that commit swaps in
//! and writes to the file atomically (a temporary file beside it, synced,
//! then renamed over it). One process holds the file: a lock file beside it
//! says which.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, OwnedMutexGuard};

use super::rows::{AccessToken, AuditEvent, Session, User};
use super::{Error, Result};

/// Everything the server keeps, as the Postgres tables hold it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub users: Vec<User>,
    pub sessions: Vec<Session>,
    pub access_tokens: Vec<AccessToken>,
    pub audit_events: Vec<AuditEvent>,
    /// The ID the next audit event takes, as Postgres's bigserial gives one.
    pub next_audit_id: i64,
}

/// The file, open.
pub struct File {
    path: PathBuf,
    state: Arc<Mutex<State>>,
    /// Held while the server runs, so a second one fails to start.
    _lock: std::fs::File,
}

/// A transaction: the state, held, and the copy it works on.
pub struct Tx {
    held: OwnedMutexGuard<State>,
    pub(super) work: State,
    path: PathBuf,
}

impl File {
    /// Opens the state at path, an empty one if there is no file yet.
    pub fn open(path: &Path) -> Result<File> {
        let lock_path = path.with_extension("lock");
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(|e| Error(format!("{}: {e}", lock_path.display())))?;
        lock.try_lock().map_err(|_| {
            Error(format!(
                "{} is held by another server: one server keeps a state file",
                path.display()
            ))
        })?;
        let state = match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| Error(format!("{}: {e}", path.display())))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(e) => return Err(Error(format!("{}: {e}", path.display()))),
        };
        Ok(File {
            path: path.to_path_buf(),
            state: Arc::new(Mutex::new(state)),
            _lock: lock,
        })
    }

    /// Waits for the state, and starts a transaction on a copy of it.
    pub async fn begin(&self) -> Tx {
        let held = self.state.clone().lock_owned().await;
        let work = held.clone();
        Tx {
            held,
            work,
            path: self.path.clone(),
        }
    }
}

impl Tx {
    /// Writes the copy to the file, then makes it the state.
    pub fn commit(mut self) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(&self.work)
            .map_err(|e| Error(format!("writing the state: {e}")))?;
        let tmp = self.path.with_extension("json.new");
        let write = || -> std::io::Result<()> {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
            std::fs::rename(&tmp, &self.path)
        };
        write().map_err(|e| Error(format!("{}: {e}", self.path.display())))?;
        *self.held = std::mem::take(&mut self.work);
        Ok(())
    }
}
