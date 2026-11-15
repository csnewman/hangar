//! The profile session the server holds with each running environment:
//! its owner's keys and shared files go to the guest, and what the guest
//! says of its coding agents, its task and its routes comes back, as does
//! a signature with its owner's key.

use base64::Engine;
use hangar_suite::worker::{AGENT_TASK, SIGNED};
use hangar_suite::{Server, body, eventually, frontendapi, machine};
use serde_json::json;

async fn running(admin: &frontendapi::Client) -> frontendapi::types::Environment {
    let t = admin
        .create_template(&body(machine("Session", "private")))
        .await
        .unwrap()
        .into_inner();
    let e = admin
        .create_environment(&body(json!({ "template_id": t.id, "name": "session" })))
        .await
        .unwrap()
        .into_inner();
    let id = e.id.clone();
    eventually("the environment running", || async {
        let e = admin.get_environment(&id).await.ok()?.into_inner();
        (serde_json::to_value(e.phase).ok()? == json!("running")).then_some(())
    })
    .await;
    e
}

#[tokio::test]
async fn the_guest_hears_its_owners_keys_and_files_and_is_heard() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let w = s.worker("w");
    let c = s.client(&s.token(&admin, "sessions").await);
    let key = c
        .generate_ssh_key(&body(json!({ "name": "session" })))
        .await
        .expect("making a key")
        .into_inner();
    let e = running(&admin).await;

    // The key reaches the guest's SSH agent, and signs for it.
    let signed = eventually("a signature for the guest", || async {
        w.profile(&e.id)
            .into_iter()
            .find(|m| m["type"] == "signed" && m["id"] == 2)
    })
    .await;
    assert!(signed.get("error").is_none(), "signing failed: {signed}");
    let sig = base64::engine::general_purpose::STANDARD
        .decode(signed["signature"].as_str().unwrap())
        .unwrap();
    let public: russh::keys::ssh_key::PublicKey = key.public_key.parse().unwrap();
    let sig = {
        use russh::keys::ssh_key::encoding::Decode;
        russh::keys::ssh_key::Signature::decode(&mut sig.as_slice())
            .expect("a signature in SSH wire form")
    };
    signature::Verifier::verify(&public, SIGNED, &sig).expect("the signature verifies");
    let keys = w
        .profile(&e.id)
        .into_iter()
        .rfind(|m| m["type"] == "keys")
        .unwrap();
    assert_eq!(keys["keys"], json!([key.public_key]));

    // The built-in profile is its shared files.
    let packs = admin
        .list_environment_packs(&e.id)
        .await
        .unwrap()
        .into_inner();
    let profile = packs.iter().find(|p| p.pack.builtin).expect("the profile");
    let paths = eventually("the profile's paths sent", || async {
        w.profile(&e.id).into_iter().rfind(|m| m["type"] == "paths")
    })
    .await;
    assert_eq!(paths["sets"][0]["id"], json!(profile.copy.id));
    assert!(
        paths["sets"][0]["paths"]
            .as_array()
            .unwrap()
            .contains(&json!("~/.claude/")),
        "the profile's paths: {paths}"
    );

    // What the guest says of its coding agent, and the task it set.
    let mine = eventually("the agent's session and task", || async {
        let e = c.get_environment(&e.id).await.ok()?.into_inner();
        (!e.agents.is_empty() && e.task.is_some()).then_some(e)
    })
    .await;
    let agent = &mine.agents[0];
    assert_eq!((agent.state.as_str(), agent.needs_you), ("asking", true));
    let task = mine.task.unwrap();
    assert_eq!(
        (task.text.as_str(), task.set_by.as_str()),
        (AGENT_TASK, "agent")
    );
    c.see_agent(&e.id, &agent.id)
        .await
        .expect("seeing the session");
    let seen = c.get_environment(&e.id).await.unwrap().into_inner();
    assert!(
        !seen.agents[0].needs_you,
        "a seen session still needs its owner"
    );

    // A task set by its owner reaches the guest.
    c.set_task(&e.id, &body(json!({ "text": "The owner's task" })))
        .await
        .unwrap();
    eventually("the owner's task sent", || async {
        w.profile(&e.id)
            .into_iter()
            .find(|m| m["type"] == "task" && m["task"]["text"] == "The owner's task")
    })
    .await;
}
