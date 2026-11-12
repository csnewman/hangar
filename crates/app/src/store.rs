//! Where the app keeps its places: their list in a file of the app's own,
//! each one's token in the OS keychain, and what was last known of each
//! (place::PlaceState) in the cache.

use std::path::{Path, PathBuf};

use crate::channel::Channel;
use crate::place::PlaceConfig;

/// The keychain's service name for places' tokens, each kept under its
/// place's ID: the app's name, so each channel signs in on its own.
fn keychain_service() -> &'static str {
    Channel::current().name()
}

/// Where the app's files are.
#[derive(Debug, Clone)]
pub struct Dirs {
    /// The places' list.
    pub config: PathBuf,
    /// What was last known of each place.
    pub cache: PathBuf,
}

impl Dirs {
    /// The user's own directories for the app, from the OS.
    pub fn user() -> Option<Dirs> {
        Some(Dirs {
            config: dirs::config_dir()?.join(Channel::current().name()),
            cache: dirs::cache_dir()?
                .join(Channel::current().name())
                .join("places"),
        })
    }

    /// Where the panels' sizes are kept.
    pub fn layout_file(&self) -> PathBuf {
        self.config.join("layout.json")
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config.join("settings.json")
    }

    fn places_file(&self) -> PathBuf {
        self.config.join("places.json")
    }

    /// The places, in the order the user keeps them; none when there is no
    /// list yet.
    pub fn places(&self) -> std::io::Result<Vec<PlaceConfig>> {
        match std::fs::read(self.places_file()) {
            Ok(b) => serde_json::from_slice(&b).map_err(std::io::Error::other),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
            Err(e) => Err(e),
        }
    }

    pub fn save_places(&self, places: &[PlaceConfig]) -> std::io::Result<()> {
        write_atomic(
            &self.places_file(),
            &serde_json::to_vec_pretty(places).map_err(std::io::Error::other)?,
        )
    }
}

fn write_atomic(path: &Path, b: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, b)?;
    std::fs::rename(tmp, path)
}

/// Where places' tokens are kept.
pub trait Tokens: Send + Sync {
    fn get(&self, place: &str) -> Option<String>;
    fn set(&self, place: &str, token: &str) -> Result<(), String>;
    fn delete(&self, place: &str);
}

/// The OS keychain: the macOS Keychain, Windows's Credential Manager, or a
/// Secret Service on Linux.
pub struct Keychain;

impl Tokens for Keychain {
    fn get(&self, place: &str) -> Option<String> {
        keyring::Entry::new(keychain_service(), place)
            .ok()?
            .get_password()
            .ok()
    }

    fn set(&self, place: &str, token: &str) -> Result<(), String> {
        keyring::Entry::new(keychain_service(), place)
            .and_then(|e| e.set_password(token))
            .map_err(|e| e.to_string())
    }

    fn delete(&self, place: &str) {
        if let Ok(e) = keyring::Entry::new(keychain_service(), place) {
            let _ = e.delete_credential();
        }
    }
}

/// Tokens kept in memory, for tests and for a place given on the command
/// line.
#[derive(Default)]
pub struct MemoryTokens(std::sync::Mutex<std::collections::HashMap<String, String>>);

impl Tokens for MemoryTokens {
    fn get(&self, place: &str) -> Option<String> {
        self.0.lock().unwrap().get(place).cloned()
    }

    fn set(&self, place: &str, token: &str) -> Result<(), String> {
        self.0
            .lock()
            .unwrap()
            .insert(place.to_string(), token.to_string());
        Ok(())
    }

    fn delete(&self, place: &str) {
        self.0.lock().unwrap().remove(place);
    }
}

/// A place's ID from its address: the host and port, which is what makes
/// one server another.
pub fn place_id(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split('/').next().unwrap_or(rest).to_ascii_lowercase()
}

/// A server's address as the user typed it, made whole: https unless it
/// says otherwise, with no trailing slash.
pub fn normalise_url(input: &str) -> Option<String> {
    let s = input.trim().trim_end_matches('/');
    if s.is_empty() || s.contains(char::is_whitespace) {
        return None;
    }
    let s = if s.contains("://") {
        s.to_string()
    } else {
        format!("https://{s}")
    };
    let (scheme, rest) = s.split_once("://")?;
    if !(scheme == "https" || scheme == "http") || rest.is_empty() {
        return None;
    }
    Some(s)
}
