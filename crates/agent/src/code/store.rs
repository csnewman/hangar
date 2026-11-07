//! The service's own small files: read whole, written beside themselves and
//! renamed over, so a reader never sees half of one.

use std::io;
use std::path::Path;

use serde_json::Value;
use tokio::io::AsyncWriteExt;

/// The file as JSON, None when it is missing or isn't JSON.
pub async fn read_json(path: &Path) -> Option<Value> {
    let bytes = tokio::fs::read(path).await.ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Writes `data` as the file, readable by all.
pub async fn write(path: &Path, data: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.tmp", std::process::id()));
    let mut f = tokio::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o644)
        .open(&tmp)
        .await?;
    f.write_all(data).await?;
    f.flush().await?;
    drop(f);
    tokio::fs::rename(&tmp, path).await
}
