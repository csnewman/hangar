//! An environment is its owner's: what others can and cannot do with it,
//! and what of it changes while it runs.

use hangar_suite::{Server, body, eventually, frontendapi, machine, status};
use serde_json::json;

fn phase(e: &frontendapi::types::Environment) -> String {
    serde_json::to_value(e.phase)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

async fn reaches(c: &frontendapi::Client, id: &str, want: &str) {
    eventually(&format!("{id} to be {want}"), || async {
        let e = c.get_environment(id).await.ok()?.into_inner();
        (phase(&e) == want).then_some(())
    })
    .await
}

#[tokio::test]
async fn another_user_cannot_reach_an_environment() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let alice = s.user(&admin, "alice").await;
    let bob = s.user(&admin, "bob").await;
    let t = alice
        .create_template(&body(machine("Alice's", "shared")))
        .await
        .unwrap()
        .into_inner();
    let e = alice
        .create_environment(&body(json!({ "template_id": t.id, "name": "alices" })))
        .await
        .unwrap()
        .into_inner();

    let listed = bob.list_environments().await.unwrap().into_inner();
    assert!(listed.iter().all(|x| x.id != e.id), "bob lists alice's");
    assert_eq!(status(bob.get_environment(&e.id).await), Some(404));
    assert_eq!(status(bob.stop_environment(&e.id).await), Some(404));
    assert_eq!(status(bob.delete_environment(&e.id).await), Some(404));
    admin
        .get_environment(&e.id)
        .await
        .expect("an administrator sees every environment");
}

#[tokio::test]
async fn a_machine_changes_while_stopped_and_its_ports_at_any_time() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let w = s.worker("w");
    let t = admin
        .create_template(&body(machine("Suite machine", "private")))
        .await
        .unwrap()
        .into_inner();
    let e = admin
        .create_environment(&body(json!({ "template_id": t.id, "name": "env" })))
        .await
        .unwrap()
        .into_inner();
    reaches(&admin, &e.id, "running").await;

    let bigger = json!({ "cpus": 2, "memory_mib": 2048, "display": "none", "gpu": "none" });
    assert_eq!(
        status(
            admin
                .update_environment_settings(&e.id, &body(bigger.clone()))
                .await
        ),
        Some(409),
        "its vCPUs changed while it runs"
    );

    let public = admin
        .set_environment_ports(&e.id, &body(json!({ "public": true })))
        .await
        .expect("making its ports public while it runs")
        .into_inner();
    assert_eq!(public.ports_public, Some(true));

    admin.stop_environment(&e.id).await.unwrap();
    reaches(&admin, &e.id, "stopped").await;
    let changed = admin
        .update_environment_settings(&e.id, &body(bigger))
        .await
        .expect("changing a stopped machine")
        .into_inner();
    assert_eq!((changed.cpus, changed.memory_mib), (2, 2048));
    admin.start_environment(&e.id).await.unwrap();
    reaches(&admin, &e.id, "running").await;
    let spec = w.spec(&e.id).expect("the worker's spec");
    assert_eq!((spec.spec.cpus, spec.spec.memory_mib), (2, 2048));
}
