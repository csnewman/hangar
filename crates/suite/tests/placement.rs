//! Where an environment runs: on a worker whose labels its template's
//! placement names and which has room for it, waiting until there is one.

use hangar_suite::{Server, body, eventually, frontendapi, machine, status};
use serde_json::json;

fn text<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

async fn online(admin: &frontendapi::Client, name: &str) {
    eventually(&format!("{name} online"), || async {
        let ws = admin.list_workers().await.ok()?.into_inner();
        ws.iter().any(|w| w.name == name && w.online).then_some(())
    })
    .await
}

/// Waits for the environment's phase and reason to be these.
async fn waits(c: &frontendapi::Client, id: &str, phase: &str, reason: &str) {
    eventually(&format!("{id} {phase}: {reason}"), || async {
        let e = c.get_environment(id).await.ok()?.into_inner();
        (text(&e.phase) == phase && e.reason.as_deref().unwrap_or("") == reason).then_some(())
    })
    .await
}

#[tokio::test]
async fn an_environment_waits_for_room_then_runs() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let _w = s.worker("w");
    online(&admin, "w").await;
    let mut big = machine("Big", "private");
    big["spec"]["memory_mib"] = json!(256 * 1024);
    let t = admin
        .create_template(&body(big))
        .await
        .unwrap()
        .into_inner();
    let e = admin
        .create_environment(&body(json!({ "template_id": t.id, "name": "big" })))
        .await
        .unwrap()
        .into_inner();
    waits(
        &admin,
        &e.id,
        "pending",
        "waiting for a worker with room for it",
    )
    .await;
    assert!(
        admin
            .get_environment(&e.id)
            .await
            .unwrap()
            .into_inner()
            .worker
            .is_none(),
        "placed on a worker without room"
    );

    // Not yet placed, it has no machine, so its size can change.
    admin
        .update_environment_settings(
            &e.id,
            &body(json!({ "cpus": 1, "memory_mib": 1024, "display": "none", "gpu": "none" })),
        )
        .await
        .expect("shrinking an environment waiting for room");
    waits(&admin, &e.id, "running", "").await;
}

#[tokio::test]
async fn placement_follows_labels() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let _w = s.worker("w");
    online(&admin, "w").await;
    let mut elsewhere = machine("Elsewhere", "private");
    elsewhere["spec"]["placement"] = json!({ "runtime": "elsewhere" });
    let t = admin
        .create_template(&body(elsewhere))
        .await
        .unwrap()
        .into_inner();
    let e = admin
        .create_environment(&body(json!({ "template_id": t.id, "name": "elsewhere" })))
        .await
        .unwrap()
        .into_inner();
    waits(
        &admin,
        &e.id,
        "pending",
        "waiting for a worker that matches its placement rules",
    )
    .await;
}

/// With no worker to wait for, stopping and deleting take effect at once.
#[tokio::test]
async fn an_unplaced_environment_changes_at_once() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let t = admin
        .create_template(&body(machine("Nowhere", "private")))
        .await
        .unwrap()
        .into_inner();
    let e = admin
        .create_environment(&body(json!({ "template_id": t.id, "name": "nowhere" })))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(text(&e.phase), "pending");
    let stopped = admin.stop_environment(&e.id).await.unwrap().into_inner();
    assert_eq!(
        (text(&stopped.desired), text(&stopped.phase)),
        ("stopped".into(), "stopped".into())
    );
    assert_eq!(
        status(admin.suspend_environment(&e.id).await),
        Some(409),
        "a stopped environment suspended"
    );
    let started = admin.start_environment(&e.id).await.unwrap().into_inner();
    assert_eq!(text(&started.phase), "pending");
    let gone = admin
        .delete_environment_or_gone(&e.id)
        .await
        .expect("deleting it");
    assert!(
        gone.is_none(),
        "an unplaced environment waited to be deleted"
    );
    assert_eq!(status(admin.get_environment(&e.id).await), Some(404));
}
