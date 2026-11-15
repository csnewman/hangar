//! The client API's event stream: every environment, synced, then each
//! change as it happens.

use hangar_suite::{Events, Server, body, eventually, machine};
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

/// A client that loses the stream comes back with the last event's ID and
/// is sent only what changed while it was away, then every ID there is, so
/// it drops what has gone.
#[tokio::test]
async fn the_event_stream_resumes() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let token = s.token(&admin, "events").await;
    let c = s.client(&token);
    let t = admin
        .create_template(&body(machine("Suite machine", "private")))
        .await
        .unwrap()
        .into_inner();
    let make = |name: &'static str| {
        let admin = &admin;
        let t = &t;
        async move {
            let e = admin
                .create_environment(&body(json!({ "template_id": t.id, "name": name })))
                .await
                .unwrap()
                .into_inner();
            // Placement says why it waits, which is a change too: waited
            // for, so nothing changes it after.
            eventually(&format!("{name} waiting"), || async {
                let e = admin.get_environment(&e.id).await.ok()?.into_inner();
                e.reason.filter(|r| !r.is_empty()).map(|_| ())
            })
            .await;
            e
        }
    };
    let kept = make("kept").await;
    let gone = make("gone").await;

    let mut ev = Events::open(&c).await;
    let synced = loop {
        let e = ev.next().await.expect("the start of the stream");
        if e.kind == "synced" {
            break e;
        }
    };
    assert!(!synced.id.is_empty(), "the synced event has no ID");
    drop(ev);

    let added = make("added").await;
    admin
        .delete_environment_or_gone(&gone.id)
        .await
        .expect("deleting one");

    let mut back = Events::resume(&s, &token, &synced.id).await;
    let mut resent = Vec::new();
    let synced = loop {
        let e = back.next().await.expect("the resumed stream's start");
        match e.kind.as_str() {
            "environment" => resent.push(e.data["id"].as_str().unwrap().to_string()),
            "synced" => break e,
            other => panic!("a {other} event at the start"),
        }
    };
    assert_eq!(resent, std::slice::from_ref(&added.id), "what was resent");
    let mut ids: Vec<String> = synced.data["ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    ids.sort();
    let mut want = vec![kept.id.clone(), added.id.clone()];
    want.sort();
    assert_eq!(ids, want, "the IDs there are");

    // Back again at once, nothing has changed.
    let mut again = Events::resume(&s, &token, &synced.id).await;
    assert_eq!(again.next().await.expect("synced").kind, "synced");

    // An ID the server cannot read starts afresh.
    let mut fresh = Events::resume(&s, &token, "not an id").await;
    let first = fresh.next().await.expect("an environment");
    assert_eq!(first.kind, "environment");
}
