//! The client API offers what the web UI's does, as the app uses it:
//! templates, teams, people, images, users, workers, the audit log, and
//! packs' copies and files, each answer as the client API's spec says.

use hangar_suite::{Server, body, eventually, machine};
use serde_json::json;

#[tokio::test]
async fn the_client_api_runs_what_the_web_ui_does() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let c = s.client(&s.token(&admin, "app").await);

    // Templates: made, read, changed, shared, deleted.
    let t = c
        .create_template(&body(machine("Recipe", "private")))
        .await
        .expect("a template")
        .into_inner();
    let got = c.get_template(&t.id).await.expect("it").into_inner();
    assert_eq!(got.name, "Recipe");
    assert!(got.can_edit);
    let mut changed = machine("Recipe 2", "shared");
    changed["description"] = json!("changed in the app");
    let t2 = c
        .update_template(&t.id, &body(changed))
        .await
        .expect("changing it")
        .into_inner();
    assert_eq!(t2.description, "changed in the app");

    // Teams, people and users.
    let team = c
        .create_team(&body(json!({ "name": "Core", "slug": "core" })))
        .await
        .expect("a team")
        .into_inner();
    let user = c
        .create_user(&body(
            json!({ "username": "bob", "password": "a-long-password" }),
        ))
        .await
        .expect("a user")
        .into_inner();
    let people = c.list_people().await.expect("people").into_inner();
    assert!(people.items.iter().any(|p| p.username == "bob"));
    let team = c
        .set_team_member(&team.id, &user.id, &body(json!({ "role": "member" })))
        .await
        .expect("a member")
        .into_inner();
    assert!(team.members.iter().any(|m| m.person.username == "bob"));
    assert!(
        c.list_teams()
            .await
            .expect("teams")
            .into_inner()
            .items
            .len()
            == 1
    );
    c.set_template_collaborators(
        &t.id,
        &body(json!({ "user_ids": [user.id], "team_ids": [team.id] })),
    )
    .await
    .expect("collaborators");
    c.update_user(&user.id, &body(json!({ "admin": true })))
        .await
        .expect("making bob an administrator");
    assert!(
        c.list_users()
            .await
            .expect("users")
            .into_inner()
            .items
            .iter()
            .any(|u| u.username == "bob" && u.admin)
    );

    // Images, workers and the audit log.
    c.list_image_repositories().await.expect("images");
    let w = s.worker("w");
    let worker = eventually("the worker enrolled", || async {
        let ws = c.list_workers().await.ok()?.into_inner();
        ws.items.into_iter().next()
    })
    .await;
    c.get_worker(&worker.id).await.expect("one worker");
    drop(w);
    let audit = c
        .list_audit(None, None, None)
        .await
        .expect("the audit log")
        .into_inner();
    assert!(audit.items.iter().any(|e| e.action == "template.create"));

    // A pack's copies and files.
    let pack = c
        .create_pack(&body(
            json!({ "name": "notes", "paths": [{ "path": "~/notes/", "sensitive": false }] }),
        ))
        .await
        .expect("a pack")
        .into_inner();
    let shared = c
        .create_copy(&pack.id, &body(json!({ "name": "work" })))
        .await
        .expect("a shared copy")
        .into_inner();
    c.rename_copy(&shared.id, &body(json!({ "name": "office" })))
        .await
        .expect("renaming it");
    c.set_default_copy(&pack.id, &body(json!({ "copy_id": shared.id })))
        .await
        .expect("using it by default");
    c.put_copy_file(
        &shared.id,
        "~/notes/todo",
        &body(json!({ "content": "milk\n" })),
    )
    .await
    .expect("a file");
    c.set_copy_file_mode(&shared.id, "~/notes/todo", &body(json!({ "mode": 384 })))
        .await
        .expect("its mode");
    let f = c
        .get_copy_file(&shared.id, "~/notes/todo")
        .await
        .expect("it")
        .into_inner();
    assert_eq!((f.content.as_str(), f.mode), ("milk\n", 384));
    let files = c.get_copy(&shared.id).await.expect("the copy").into_inner();
    assert_eq!(files.files.len(), 1);
    c.delete_copy_file(&shared.id, "~/notes/todo")
        .await
        .expect("deleting it");
    c.delete_copy(&shared.id).await.expect("deleting the copy");
    c.delete_pack(&pack.id).await.expect("deleting the pack");

    c.delete_template(&t.id)
        .await
        .expect("deleting the template");
    c.delete_team(&team.id).await.expect("deleting the team");
    c.delete_user(&user.id).await.expect("deleting bob");
}
