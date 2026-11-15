//! The machines that run environments: their identity and credentials, what
//! they report, and what each is asked to run.

use std::time::Duration;

use base64::Engine;
use chrono::Utc;
use hangar_api::{DesiredSet, EnvironmentSpec, ImageCopy, Phase, WorkerCredential, WorkerStatus};
use rand::RngCore;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::audit::{self, Actor, Event};
use crate::fail::{self, Fail};
use crate::store::rows::Worker;
use crate::store::workers::Listed;
use crate::store::{Result, Store};
use crate::users::Principal;

/// A worker that has asked for its desired set or reported within this is
/// online.
pub const ONLINE_WINDOW: chrono::Duration = chrono::Duration::seconds(45);

/// How long a worker's request for its desired set is held, waiting for a
/// change, before the unchanged set is answered.
pub const DESIRED_HOLD: Duration = Duration::from_secs(25);

/// A worker's name: letters, digits and . _ -, starting and ending with a
/// letter or digit, at most 128 characters.
fn valid_name(name: &str) -> bool {
    let b = name.as_bytes();
    let edge = |c: u8| c.is_ascii_alphanumeric();
    !b.is_empty()
        && b.len() <= 128
        && edge(b[0])
        && edge(b[b.len() - 1])
        && b.iter()
            .all(|&c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

fn worker_actor(id: Uuid, name: &str) -> Actor {
    Actor {
        worker: Some(id),
        name: name.to_string(),
        ..Actor::default()
    }
}

/// Registers a worker, giving its credential: its ID and a secret, of
/// which only the SHA-256 is kept.
pub async fn register(
    store: &Store,
    name: &str,
    labels: &std::collections::BTreeMap<String, String>,
) -> fail::Result<WorkerCredential> {
    if !valid_name(name) {
        return fail::invalid(format!("worker name {name:?}"));
    }
    let mut secret = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut secret);
    let now = Utc::now();
    let w = Worker {
        id: Uuid::new_v4(),
        name: name.to_string(),
        credential_hash: Sha256::digest(secret).to_vec(),
        labels: serde_json::to_value(labels).unwrap_or_else(|_| serde_json::json!({})),
        cpus: 0,
        memory_mib: 0,
        unknown: serde_json::json!([]),
        stats: None,
        images: serde_json::json!([]),
        gpu: None,
        desired_version: 1,
        last_seen_at: None,
        revoked_at: None,
        created_at: now,
        monitor: String::new(),
        version: String::new(),
        features: Vec::new(),
    };
    let mut tx = store.begin().await?;
    if !tx.insert_worker(&w).await? {
        return fail::conflict(format!("a worker named {name} is already registered"));
    }
    audit::record(
        &mut tx,
        &audit::system(),
        Event::new("worker.register", "worker", w.id, name)
            .details(serde_json::json!({ "labels": labels })),
    )
    .await?;
    tx.commit().await?;
    Ok(WorkerCredential {
        id: w.id.to_string(),
        credential: format!(
            "{}.{}",
            w.id,
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret)
        ),
    })
}

/// The worker a credential is, unless it is wrong or revoked.
pub async fn authenticate(store: &Store, credential: &str) -> Result<Option<Uuid>> {
    let Some((id, secret)) = credential.split_once('.') else {
        return Ok(None);
    };
    let (Ok(id), Ok(secret)) = (
        Uuid::parse_str(id),
        base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(secret),
    ) else {
        return Ok(None);
    };
    let Some(hash) = store.begin().await?.worker_credential(id).await? else {
        return Ok(None);
    };
    let sum = Sha256::digest(&secret);
    // Compared whole, however early they differ.
    let same = hash.len() == sum.len()
        && hash
            .iter()
            .zip(sum.iter())
            .fold(0u8, |d, (a, b)| d | (a ^ b))
            == 0;
    Ok(same.then_some(id))
}

/// Marks a worker seen.
pub async fn touch(store: &Store, id: Uuid) -> Result<()> {
    let mut tx = store.begin().await?;
    tx.touch_worker(id, Utc::now()).await?;
    tx.commit().await
}

