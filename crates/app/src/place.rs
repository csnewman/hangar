//! A place: a Hangar the app reaches, its connection and its last-known
//! state. Each server place keeps its own connection on a thread, so a slow
//! or unreachable one never holds up the others (docs/places.md §3).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::api::{self, Client, Environment, Event, Me};

/// A place the app knows: a server at an address.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PlaceConfig {
    pub id: String,
    pub name: String,
    pub url: String,
    /// The SHA-256 of the certificate its HTTPS presents, taken alone, for
    /// a place whose certificate no authority signs: a Hangar the app made.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pin: String,
    /// Where a Hangar the app made on a Proxmox is, to upgrade and remove.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxmox: Option<ProxmoxPlace>,
}

/// A Hangar the app made on a Proxmox: the Proxmox's API and certificate,
/// and the VM and what it made there (its Proxmox token is in the
/// keychain, beside the place's own).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ProxmoxPlace {
    pub url: String,
    pub fingerprint: String,
    pub node: String,
    pub vmid: u32,
    pub isos: String,
    pub pool: String,
    pub prefix: String,
    /// Where its environments' disks go, and the network they join.
    #[serde(default)]
    pub disks: String,
    #[serde(default)]
    pub network: String,
    /// Its VM's size.
    #[serde(default)]
    pub cores: u32,
    #[serde(default)]
    pub memory_mib: u32,
    #[serde(default)]
    pub data_gib: u32,
    /// The host name it is reached at, how its other hosts are named, and
    /// the certificate it serves there (its key is in the keychain); none,
    /// its address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web: Option<WebPlace>,
}

/// A Hangar's host name, host style and certificate, as the app gave them
/// (`hangar::Web` without its key).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct WebPlace {
    pub host: String,
    pub style: String,
    pub cert: String,
}

/// How a place's connection is.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Connection {
    #[default]
    Connecting,
    Online,
    /// It could not be reached, or the connection failed; trying again.
    Offline {
        error: String,
    },
    /// Its token is missing or not accepted.
    SignIn,
    /// It does not serve the API version the app speaks.
    Unsupported {
        server: String,
    },
}

/// What a place's connection has learnt.
#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    Connection(Connection),
    Me(Me),
    /// Every environment, replacing what was known.
    Snapshot(Vec<Environment>),
    /// A stream resumed: the environments changed since, and the IDs of
    /// every one there is, the rest kept as they were known.
    Resumed {
        changed: Vec<Environment>,
        ids: Vec<String>,
    },
    Environment(Box<Environment>),
    Removed(String),
}

/// What is known of a place: as its connection last said, kept on disk so
/// the app shows it before it reconnects.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PlaceState {
    #[serde(skip)]
    pub connection: Connection,
    pub me: Option<Me>,
    /// Its environments, oldest first.
    pub environments: Vec<Environment>,
    /// When it last had every environment from the server.
    pub synced_at: Option<DateTime<Utc>>,
}

impl PlaceState {
    pub fn apply(&mut self, u: Update) {
        match u {
            Update::Connection(c) => self.connection = c,
            Update::Me(me) => self.me = Some(me),
            Update::Snapshot(mut envs) => {
                envs.sort_by_key(|e| e.created_at);
                self.environments = envs;
                self.synced_at = Some(Utc::now());
                self.connection = Connection::Online;
            }
            Update::Resumed { changed, ids } => {
                let mut envs: Vec<Environment> = ids
                    .iter()
                    .filter_map(|id| {
                        changed
                            .iter()
                            .chain(&self.environments)
                            .find(|e| &e.id == id)
                            .cloned()
                    })
                    .collect();
                envs.sort_by_key(|e| e.created_at);
                self.environments = envs;
                self.synced_at = Some(Utc::now());
                self.connection = Connection::Online;
            }
            Update::Environment(e) => {
                match self.environments.iter_mut().find(|x| x.id == e.id) {
                    Some(x) => *x = *e,
                    None => {
                        self.environments.push(*e);
                        self.environments.sort_by_key(|e| e.created_at);
                    }
                }
                self.synced_at = Some(Utc::now());
            }
            Update::Removed(id) => self.environments.retain(|e| e.id != id),
        }
    }

    pub fn environment(&self, id: &str) -> Option<&Environment> {
        self.environments.iter().find(|e| e.id == id)
    }

    /// Loads what was last known of a place from dir, if anything.
    pub fn load(dir: &Path, id: &str) -> Option<PlaceState> {
        let b = std::fs::read(cache_file(dir, id)).ok()?;
        serde_json::from_slice(&b).ok()
    }

