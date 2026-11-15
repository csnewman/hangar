//! The audit log: who did what, newest first, an administrator seeing all
//! of it and anyone else what concerns them; and an environment's
//! activity, its events alone.

use hangar_suite::{Server, body, eventually, machine};
use serde_json::json;

fn kind(a: &impl serde::Serialize) -> String {
    serde_json::to_value(a).unwrap()["kind"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn the_audit_log_says_who_did_what() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let _w = s.worker("w");
    let bob = s.user(&admin, "bob").await;
    let carol = s.user(&admin, "carol").await;
    let t = bob
        .create_template(&body(machine("Bob's", "shared")))
        .await
        .unwrap()
        .into_inner();
    let e = bob
        .create_environment(&body(json!({ "template_id": t.id, "name": "bobs" })))
        .await
        .unwrap()
        .into_inner();
    let subject = vec![format!("environment:{}", e.id)];

    // Placement is Hangar's own doing.
    let placed = eventually("the environment placed", || async {
        let events = admin
            .list_audit(None, None, Some(&subject))
            .await
            .ok()?
            .into_inner();
        events.into_iter().find(|x| x.action == "environment.place")
    })
    .await;
    assert_eq!(kind(&placed.actor), "system");
    assert_eq!(placed.actor.name, "hangar:placement");

    let all = admin
        .list_audit(None, None, Some(&subject))
        .await
        .unwrap()
        .into_inner();
    assert!(
        all.windows(2).all(|w| w[0].id > w[1].id),
        "not newest first"
    );
    assert!(all.iter().all(|x| x.subjects.contains(&subject[0])));
    let made = all
        .iter()
        .find(|x| x.action == "environment.create")
        .expect("the environment's creation");
    assert_eq!(
        (kind(&made.actor), made.actor.name.as_str()),
        ("person".into(), "bob")
    );
    assert_eq!(made.target.id, e.id);
    assert!(
        made.ip.is_some(),
        "an administrator is not shown where it came from"
    );

    let page = admin
        .list_audit(None, Some(1.try_into().unwrap()), Some(&subject))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(page.len(), 1);
    let older = admin
        .list_audit(Some(page[0].id), None, Some(&subject))
        .await
        .unwrap()
        .into_inner();
    assert!(older.iter().all(|x| x.id < page[0].id));
    assert_eq!(older.len(), all.len() - 1);

    // Bob sees what is his, without addresses; Carol sees none of it.
    let bobs = bob
        .list_audit(None, None, Some(&subject))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(bobs.len(), all.len());
    assert!(bobs.iter().all(|x| x.ip.is_none()));
    let carols = carol
        .list_audit(None, None, Some(&subject))
        .await
        .unwrap()
        .into_inner();
    assert!(carols.is_empty(), "carol sees bob's environment's events");
    let everything = carol
        .list_audit(None, None, None)
        .await
        .unwrap()
        .into_inner();
    let me = carol.get_me().await.unwrap().into_inner().id;
    assert!(
        !everything.is_empty()
            && everything
                .iter()
                .all(|x| x.subjects.iter().any(|s| s.ends_with(&me))),
        "carol sees events not about her"
    );

    // A token's change says it was by the token.
    let token = s.token(&bob, "bobs-laptop").await;
    let c = s.client(&token);
    c.stop_environment(&e.id).await.expect("stopping by token");
    let activity = c
        .list_activity(&e.id, None, None)
        .await
        .expect("the environment's activity")
        .into_inner()
        .items;
    let stop = activity
        .iter()
        .find(|x| x.action == "environment.stop")
        .expect("the stop");
    assert_eq!(stop.details["via"], json!("access token bobs-laptop"));
    assert_eq!(stop.actor.name, "bob");
    // A full page says where the next is, and following it carries on
    // from there; the last page says nothing.
    assert!(activity.len() >= 2, "{activity:?}");
    let first = c
        .list_activity(&e.id, None, Some(1))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(first.items.len(), 1);
    let next = first.next.expect("a page that is not the last says so");
    let r = s
        .http()
        .get(format!("{}{next}", s.url()))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let second: serde_json::Value = r.json().await.unwrap();
    assert_eq!(second["items"][0]["id"], json!(activity[1].id), "{second}");
    let all = c
        .list_activity(&e.id, None, Some(500))
        .await
        .unwrap()
        .into_inner();
    assert!(all.next.is_none(), "the last page names a next");
    let carol_c = s.client(&s.token(&carol, "carols").await);
    let refused = carol_c.list_activity(&e.id, None, None).await;
    assert_eq!(
        refused.err().and_then(|e| e.status()).map(|s| s.as_u16()),
        Some(404)
    );
}
