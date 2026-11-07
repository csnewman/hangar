//! Which folders' own code the service may run. A language server runs the
//! code of the folder it serves -- rust-analyzer its build scripts and proc
//! macros, TypeScript's the plugins its tsconfig names, Python's the
//! interpreter in its .venv -- so one starts only in a trusted folder, and
//! opening a folder to read it runs nothing of it.
//!
//! The worker writes the folders the environment's template trusts (its
//! repositories and projects, unless it trusts nothing) to a file the
//! service is told of; the person trusts more from the Code tab, kept in
//! their home in a file the profile does not share, so trusting a folder in
//! one environment trusts it in that one alone. A service told of no file
//! trusts every folder.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tokio::sync::broadcast;

use super::error::{Result, at};
use super::paths::{normalize, within};
use super::store;

pub struct Trust {
    /// The worker's list, or None to trust everything.
    system: Option<PathBuf>,
    /// The folders the person has trusted.
    granted: PathBuf,
    home: PathBuf,
    changed: broadcast::Sender<Value>,
}

impl Trust {
    pub fn new(system: Option<PathBuf>, home: &Path) -> Self {
        Trust {
            system,
            granted: home.join(".local/state/hangar/trusted-folders.json"),
            home: home.to_path_buf(),
            changed: broadcast::channel(16).0,
        }
    }

    /// Whether code in `folder` may run: it is a trusted folder or inside
    /// one.
    pub async fn trusts(&self, folder: &Path) -> bool {
        let Some(system) = &self.system else {
            return true;
        };
        let folder = normalize(folder);
        let mut trusted = self.read(system).await;
        trusted.extend(self.read(&self.granted).await);
        trusted.iter().any(|t| within(t, &folder))
    }

    /// Trusts `folder`, and tells every tab.
    pub async fn grant(&self, folder: &Path) -> Result<()> {
        let folder = normalize(folder);
        let mut granted = self.read(&self.granted).await;
        if !granted.contains(&folder) {
            granted.push(folder);
            let list: Vec<String> = granted.iter().map(|p| p.display().to_string()).collect();
            let text = serde_json::to_string_pretty(&list).unwrap_or_default() + "\n";
            store::write(&self.granted, text.as_bytes())
                .await
                .map_err(at(&self.granted))?;
        }
        let _ = self.changed.send(json!({}));
        Ok(())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.changed.subscribe()
    }

    /// A list of folders, absolute or under ~/; none when the file is
    /// missing or unreadable.
    async fn read(&self, file: &Path) -> Vec<PathBuf> {
        let Some(Value::Array(list)) = store::read_json(file).await else {
            return Vec::new();
        };
        list.iter()
            .filter_map(Value::as_str)
            .filter_map(|p| match p.strip_prefix("~/") {
                Some(rest) => Some(self.home.join(rest)),
                None if p == "~" => Some(self.home.clone()),
                None if p.starts_with('/') => Some(PathBuf::from(p)),
                None => None,
            })
            .map(|p| normalize(&p))
            .collect()
    }
}
