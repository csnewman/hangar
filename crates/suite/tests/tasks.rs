//! An environment's task: what it is for, set by its owner or its coding
//! agent, kept as history when done, and sent on the event stream.

use hangar_suite::{Events, Server, body, machine};
use serde_json::json;

#[tokio::test]
async fn a_task_is_set_replaced_and_ended() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let bob = s.user(&admin, "bob").await;
    let token = s.token(&admin, "tasks").await;
    let c = s.client(&token);
    let bob_c = s.client(&s.token(&bob, "bobs").await);
    let t = admin
        .create_template(&body(machine("Tasks", "private")))
        .await
        .unwrap()
        .into_inner();
    let e = admin
        .create_environment(&body(json!({ "template_id": t.id, "name": "tasks" })))
        .await
        .unwrap()
        .into_inner();
    let mut ev = Events::open(&c).await;
    ev.until("environment", &e.id)
        .await
        .expect("the environment");

    let first = c
        .set_task(&e.id, &body(json!({ "text": "  Fix the\n  login   bug " })))
        .await
        .expect("setting a task")
        .into_inner();
    assert_eq!(first.text, "Fix the login bug");
    assert_eq!(first.set_by, "user");
    assert!(first.done_at.is_none());
    let sent = ev.until("environment", &e.id).await.expect("the task sent");
    assert_eq!(sent.data["task"]["text"], json!("Fix the login bug"));

    let again = c
        .set_task(&e.id, &body(json!({ "text": "Fix the login bug" })))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(again.id, first.id, "the same text is the same task");

    let second = c
        .set_task(&e.id, &body(json!({ "text": "Write the tests" })))
        .await
        .unwrap()
        .into_inner();
    assert_ne!(second.id, first.id);
    let tasks = c.list_tasks(&e.id).await.unwrap().into_inner().items;
    assert_eq!(
        tasks.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
        ["Write the tests", "Fix the login bug"]
    );
    assert!(tasks[0].done_at.is_none() && tasks[1].done_at.is_some());

    for bad in [String::new(), "   ".into(), "x".repeat(201)] {
        let r = c.set_task(&e.id, &body(json!({ "text": bad }))).await;
        assert_eq!(
            r.err().and_then(|e| e.status()).map(|s| s.as_u16()),
            Some(400),
            "a task of {} characters",
            bad.len()
        );
    }
    let r = bob_c
        .set_task(&e.id, &body(json!({ "text": "Bob's" })))
        .await;
    assert_eq!(
        r.err().and_then(|e| e.status()).map(|s| s.as_u16()),
        Some(404)
    );

    c.end_task(&e.id).await.expect("ending the task");
    let now = c.get_environment(&e.id).await.unwrap().into_inner();
    assert!(now.task.is_none(), "a task left after it ended");
    let tasks = c.list_tasks(&e.id).await.unwrap().into_inner().items;
    assert!(tasks.iter().all(|t| t.done_at.is_some()));
}
