//! What a commit tells whoever waits on it: the workers whose desired sets
//! changed, placement that an environment may be waiting, and the event
//! streams which environments changed. With Postgres the same goes to every
//! server sharing the database, on the Go server's channels.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;
use uuid::Uuid;

/// One thing a transaction changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// A worker's desired set.
    Worker(Uuid),
    /// Something placement decides on.
    Placement,
    /// Something a client shows of an environment.
    Environment(Uuid),
}

/// The channels a signal goes out on to other servers, and comes in on.
pub const WORKER_CHANNEL: &str = "hangar_worker";
pub const PLACEMENT_CHANNEL: &str = "hangar_placement";
pub const CAPACITY_CHANNEL: &str = "hangar_capacity";
pub const ENVIRONMENT_CHANNEL: &str = "environment_changed";

/// Everyone waiting on a signal in this process.
#[derive(Default)]
pub struct Signals {
    workers: Mutex<HashMap<Uuid, Arc<Notify>>>,
    /// Wakes placement.
    pub placement: Notify,
    pub changes: Changes,
}

impl Signals {
    /// What wakes for the worker with id.
    pub fn watch_worker(&self, id: Uuid) -> Arc<Notify> {
        self.workers.lock().unwrap().entry(id).or_default().clone()
    }

    pub fn deliver(&self, s: Signal) {
        match s {
            Signal::Worker(id) => {
                if let Some(n) = self.workers.lock().unwrap().get(&id) {
                    n.notify_waiters();
                }
            }
            Signal::Placement => self.placement.notify_one(),
            Signal::Environment(id) => self.changes.publish(Some(id)),
        }
    }

    /// Wakes everyone, as after signals may have been lost.
    pub fn everything(&self) {
        for n in self.workers.lock().unwrap().values() {
            n.notify_waiters();
        }
        self.placement.notify_one();
        self.changes.publish(None);
    }

    /// The signal a notification from another server is.
    pub fn from_channel(channel: &str, payload: &str) -> Option<Signal> {
        match channel {
            WORKER_CHANNEL => Uuid::parse_str(payload).ok().map(Signal::Worker),
            PLACEMENT_CHANNEL | CAPACITY_CHANNEL => Some(Signal::Placement),
            ENVIRONMENT_CHANNEL => Uuid::parse_str(payload).ok().map(Signal::Environment),
            _ => None,
        }
    }
}

/// Environments that changed, collected for each watcher until it takes
/// them.
#[derive(Default)]
pub struct Changes {
    next: AtomicU64,
    watchers: Mutex<HashMap<u64, Arc<Pending>>>,
}

#[derive(Default)]
struct Pending {
    taken: Mutex<(HashSet<Uuid>, bool)>,
    ready: Notify,
}

/// One watcher's changes, as yet untaken; it stops collecting when dropped.
pub struct Watch {
    signals: Arc<Signals>,
    id: u64,
    pending: Arc<Pending>,
}

impl Changes {
    /// Passes on that an environment changed, or with None that any may
    /// have.
    fn publish(&self, id: Option<Uuid>) {
        for p in self.watchers.lock().unwrap().values() {
            {
                let mut t = p.taken.lock().unwrap();
                match id {
                    Some(id) => {
                        t.0.insert(id);
                    }
                    None => t.1 = true,
                }
            }
            p.ready.notify_one();
        }
    }
}

impl Signals {
    /// Collects the environments changed after this call.
    pub fn watch_changes(self: &Arc<Self>) -> Watch {
        let id = self.changes.next.fetch_add(1, Ordering::Relaxed);
        let pending = Arc::new(Pending::default());
        self.changes
            .watchers
            .lock()
            .unwrap()
            .insert(id, pending.clone());
        Watch {
            signals: self.clone(),
            id,
            pending,
        }
    }
}

impl Watch {
    /// Waits until there are changes to take.
    pub async fn ready(&self) {
        self.pending.ready.notified().await
    }

    /// The environments changed since last taken, and whether any may have.
    pub fn take(&self) -> (Vec<Uuid>, bool) {
        let mut t = self.pending.taken.lock().unwrap();
        let ids = t.0.drain().collect();
        (ids, std::mem::take(&mut t.1))
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.signals
            .changes
            .watchers
            .lock()
            .unwrap()
            .remove(&self.id);
    }
}

/// Delivers what other servers sharing the database signal, until the
/// process ends. A notification lost while the connection was down could
/// be anything, so then everyone is woken.
pub async fn listen(pool: sqlx::PgPool, signals: Arc<Signals>) {
    loop {
        let mut l = match sqlx::postgres::PgListener::connect_with(&pool).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("hangar-server: listening for other servers' changes: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        };
        if let Err(e) = l
            .listen_all([
                WORKER_CHANNEL,
                PLACEMENT_CHANNEL,
                CAPACITY_CHANNEL,
                ENVIRONMENT_CHANNEL,
            ])
            .await
        {
            eprintln!("hangar-server: listening for other servers' changes: {e}");
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            continue;
        }
        signals.everything();
        loop {
            match l.try_recv().await {
                Ok(Some(n)) => {
                    if let Some(s) = Signals::from_channel(n.channel(), n.payload()) {
                        signals.deliver(s);
                    }
                }
                Ok(None) => signals.everything(),
                Err(e) => {
                    eprintln!("hangar-server: listening for other servers' changes: {e}");
                    break;
                }
            }
        }
    }
}
