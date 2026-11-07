//! The Code tab's service in the guest, run by the agent as the
//! environment's user, on a unix socket the agent joins the browser to. One
//! runs per root folder; a tab may work in another folder, named in its
//! hello.

mod child;
mod connection;
mod error;
mod files;
mod frame;
mod git;
mod paths;
mod repos;
mod search;
mod settings;
mod store;
mod workspace;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tokio::net::UnixListener;
use tokio::signal::unix::{SignalKind, signal};

use files::Files;
use repos::Repos;
use search::Search;
use settings::SettingsFile;
use workspace::WorkspaceFile;

/// What serves one folder, shared by every tab that has it open: its files,
/// its git repositories, and searches in it.
pub struct Folder {
    pub root: PathBuf,
    pub files: Arc<Files>,
    pub repos: Arc<Repos>,
    pub search: Search,
}

/// The service's own: the folder it was started on, the one serving any
/// other folder, and what every tab shares.
pub struct Services {
    pub root: PathBuf,
    pub home: PathBuf,
    folders: Mutex<HashMap<PathBuf, Arc<Folder>>>,
    pub settings: SettingsFile,
    pub workspace: WorkspaceFile,
}

impl Services {
    pub fn folder(&self, dir: &Path) -> Arc<Folder> {
        self.folders
            .lock()
            .unwrap()
            .entry(dir.to_path_buf())
            .or_insert_with(|| {
                Arc::new(Folder {
                    root: dir.to_path_buf(),
                    files: Files::new(dir),
                    repos: Repos::new(dir),
                    search: Search::new(dir),
                })
            })
            .clone()
    }
}

pub async fn serve(socket: PathBuf, root: PathBuf) -> std::io::Result<()> {
    let root = paths::resolve(&std::env::current_dir()?, &root);
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    let s = Arc::new(Services {
        folders: Mutex::new(HashMap::new()),
        settings: SettingsFile::new(&home),
        workspace: WorkspaceFile::new(&root),
        home,
        root,
    });

    if socket.exists() {
        std::fs::remove_file(&socket)?;
    }
    let listener = UnixListener::bind(&socket)?;
    println!(
        "hangar-code: serving {} on {}",
        s.root.display(),
        socket.display()
    );

    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                tokio::spawn(connection::run(stream, s.clone()));
            }
            _ = term.recv() => return Ok(()),
            _ = int.recv() => return Ok(()),
        }
    }
}
