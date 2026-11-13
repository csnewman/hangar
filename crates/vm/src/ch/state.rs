//! A backend's record of what its guest holds, written on request.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::launch::Backend;
use crate::err;

/// Asks a backend to write what its guest is holding, and waits for it to
/// have done so.
///
/// The backends write on SIGUSR1, into a temporary file renamed over the
/// real one, so the file appearing is the sign the write is complete. The
/// old one is removed first, or it would look like success before the
/// backend had begun.
pub async fn save_state(b: &Backend, path: &Path, timeout: Duration) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    b.signal(libc::SIGUSR1)
        .map_err(|e| err(format!("asking the backend to save its state: {e}")))?;
    wait_file(path, timeout).await
}

/// Where a backend says a restored session is whole again: the state file
/// with its extension replaced, as the backend derives it.
pub fn ready_path(state: &Path) -> PathBuf {
    state.with_extension("ready")
}

/// Where a backend's log goes: beside its state, so what it did to that
/// state can be read alongside it.
pub fn log_path_for(state: &Path) -> PathBuf {
    state.with_extension("log")
}

/// Waits for path to appear.
pub async fn wait_file(path: &Path, timeout: Duration) -> io::Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if path.exists() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Err(err(format!(
        "{} did not appear within {timeout:?}",
        path.display()
    )))
}
