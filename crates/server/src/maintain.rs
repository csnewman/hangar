//! What the server does now and then on its own: deleting expired
//! sessions, which are refused whether or not they are deleted, so this
//! only keeps the store small; and the registry's abandoned uploads and
//! what nothing keeps.

use std::time::Duration;

use chrono::Utc;

use crate::http::Shared;

/// How often.
const EVERY: Duration = Duration::from_secs(60 * 60);

/// How old what the registry collects must be: nothing made within it
/// goes, so no push in progress is caught halfway.
const GRACE: chrono::Duration = chrono::Duration::hours(1);

/// Runs until the process ends.
pub async fn run(app: Shared) {
    loop {
        once(&app).await;
        tokio::time::sleep(EVERY).await;
    }
}

async fn once(app: &Shared) {
    let pruned = async {
        let mut tx = app.store.begin().await?;
        let now = Utc::now();
        let sessions = tx.prune_sessions(now).await?;
        let hosts = tx.prune_host_sessions(now).await?;
        tx.commit().await?;
        Ok::<_, crate::store::Error>((sessions, hosts))
    };
    match pruned.await {
        Ok((0, 0)) => {}
        Ok((sessions, hosts)) => {
            eprintln!(
                "hangar-server: pruned {sessions} expired sessions and {hosts} environment host sessions"
            )
        }
        Err(e) => eprintln!("hangar-server: pruning sessions: {e}"),
    }
    let Some(blobs) = &app.blobs else {
        return;
    };
    match crate::registry::prune_uploads(&app.store, blobs).await {
        Ok(0) => {}
        Ok(n) => eprintln!("hangar-server: pruned {n} abandoned registry uploads"),
        Err(e) => eprintln!("hangar-server: pruning registry uploads: {e}"),
    }
    match crate::registry::collect(&app.store, blobs, GRACE).await {
        Ok(c) if c.manifests == 0 && c.blobs == 0 => {}
        Ok(c) => eprintln!(
            "hangar-server: collected the registry's unused manifests ({}) and blobs ({}, {} bytes)",
            c.manifests, c.blobs, c.bytes
        ),
        Err(e) => {
            eprintln!("hangar-server: collecting the registry's unused manifests and blobs: {e}")
        }
    }
}
