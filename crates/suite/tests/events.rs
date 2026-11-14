//! The client API's event stream: every environment, synced, then each
//! change as it happens.

use hangar_suite::{Events, Server, body, machine};
use serde_json::json;

#[tokio::test]
async fn the_event_stream_follows_environments() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let token = s.token(&admin, "events").await;
    let c = s.client(&token);
    let t = admin
        .create_template(&body(machine("Suite machine", "private")))
        .await
        .unwrap()
        .into_inner();
    let first = admin
        .create_environment(&body(json!({ "template_id": t.id, "name": "first" })))
        .await
        .unwrap()
        .into_inner();

    let mut ev = Events::open(&c).await;
    let sent = ev.next().await.expect("the environment there already");
    assert_eq!(sent.kind, "environment");
    assert_eq!(sent.data["id"], json!(first.id));
    assert_eq!(ev.next().await.expect("synced").kind, "synced");

    let second = admin
        .create_environment(&body(json!({ "template_id": t.id, "name": "second" })))
        .await
        .unwrap()
        .into_inner();
    let made = ev
        .until("environment", &second.id)
        .await
        .expect("the new environment");
    assert_eq!(made.data["name"], json!("second"));

    admin
        .delete_environment_or_gone(&second.id)
        .await
        .expect("deleting it");
    ev.until("removed", &second.id)
        .await
        .expect("word of its removal");
}

/// A stream sees only what its user may.
#[tokio::test]
async fn the_event_stream_keeps_to_its_user() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let bob = s.user(&admin, "bob").await;
    let token = s.token(&bob, "bobs").await;
    let mut ev = Events::open(&s.client(&token)).await;
    assert_eq!(ev.next().await.expect("synced").kind, "synced");
    let t = admin
        .create_template(&body(machine("Admin's", "private")))
        .await
        .unwrap()
        .into_inner();
    let theirs = admin
        .create_environment(&body(json!({ "template_id": t.id, "name": "admins" })))
        .await
        .unwrap()
        .into_inner();
    let mine_t = bob
        .create_template(&body(machine("Bob's", "private")))
        .await
        .unwrap()
        .into_inner();
    let mine = bob
        .create_environment(&body(json!({ "template_id": mine_t.id, "name": "bobs" })))
        .await
        .unwrap()
        .into_inner();
    loop {
        let e = ev.next().await.expect("bob's environment");
        assert_ne!(e.data["id"], json!(theirs.id), "bob was told of another's");
        if e.data["id"] == json!(mine.id) {
            break;
        }
    }
}
