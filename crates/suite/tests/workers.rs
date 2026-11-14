//! Workers: registering with the bootstrap token, what they offer, and
//! being revoked and removed.

use hangar_suite::{Server, eventually, frontendapi, status, why};

/// The worker named name, once the server lists it online.
async fn online(admin: &frontendapi::Client, name: &str) -> frontendapi::types::Worker {
    eventually(&format!("{name} online"), || async {
        match admin.list_workers().await {
            Ok(ws) => ws
                .into_inner()
                .into_iter()
                .find(|w| w.name == name && w.online),
            Err(e) => {
                eprintln!("listing workers: {}", why(&e));
                None
            }
        }
    })
    .await
}

#[tokio::test]
async fn a_worker_registers_and_offers_its_machine() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let _w = s.worker("w1");
    let w = online(&admin, "w1").await;
    assert_eq!(w.labels.get("runtime").map(String::as_str), Some("fake"));
    assert!(w.capacity.cpus > 0 && w.capacity.memory_mib > 0, "{w:?}");
    assert_eq!(
        serde_json::to_value(w.monitor).unwrap(),
        serde_json::json!("krun")
    );
    assert!(!w.revoked);
    let one = admin
        .get_worker(&w.id)
        .await
        .expect("the worker")
        .into_inner();
    assert_eq!(one.name, "w1");
}

#[tokio::test]
async fn a_wrong_bootstrap_token_is_rejected() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let mut w = s.worker_with_token("w-bad", "not-the-bootstrap-token");
    let out = w.ended().await.expect("the worker stops");
    assert!(
        out.unwrap_err().contains("rejected"),
        "it stopped for another reason"
    );
    let listed = admin.list_workers().await.unwrap().into_inner();
    assert!(listed.iter().all(|w| w.name != "w-bad"), "{listed:?}");
}

#[tokio::test]
async fn a_revoked_worker_is_cut_off_and_then_removed() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let mut w = s.worker("w2");
    let id = online(&admin, "w2").await.id;
    assert_eq!(
        status(admin.delete_worker(&id).await),
        Some(409),
        "removed before it was revoked"
    );
    admin.revoke_worker(&id).await.expect("revoking it");
    let out = w.ended().await.expect("the worker stops");
    assert!(out.unwrap_err().contains("rejected"));
    let revoked = admin.get_worker(&id).await.unwrap().into_inner();
    assert!(revoked.revoked);
    admin.delete_worker(&id).await.expect("removing it");
    assert_eq!(status(admin.get_worker(&id).await), Some(404));
}

#[tokio::test]
async fn only_an_administrator_sees_workers() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let bob = s.user(&admin, "bob").await;
    assert_eq!(status(bob.list_workers().await), Some(403));
}
