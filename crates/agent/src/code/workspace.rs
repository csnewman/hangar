use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Map, Value, json};
use tokio::sync::broadcast;

use super::error::{Error, Result, at};
use super::paths::resolve;
use super::store;

/// The most projects a workspace holds.
const MAX_FOLDERS: usize = 32;

/// Where the workspace is kept unless `HANGAR_WORKSPACE` names another, as
/// the tests do.
const DEFAULT_PATH: &str = "/var/lib/hangar-workspace/projects.code-workspace";

#[derive(Clone, Debug, Serialize)]
pub struct WorkspaceState {
    pub folders: Vec<String>,
    /// Whether the list has been set, by the worker from the template's
    /// projects or by a tab; until it has, it is the service's own folder.
    pub saved: bool,
}

/// The workspace is the environment's list of projects: the folders the
/// Code tab shows side by side, each with its own files, repositories and
/// language servers. It is the environment's, not the person's, so it is
/// kept in the guest outside what the profile shares, and everyone using the
/// environment sees the same list. It is a VS Code workspace file, and the
/// worker writes it first with the projects the environment's template names
/// (api.WorkspaceFile).
pub struct WorkspaceFile {
    root: PathBuf,
    path: PathBuf,
    changed: broadcast::Sender<WorkspaceState>,
}

impl WorkspaceFile {
    pub fn new(root: &Path) -> Self {
        let path = std::env::var_os("HANGAR_WORKSPACE")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_PATH));
        WorkspaceFile {
            root: root.to_path_buf(),
            path,
            changed: broadcast::channel(16).0,
        }
    }

    pub async fn get(&self) -> WorkspaceState {
        if let Some(Value::Array(folders)) = store::read_json(&self.path)
            .await
            .and_then(|mut v| v.get_mut("folders").map(Value::take))
        {
            let dir = self.path.parent().unwrap_or(Path::new("/"));
            // A folder's path may be relative to the file, as VS Code allows.
            let folders: Vec<String> = folders
                .iter()
                .filter_map(|f| f.get("path")?.as_str())
                .filter(|p| !p.is_empty())
                .map(|p| resolve(dir, Path::new(p)).display().to_string())
                .collect();
            if !folders.is_empty() {
                return WorkspaceState {
                    folders,
                    saved: true,
                };
            }
        }
        WorkspaceState {
            folders: vec![self.root.display().to_string()],
            saved: false,
        }
    }

    /// Replaces the list: each an absolute path to a folder, once each.
    /// Whatever else VS Code keeps in the file, its settings say, stays.
    pub async fn set(&self, folders: &Value) -> Result<WorkspaceState> {
        let Value::Array(folders) = folders else {
            return Err(Error::msg("folders is a list"));
        };
        let mut out: Vec<String> = Vec::new();
        for f in folders {
            let p = f.as_str().filter(|p| p.starts_with('/'));
            let Some(p) = p else {
                let shown = f
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| f.to_string());
                return Err(Error::msg(format!("{shown} is not an absolute path")));
            };
            let dir = resolve(Path::new("/"), Path::new(p));
            match tokio::fs::metadata(&dir).await {
                Err(_) => return Err(Error::msg(format!("{} does not exist", dir.display()))),
                Ok(m) if !m.is_dir() => {
                    return Err(Error::msg(format!("{} is not a folder", dir.display())));
                }
                Ok(_) => {}
            }
            let dir = dir.display().to_string();
            if !out.contains(&dir) {
                out.push(dir);
            }
        }
        if out.len() > MAX_FOLDERS {
            return Err(Error::msg(format!(
                "a workspace holds at most {MAX_FOLDERS} folders"
            )));
        }
        let mut body = match store::read_json(&self.path).await {
            Some(Value::Object(m)) => m,
            _ => Map::new(),
        };
        body.insert(
            "folders".into(),
            Value::Array(out.iter().map(|p| json!({ "path": p })).collect()),
        );
        let text =
            serde_json::to_string_pretty(&body).map_err(|e| Error::msg(e.to_string()))? + "\n";
        store::write(&self.path, text.as_bytes())
            .await
            .map_err(at(&self.path))?;
        let state = WorkspaceState {
            folders: out,
            saved: true,
        };
        let _ = self.changed.send(state.clone());
        Ok(state)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<WorkspaceState> {
        self.changed.subscribe()
    }

    /// The git repositories in the home folder, for adding to the
    /// workspace: those a few folders down, not inside another.
    pub async fn repositories(&self, home: &Path) -> Vec<String> {
        scan(home).await
    }
}

// Past these, a folder is a home directory or a disk, not a workspace: what
// was found is shown and the rest left.
const MAX_REPOS: usize = 64;
const SCAN_DEPTH: usize = 3;
const SCAN_DIRS: usize = 4000;
const SKIP: [&str; 5] = ["node_modules", "target", "dist", "vendor", "__pycache__"];

/// The repositories under `root`, breadth first: a folder holding `.git` is
/// one, and isn't looked inside.
pub async fn scan(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    let mut read = 0;
    for _ in 0..=SCAN_DEPTH {
        if dirs.is_empty() {
            break;
        }
        let mut next = Vec::new();
        for dir in dirs {
            read += 1;
            if read > SCAN_DIRS || found.len() >= MAX_REPOS {
                return found;
            }
            let Ok(mut rd) = tokio::fs::read_dir(&dir).await else {
                continue;
            };
            let mut entries = Vec::new();
            while let Ok(Some(e)) = rd.next_entry().await {
                let is_dir = e.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
                entries.push((e.file_name().to_string_lossy().into_owned(), is_dir));
            }
            if dir != root && entries.iter().any(|(n, _)| n == ".git") {
                found.push(dir.display().to_string());
                continue;
            }
            for (name, is_dir) in entries {
                if is_dir && !name.starts_with('.') && !SKIP.contains(&name.as_str()) {
                    next.push(dir.join(name));
                }
            }
        }
        dirs = next;
    }
    found
}
