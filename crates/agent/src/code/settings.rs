use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use tokio::sync::broadcast;

use super::error::{Error, Result, at};
use super::store;

/// The largest settings file kept.
const MAX_SIZE: usize = 64 << 10;

/// The Code tab's settings: a file in the user's home, which the profile
/// shares by default, so they follow the person to each environment. Every
/// tab is told when any tab changes them.
pub struct SettingsFile {
    path: PathBuf,
    changed: broadcast::Sender<Value>,
}

impl SettingsFile {
    pub fn new(home: &Path) -> Self {
        SettingsFile {
            path: home.join(".config/hangar/code.json"),
            changed: broadcast::channel(16).0,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The settings, empty when there are none or they are unreadable.
    pub async fn get(&self) -> Map<String, Value> {
        match store::read_json(&self.path).await {
            Some(Value::Object(m)) => m,
            _ => Map::new(),
        }
    }

    /// Replaces the settings.
    pub async fn set(&self, s: &Value) -> Result<()> {
        if !s.is_object() {
            return Err(Error::msg("settings are an object"));
        }
        let text = serde_json::to_string_pretty(s).map_err(|e| Error::msg(e.to_string()))? + "\n";
        if text.len() > MAX_SIZE {
            return Err(Error::msg("the settings are too large"));
        }
        store::write(&self.path, text.as_bytes())
            .await
            .map_err(at(&self.path))?;
        let _ = self.changed.send(s.clone());
        Ok(())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.changed.subscribe()
    }
}