/// What a worker is asked to run, once its version passes after or
/// DESIRED_HOLD goes by; None if there is no such worker.
pub async fn desired(
    store: &Store,
    id: Uuid,
    after: i64,
    registry: &str,
) -> Result<Option<DesiredSet>> {
    let notify = store.signals.watch_worker(id);
    let deadline = tokio::time::Instant::now() + DESIRED_HOLD;
    loop {
        let changed = notify.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        let Some(v) = store.begin().await?.desired_version(id).await? else {
            return Ok(None);
        };
        if v > after {
            break;
        }
        tokio::select! {
            _ = changed => continue,
            _ = tokio::time::sleep_until(deadline) => break,
        }
    }
    let Some((version, envs, images, forget)) = store.begin().await?.desired(id).await? else {
        return Ok(None);
    };
    let mut environments = Vec::with_capacity(envs.len());
    for e in envs {
        let spec = serde_json::from_value(e.spec.clone())
            .map_err(|err| crate::store::Error(format!("environment {}: {err}", e.id)))?;
        let desired = serde_json::from_value(serde_json::json!(e.desired))
            .map_err(|err| crate::store::Error(format!("environment {}: {err}", e.id)))?;
        environments.push(EnvironmentSpec {
            id: e.id.to_string(),
            name: e.name,
            desired,
            spec,
            pin_digest: e.image_pin_want,
            drop_rollback: e.image_drop_rollback,
        });
    }
    Ok(Some(DesiredSet {
        version,
        environments,
        remove_images: images
            .into_iter()
            .map(|r| ImageCopy {
                r#ref: r.r#ref,
                digest: r.digest,
            })
            .collect(),
        remove_environments: forget,
        registry: registry.to_string(),
    }))
}

