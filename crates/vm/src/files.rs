//! A guest's shared files, served over NFS on the vsock port its agent
//! mounts them from.

use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::vsock::HybridListener;

/// The vsock port the agent mounts the shared files from.
pub const SHARED_FILES_PORT: u32 = 2049;

/// Owns the shared files in the guest: the user an image's environments
/// are used as, dev, which every image makes first.
pub const SHARED_FILES_UID: u32 = 1000;

fn file_id(p: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(p)
        .ok()
        .map(|m| (m.dev(), m.ino()))
}

/// Serves a guest its shared files until closed.
pub struct SharedFiles {
    srv: Arc<hangar_nfs::Server>,
    accept: tokio::task::JoinHandle<()>,
    socket: (std::path::PathBuf, Option<(u64, u64)>),
    conns: Arc<Mutex<Option<Vec<std::os::unix::net::UnixStream>>>>,
}

impl SharedFiles {
    /// Serves root, as view says the guest may reach it, on base's port
    /// for shared files. view is consulted on every call, so whoever holds
    /// it keeps it current.
    pub fn serve(
        base: &Path,
        cid: u32,
        root: &Path,
        view: Arc<dyn hangar_nfs::View>,
    ) -> io::Result<SharedFiles> {
        let srv = hangar_nfs::Server::new(hangar_nfs::Config {
            root,
            view,
            uid: SHARED_FILES_UID,
            gid: SHARED_FILES_UID,
        })?;
        let l = match HybridListener::listen(base, SHARED_FILES_PORT, cid) {
            Ok(l) => l,
            Err(e) => {
                srv.close();
                return Err(e);
            }
        };
        let socket = (l.path().to_path_buf(), file_id(l.path()));
        let conns: Arc<Mutex<Option<Vec<std::os::unix::net::UnixStream>>>> =
            Arc::new(Mutex::new(Some(vec![])));
        let accept = {
            let (srv, conns) = (srv.clone(), conns.clone());
            tokio::spawn(async move {
                loop {
                    let c = match l.accept(None).await {
                        Ok((c, _)) => c,
                        Err(_) => {
                            tokio::time::sleep(Duration::from_secs(1)).await;
                            continue;
                        }
                    };
                    let Ok(c) = c.into_std() else { continue };
                    if c.set_nonblocking(false).is_err() {
                        continue;
                    }
                    let Ok(held) = c.try_clone() else { continue };
                    match conns.lock().unwrap().as_mut() {
                        Some(all) => all.push(held),
                        None => return,
                    }
                    let srv = srv.clone();
                    std::thread::spawn(move || {
                        let _ = srv.serve(c);
                    });
                }
            })
        };
        Ok(SharedFiles {
            srv,
            accept,
            socket,
            conns,
        })
    }

    /// Stops serving: connections end, and the guest's state -- its open
    /// files and locks -- goes with them.
    pub fn close(&self) {
        // The listener goes with the task, in the runtime's own time; its
        // socket goes at once, unless another has taken the path.
        self.accept.abort();
        if file_id(&self.socket.0) == self.socket.1 {
            let _ = std::fs::remove_file(&self.socket.0);
        }
        if let Some(all) = self.conns.lock().unwrap().take() {
            for c in all {
                let _ = c.shutdown(std::net::Shutdown::Both);
            }
        }
        self.srv.close();
    }
}

impl Drop for SharedFiles {
    fn drop(&mut self) {
        self.close();
    }
}