    /// Forgets what was known of a place.
    pub fn forget(dir: &Path, id: &str) {
        let _ = std::fs::remove_file(cache_file(dir, id));
    }

    /// Keeps what is known of a place in dir.
    pub fn save(&self, dir: &Path, id: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let path = cache_file(dir, id);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self)?)?;
        std::fs::rename(tmp, path)
    }
}

fn cache_file(dir: &Path, id: &str) -> PathBuf {
    let safe: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    dir.join(format!("{safe}.json"))
}

/// Keeps a place's connection on a thread of its own until it is dropped.
pub struct Connector {
    stop: Arc<AtomicBool>,
}

impl Drop for Connector {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// How long the connection waits after a failure before trying again,
/// doubling up to RETRY_MAX.
const RETRY_FIRST: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(30);

/// Connects to a server place and keeps connected, telling send what it
/// learns, until the Connector is dropped or send returns false. A token
/// the server refuses, or a server that does not speak the app's API
/// version, ends it.
pub fn connect(client: Client, send: impl Fn(Update) -> bool + Send + 'static) -> Connector {
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    thread::Builder::new()
        .name("place".into())
        .spawn(move || {
            let running = || !stopped.load(Ordering::Relaxed);
            let send = |u: Update| running() && send(u);
            let mut wait = RETRY_FIRST;
            // The last event's ID, kept while what it stands for is: what
            // came before it has all been applied.
            let mut last: Option<String> = None;
            while running() {
                if !send(Update::Connection(Connection::Connecting)) {
                    return;
                }
                let (synced, err) = session(&client, &send, &mut last);
                let next = match err {
                    api::Error::Unauthorized => Connection::SignIn,
                    api::Error::Status(426, server) => Connection::Unsupported { server },
                    e => Connection::Offline {
                        error: e.to_string(),
                    },
                };
                let ends = !matches!(next, Connection::Offline { .. });
                if !send(Update::Connection(next)) || ends {
                    return;
                }
                if synced {
                    wait = RETRY_FIRST;
                }
                let until = std::time::Instant::now() + wait;
                while running() && std::time::Instant::now() < until {
                    thread::sleep(Duration::from_millis(100));
                }
                wait = (wait * 2).min(RETRY_MAX);
            }
        })
        .expect("starting a place's connection");
    Connector { stop }
}

/// One connection: the version, who is signed in, then the event stream
/// until it ends, resumed from last, which it keeps up to date. It returns
/// whether the stream got as far as Synced, and why it ended.
fn session(
    client: &Client,
    send: &impl Fn(Update) -> bool,
    last: &mut Option<String>,
) -> (bool, api::Error) {
    let version = match client.version() {
        Ok(v) => v,
        Err(e) => return (false, e),
    };
    if !version.versions.iter().any(|v| v == api::VERSION) {
        // Told as the HTTP status for "upgrade required", which no
        // request returns.
        return (false, api::Error::Status(426, version.server));
    }
    match client.me() {
        Ok(me) => {
            if !send(Update::Me(me)) {
                return (false, api::Error::Transport("stopped".into()));
            }
        }
        Err(e) => return (false, e),
    }
    let mut events = match client.events(last.as_deref()) {
        Ok(e) => e,
        Err(e) => return (false, e),
    };
    let mut first: Option<Vec<Environment>> = Some(Vec::new());
    while let Some(event) = events.next() {
        let update = match (event, first.as_mut()) {
            (Err(e), _) => return (first.is_none(), e),
            (Ok(Event::Environment(e)), Some(list)) => {
                list.push(*e);
                continue;
            }
            (Ok(Event::Removed(id)), Some(list)) => {
                list.retain(|e| e.id != id);
                continue;
            }
            (Ok(Event::Synced(ids)), Some(_)) => {
                let changed = first.take().unwrap_or_default();
                // A server older than resuming names none, and has sent
                // every environment.
                if ids.is_empty() {
                    Update::Snapshot(changed)
                } else {
                    Update::Resumed { changed, ids }
                }
            }
            (Ok(Event::Environment(e)), None) => Update::Environment(e),
            (Ok(Event::Removed(id)), None) => Update::Removed(id),
            (Ok(Event::Synced(_)), None) => continue,
        };
        if !send(update) {
            return (true, api::Error::Transport("stopped".into()));
        }
        // Applied: a stream resumed from here misses nothing.
        if first.is_none() {
            *last = events.last_id().map(str::to_string);
        }
    }
    (
        first.is_none(),
        api::Error::Transport("the server closed the event stream".into()),
    )
}
