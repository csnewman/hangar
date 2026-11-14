//! Which worker runs each environment (internal/placement).
//!
//! Placement is permanent: an environment's disks are local to the worker
//! it lands on, so it stays there for life, and one whose worker goes
//! waits for it.

use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use chrono::Utc;
use uuid::Uuid;

use crate::audit::{self, Actor, Event};
use crate::store::{Result, Store};
use crate::workers::ONLINE_WINDOW;

/// Why an environment no suitable worker has room for waits.
pub const UNPLACEABLE: &str = "waiting for a worker with room for it";

/// Why an environment whose placement rules no online worker meets waits.
pub const NO_MATCH: &str = "waiting for a worker that matches its placement rules";

/// How much of the queue one transaction takes, so a long queue is shared
/// between servers rather than taken whole by one.
const BATCH: i64 = 100;

/// How often placement runs without being asked, for what nothing
/// signals, such as a worker coming back by reporting again.
const EVERY: Duration = Duration::from_secs(10);

/// The decider's own name in the audit log.
fn actor() -> Actor {
    Actor {
        user: Uuid::parse_str("00000000-0000-4000-8000-000000000002").ok(),
        name: "hangar:placement".into(),
        ..Actor::default()
    }
}

struct Free {
    id: Uuid,
    cpus: i64,
    memory_mib: i64,
    labels: BTreeMap<String, String>,
}

/// Places environments that want to run on online workers with room for
/// them, and says how many it placed. It spreads rather than packs: of the
/// workers an environment's rules allow, the one with the most free memory
/// that fits takes it, memory being what runs out first.
///
/// The queue and the candidate workers are held for the transaction, so
/// servers sharing a database place different environments and cannot
/// both spend a worker's last memory.
pub async fn place(store: &Store) -> Result<usize> {
    let mut tx = store.begin().await?;
    let queue = tx.placement_queue(BATCH).await?;
    if queue.is_empty() {
        return Ok(0);
    }
    let workers = tx.lock_online_workers(Utc::now() - ONLINE_WINDOW).await?;
    let occupying = tx.occupying().await?;
    let mut free: Vec<Free> = workers
        .into_iter()
        .map(|w| {
            let (c, m) = occupying
                .iter()
                .filter(|o| o.0 == w.id)
                .fold((0, 0), |(c, m), o| (c + o.2, m + o.3));
            Free {
                id: w.id,
                cpus: w.cpus as i64 - c,
                memory_mib: w.memory_mib as i64 - m,
                labels: serde_json::from_value(w.labels).unwrap_or_default(),
            }
        })
        .collect();
    let by = actor();
    let mut placed = 0;
    let mut bumped = HashSet::new();
    for mut e in queue {
        let rules: BTreeMap<String, String> = e
            .spec
            .get("placement")
            .and_then(|p| serde_json::from_value(p.clone()).ok())
            .unwrap_or_default();
        let allowed: Vec<usize> = (0..free.len())
            .filter(|&i| rules.iter().all(|(k, v)| free[i].labels.get(k) == Some(v)))
            .collect();
        let mut best: Option<usize> = None;
        for &i in &allowed {
            let w = &free[i];
            if w.cpus >= e.cpus as i64
                && w.memory_mib >= e.memory_mib as i64
                && best.is_none_or(|b| w.memory_mib > free[b].memory_mib)
            {
                best = Some(i);
            }
        }
        let Some(best) = best.map(|i| &mut free[i]) else {
            let matched = !allowed.is_empty();
            let reason = if matched { UNPLACEABLE } else { NO_MATCH };
            if e.reason != reason {
                e.reason = reason.to_string();
                e.updated_at = Utc::now();
                tx.save_environment(&e).await?;
                audit::record(
                    &mut tx,
                    &by,
                    Event::new("environment.unplaceable", "environment", e.id, &e.name)
                        .related("owner", e.owner_id)
                        .details(serde_json::json!({ "reason": reason })),
                )
                .await?;
            }
            continue;
        };
        best.cpus -= e.cpus as i64;
        best.memory_mib -= e.memory_mib as i64;
        e.worker_id = Some(best.id);
        e.phase = "pending".into();
        e.reason = String::new();
        e.updated_at = Utc::now();
        tx.save_environment(&e).await?;
        audit::record(
            &mut tx,
            &by,
            Event::new("environment.place", "environment", e.id, &e.name)
                .related("owner", e.owner_id)
                .related("worker", best.id)
                .details(serde_json::json!({ "cpus": e.cpus, "memory_mib": e.memory_mib })),
        )
        .await?;
        bumped.insert(best.id);
        placed += 1;
    }
    for w in bumped {
        tx.bump(w).await?;
    }
    tx.commit().await?;
    Ok(placed)
}

/// Places whenever something signals placement, and every EVERY besides,
/// until the process ends.
pub async fn run(store: Store) {
    loop {
        tokio::select! {
            _ = store.signals.placement.notified() => {}
            _ = tokio::time::sleep(EVERY) => {}
        }
        match place(&store).await {
            Ok(0) => {}
            Ok(n) => eprintln!("hangar-server: placed {n} environments"),
            Err(e) => eprintln!("hangar-server: placement failed: {e}"),
        }
    }
}
