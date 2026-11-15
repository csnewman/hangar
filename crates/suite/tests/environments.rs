//! An environment's life on a worker: made from a template, placed, and
//! taken through start, stop, suspend and delete, each as the server shows
//! it and as the worker is asked.

use hangar_api::DesiredState;
use hangar_suite::{Server, body, eventually, frontendapi, machine, why};
use serde_json::json;

/// What the server says an environment's phase is.
fn phase(e: &frontendapi::types::Environment) -> String {
    serde_json::to_value(e.phase)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// A template the suite's workers take.
async fn template(admin: &frontendapi::Client) -> frontendapi::types::Template {
    admin
        .create_template(&body(machine("Suite machine", "private")))
        .await
        .expect("making a template")
        .into_inner()
}

/// Waits for the environment to be in phase.
async fn reaches(admin: &frontendapi::Client, id: &str, want: &str) {
    eventually(&format!("{id} to be {want}"), || async {
        let e = admin.get_environment(id).await.ok()?.into_inner();
        (phase(&e) == want).then_some(())
    })
    .await
}

#[tokio::test]
async fn an_environment_lives_on_a_worker() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let w = s.worker("suite-worker");
    eventually("the worker online", || async {
        let workers = match admin.list_workers().await {
            Ok(w) => w.into_inner(),
            Err(e) => {
                eprintln!("listing workers: {}", why(&e));
                return None;
            }
        };
        workers
            .iter()
            .any(|w| w.name == "suite-worker" && w.online)
            .then_some(())
    })
    .await;

    let t = template(&admin).await;
    let e = admin
        .create_environment(&body(json!({ "template_id": t.id, "name": "suite-env" })))
        .await
        .expect("making an environment")
        .into_inner();
    reaches(&admin, &e.id, "running").await;
    assert_eq!(w.desired(&e.id), Some(DesiredState::Running));
    let placed = admin.get_environment(&e.id).await.unwrap().into_inner();
    assert_eq!(placed.worker.as_deref(), Some("suite-worker"));
    // Its guest's agent, as the worker reports it, while it runs.
    let agent = eventually("its guest agent", || async {
        admin
            .get_environment(&e.id)
            .await
            .ok()?
            .into_inner()
            .guest_agent
    })
    .await;
    assert_eq!((agent.protocol, agent.outdated), (1, Some(true)));

    admin.stop_environment(&e.id).await.expect("stopping it");
    reaches(&admin, &e.id, "stopped").await;
    let stopped = admin.get_environment(&e.id).await.unwrap().into_inner();
    assert!(stopped.guest_agent.is_none(), "a stopped one has no agent");
    assert_eq!(w.desired(&e.id), Some(DesiredState::Stopped));

    admin.start_environment(&e.id).await.expect("starting it");
    reaches(&admin, &e.id, "running").await;

    admin
        .suspend_environment(&e.id)
        .await
        .expect("suspending it");
    reaches(&admin, &e.id, "suspended").await;
    assert_eq!(w.desired(&e.id), Some(DesiredState::Suspended));
    admin.start_environment(&e.id).await.expect("resuming it");
    reaches(&admin, &e.id, "running").await;

    admin
        .delete_environment_or_gone(&e.id)
        .await
        .expect("deleting it");
    eventually("the environment gone", || async {
        let gone = admin.get_environment(&e.id).await.err()?;
        (gone.status().map(|s| s.as_u16()) == Some(404)).then_some(())
    })
    .await;
    assert_eq!(w.desired(&e.id), None, "the worker still holds it");
}
