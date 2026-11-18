//! Each environment's view of the shared files, as the server gives it.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hangar_api::{EnvironmentFiles, Squash};
use hangar_vm::log::Log;

use crate::client::Client;

/// How often an environment's view of the shared files is looked up again,
/// so a pack its owner is given reaches it while it runs.
pub const FILES_REFRESH: Duration = Duration::from_secs(5);

/// An environment's view of the shared files, as the server last gave it.
/// Until it has, the environment reaches nothing. It gathers the copies
/// the environment's calls change, for the server to be told at once.
#[derive(Default)]
pub struct FilesView {
    files: Mutex<EnvironmentFiles>,
    changed: Mutex<BTreeSet<String>>,
    wake: Arc<tokio::sync::Notify>,
}

impl FilesView {
    pub fn set(&self, files: EnvironmentFiles) {
        *self.files.lock().unwrap() = files;
    }
}

impl hangar_nfs::View for FilesView {
    fn sets(&self) -> Vec<String> {
        self.files.lock().unwrap().sets.clone()
    }

    /// Whether a path in a copy is kept from the environment: one hidden,
    /// or inside one, whether given as a directory or not.
    fn hidden(&self, set: &str, path: &[u8]) -> bool {
        let files = self.files.lock().unwrap();
        let Some(hidden) = files.hidden.get(set) else {
            return false;
        };
        hidden.iter().any(|h| {
            let h = h.trim_end_matches('/').as_bytes();
            path == h || (path.starts_with(h) && path.get(h.len()) == Some(&b'/'))
        })
    }

    /// How a copy's callers act on its files, as its pack says.
    fn squash(&self, set: &str) -> hangar_nfs::Squash {
        match self.files.lock().unwrap().squash.get(set) {
            Some(Squash::Root) => hangar_nfs::Squash::Root,
            Some(Squash::None) => hangar_nfs::Squash::None,
            _ => hangar_nfs::Squash::All,
        }
    }

    fn changed(&self, set: &str) {
        let mut changed = self.changed.lock().unwrap();
        if !changed.contains(set) {
            changed.insert(set.to_string());
            self.wake.notify_one();
        }
    }
}

/// env's view of the shared files, looked up at once and every FILES_REFRESH
/// for as long as anything holds the view; the copies its calls change are
/// told to the server as they change, those changed while it is being told
/// together after.
pub fn view(client: &Client, log: &Log, env: &str) -> Arc<FilesView> {
    let v = Arc::new(FilesView::default());
    let weak = Arc::downgrade(&v);
    let (client, log, env) = (
        client.clone(),
        log.with("environment", env),
        env.to_string(),
    );
    {
        let (weak, client, log, env) = (weak.clone(), client.clone(), log.clone(), env.clone());
        let wake = v.wake.clone();
        tokio::spawn(async move {
            loop {
                let woken = wake.notified();
                tokio::pin!(woken);
                woken.as_mut().enable();
                let Some(v) = weak.upgrade() else { return };
                let sets: Vec<String> = std::mem::take(&mut *v.changed.lock().unwrap())
                    .into_iter()
                    .collect();
                drop(v);
                if !sets.is_empty()
                    && let Err(e) = client.files_changed(&env, sets).await
                {
                    log.warn("telling the server of changed shared files", &[("err", &e)]);
                }
                // Woken at least every FILES_REFRESH, so the task ends with the view.
                let _ = tokio::time::timeout(FILES_REFRESH, woken).await;
            }
        });
    }
    tokio::spawn(async move {
        let mut t = tokio::time::interval(FILES_REFRESH);
        loop {
            t.tick().await;
            if weak.strong_count() == 0 {
                return;
            }
            let got = client.environment_files(&env).await;
            let Some(v) = weak.upgrade() else { return };
            match got {
                Ok(files) => v.set(files),
                Err(e) => log.warn("looking up an environment's shared files", &[("err", &e)]),
            }
        }
    });
    v
}