/// Takes a worker's status report.
pub async fn report(store: &Store, id: Uuid, st: &WorkerStatus) -> fail::Result<()> {
    if st.capacity.cpus < 0 || st.capacity.memory_mib < 0 {
        return fail::invalid("capacity cannot be negative");
    }
    if let Some(o) = st.environments.iter().find(|o| o.phase == Phase::Pending) {
        return fail::invalid(format!("environment {}: unknown phase \"pending\"", o.id));
    }
    let mut tx = store.begin().await?;
    let Some(taken) = tx.report(id, st, Utc::now()).await? else {
        return Err(Fail::NotFound("no such worker".into()));
    };
    let by = worker_actor(id, &taken.worker_name);
    for p in &taken.phases {
        audit::record(
            &mut tx,
            &by,
            Event::new("environment.phase", "environment", p.id, &p.name)
                .related("owner", p.owner)
                .details(serde_json::json!({ "from": p.from, "to": p.to, "reason": p.reason })),
        )
        .await?;
    }
    for (env, name, owner) in &taken.deleted {
        audit::record(
            &mut tx,
            &by,
            Event::new("environment.deleted", "environment", env, name).related("owner", owner),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Every worker, by name.
pub async fn list(store: &Store) -> Result<Vec<Listed>> {
    store.begin().await?.workers(None).await
}

/// The worker with id.
pub async fn get(store: &Store, id: Uuid) -> Result<Option<Listed>> {
    Ok(store
        .begin()
        .await?
        .workers(Some(id))
        .await?
        .into_iter()
        .next())
}

/// Whether a worker is online: seen within ONLINE_WINDOW, and not revoked.
pub fn online(w: &Worker) -> bool {
    w.revoked_at.is_none()
        && w.last_seen_at
            .is_some_and(|at| Utc::now() - at < ONLINE_WINDOW)
}

/// Revokes a worker: its credential stops working, and it powers its
/// environments off as it learns so.
pub async fn revoke(store: &Store, by: &Principal, ip: &str, id: Uuid) -> fail::Result<()> {
    let mut tx = store.begin().await?;
    let Some(name) = tx.revoke_worker(id, Utc::now()).await? else {
        return Err(Fail::NotFound("worker not found".into()));
    };
    audit::record(
        &mut tx,
        &by.actor(ip),
        Event::new("worker.revoke", "worker", id, &name),
    )
    .await?;
    tx.bump(id).await?;
    tx.commit().await?;
    Ok(())
}

/// Removes a revoked worker that holds no environments.
pub async fn delete(store: &Store, by: &Principal, ip: &str, id: Uuid) -> fail::Result<()> {
    let mut tx = store.begin().await?;
    match tx.worker_revoked(id).await? {
        None => return Err(Fail::NotFound("worker not found".into())),
        Some(false) => return fail::conflict("revoke the worker before removing it"),
        Some(true) => {}
    }
    let held = tx.held_environments(id).await?;
    if held > 0 {
        return fail::conflict(format!("the worker still holds {held} environments"));
    }
    let name = tx.delete_worker(id).await?.unwrap_or_default();
    audit::record(
        &mut tx,
        &by.actor(ip),
        Event::new("worker.delete", "worker", id, &name),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Asks a worker to delete a copy of an image from its store, refused
/// while an environment on it uses the copy: one the worker says uses it,
/// or one pinned to it (or to the reference's current copy, when pinned to
/// none yet).
pub async fn remove_image(
    store: &Store,
    by: &Principal,
    ip: &str,
    id: Uuid,
    c: &ImageCopy,
) -> fail::Result<()> {
    let mut tx = store.begin().await?;
    // Environments before workers, as everywhere.
    let envs = tx.placed_for_update(id).await?;
    let Some(w) = tx.worker_for_update(id).await? else {
        return Err(Fail::NotFound("worker not found".into()));
    };
    let images: Vec<hangar_api::LocalImage> =
        serde_json::from_value(w.images.clone()).unwrap_or_default();
    let Some(held) = images
        .iter()
        .find(|i| i.r#ref == c.r#ref && i.digest == c.digest)
    else {
        return Err(Fail::NotFound(format!(
            "the worker does not hold {} at {}",
            c.r#ref, c.digest
        )));
    };
    let users: Vec<&str> = envs
        .iter()
        .filter(|e| {
            held.environments.contains(&e.id.to_string())
                || (e.image == c.r#ref
                    && (e.image_digest == c.digest || (e.image_digest.is_empty() && held.current)))
        })
        .map(|e| e.name.as_str())
        .collect();
    if !users.is_empty() {
        return fail::conflict(format!("used by {}", users.join(", ")));
    }
    tx.add_image_removal(&crate::store::rows::ImageRemoval {
        worker_id: id,
        r#ref: c.r#ref.clone(),
        digest: c.digest.clone(),
    })
    .await?;
    audit::record(
        &mut tx,
        &by.actor(ip),
        Event::new("image.remove", "image", &c.r#ref, &c.r#ref)
            .related("worker", id)
            .details(serde_json::json!({ "digest": c.digest })),
    )
    .await?;
    tx.bump(id).await?;
    tx.commit().await?;
    Ok(())
}

/// Asks a worker to delete environments it runs that the server has no
/// record of: each must be one the worker reports as such, since one the
/// server knows is deleted through the environment.
pub async fn remove_unknown(
    store: &Store,
    by: &Principal,
    ip: &str,
    id: Uuid,
    ids: &[String],
) -> fail::Result<()> {
    let mut tx = store.begin().await?;
    let Some(w) = tx.worker_for_update(id).await? else {
        return Err(Fail::NotFound("worker not found".into()));
    };
    let unknown: Vec<String> = serde_json::from_value(w.unknown.clone()).unwrap_or_default();
    for env in ids {
        if !unknown.contains(env) {
            return Err(Fail::Invalid(format!(
                "the worker does not report {env} as unknown to the server"
            )));
        }
        if let Ok(known) = env.parse::<Uuid>()
            && tx.environment_known(known).await?
        {
            return Err(Fail::Invalid(format!(
                "{env} is an environment the server knows"
            )));
        }
        tx.add_environment_removal(&crate::store::rows::EnvironmentRemoval {
            worker_id: id,
            environment: env.clone(),
        })
        .await?;
        audit::record(
            &mut tx,
            &by.actor(ip),
            Event::new("environment.remove_unknown", "environment", env, "").related("worker", id),
        )
        .await?;
    }
    tx.bump(id).await?;
    tx.commit().await?;
    Ok(())
}
