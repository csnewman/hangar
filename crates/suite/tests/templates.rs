//! Templates and who may do what with them: their owner, collaborators,
//! everyone when shared, and a team's members by their role.

use hangar_suite::{Server, body, frontendapi, machine, status};
use serde_json::json;

async fn template(c: &frontendapi::Client, t: serde_json::Value) -> frontendapi::types::Template {
    c.create_template(&body(t))
        .await
        .expect("making a template")
        .into_inner()
}

async fn seen(c: &frontendapi::Client, id: &str) -> bool {
    let listed = c
        .list_templates()
        .await
        .expect("the templates")
        .into_inner();
    listed.iter().any(|t| t.id == id)
}

/// A private template is its owner's and their collaborators' alone.
#[tokio::test]
async fn a_private_template_is_for_its_owner_and_collaborators() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let bob = s.user(&admin, "bob").await;
    let alice = s.user(&admin, "alice").await;
    let t = template(&alice, machine("Alice's", "private")).await;
    assert!(t.can_edit && t.can_manage);

    assert!(
        !seen(&bob, &t.id).await,
        "bob sees alice's private template"
    );
    assert_eq!(status(bob.get_template(&t.id).await), Some(404));
    // Refused as a template that does not exist is: nothing says it does.
    let use_it = bob
        .create_environment(&body(json!({ "template_id": t.id, "name": "bobs" })))
        .await;
    let nothing = bob
        .create_environment(&body(json!({
            "template_id": "00000000-0000-0000-0000-000000000000",
            "name": "bobs",
        })))
        .await;
    match (&use_it, &nothing) {
        (Err(a), Err(b)) => {
            assert_eq!(a.status(), b.status(), "{a} / {b}");
            assert_eq!(
                a.to_string().split("value:").nth(1),
                b.to_string().split("value:").nth(1)
            );
        }
        _ => panic!("bob made an environment from a template he cannot see"),
    }

    let bob_id = bob.get_me().await.unwrap().into_inner().id;
    alice
        .set_template_collaborators(&t.id, &body(json!({ "user_ids": [bob_id] })))
        .await
        .expect("adding bob");
    assert!(seen(&bob, &t.id).await, "a collaborator cannot see it");
    bob.create_environment(&body(json!({ "template_id": t.id, "name": "bobs" })))
        .await
        .expect("a collaborator makes an environment from it");
}

/// A shared template everyone sees and uses; only its owner changes it.
#[tokio::test]
async fn a_shared_template_is_everyones_to_use() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let bob = s.user(&admin, "bob").await;
    let alice = s.user(&admin, "alice").await;
    let t = template(&alice, machine("Everyone's", "shared")).await;

    assert!(seen(&bob, &t.id).await);
    let as_bob = bob.get_template(&t.id).await.unwrap().into_inner();
    assert!(!as_bob.can_edit);
    bob.create_environment(&body(json!({ "template_id": t.id, "name": "bobs" })))
        .await
        .expect("anyone makes an environment from it");
    assert_eq!(
        status(
            bob.update_template(&t.id, &body(machine("Bob's", "shared")))
                .await
        ),
        Some(403)
    );
    assert_eq!(status(bob.delete_template(&t.id).await), Some(403));

    alice
        .update_template(&t.id, &body(machine("Renamed", "shared")))
        .await
        .expect("its owner changes it");
    assert_eq!(
        bob.get_template(&t.id).await.unwrap().into_inner().name,
        "Renamed"
    );
    alice
        .delete_template(&t.id)
        .await
        .expect("its owner deletes it");
    assert_eq!(status(bob.get_template(&t.id).await), Some(404));
}

/// A team's template: a viewer sees and uses it, a member changes it, an
/// admin deletes it; someone outside the team does none of these.
#[tokio::test]
async fn a_teams_template_goes_by_role() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let bob = s.user(&admin, "bob").await;
    let carol = s.user(&admin, "carol").await;
    let team = admin
        .create_team(&body(json!({ "name": "Core", "slug": "core" })))
        .await
        .expect("making a team")
        .into_inner();
    let bob_id = bob.get_me().await.unwrap().into_inner().id;
    let role = |r: &'static str| body::<frontendapi::types::TeamMembership>(json!({ "role": r }));
    admin
        .set_team_member(&team.id, &bob_id, &role("viewer"))
        .await
        .expect("bob joins as a viewer");

    let mut t = machine("Core service", "private");
    t["team_id"] = json!(team.id);
    let t = template(&admin, t).await;
    assert!(
        seen(&bob, &t.id).await,
        "a viewer cannot see the team's template"
    );
    assert!(
        !seen(&carol, &t.id).await,
        "someone outside the team sees it"
    );
    bob.create_environment(&body(json!({ "template_id": t.id, "name": "bobs" })))
        .await
        .expect("a viewer makes an environment from it");
    let mut changed = machine("Core service, changed", "private");
    changed["team_id"] = json!(team.id);
    assert_eq!(
        status(bob.update_template(&t.id, &body(changed.clone())).await),
        Some(403),
        "a viewer changed it"
    );

    admin
        .set_team_member(&team.id, &bob_id, &role("member"))
        .await
        .expect("bob becomes a member");
    bob.update_template(&t.id, &body(changed))
        .await
        .expect("a member changes it");
    assert_eq!(
        status(bob.delete_template(&t.id).await),
        Some(403),
        "a member deleted it"
    );

    admin
        .set_team_member(&team.id, &bob_id, &role("admin"))
        .await
        .expect("bob becomes an admin");
    bob.delete_template(&t.id)
        .await
        .expect("a team admin deletes it");
}

/// What a template keeps from its environments, for code its owner does
/// not trust, is kept with it and given back as it was set.
#[tokio::test]
async fn what_a_template_keeps_from_its_environments_is_kept() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let mut t = machine("Untrusted", "private");
    t["spec"]["access"] = json!({ "no_registry": true, "no_ssh_keys": true });
    let made = admin
        .create_template(&body(t))
        .await
        .expect("a template")
        .into_inner();
    let got = admin.get_template(&made.id).await.expect("it").into_inner();
    let access = got
        .spec
        .access
        .expect("what it keeps from its environments");
    assert_eq!(access.no_registry, Some(true));
    assert_eq!(access.no_ssh_keys, Some(true));
    assert_ne!(access.no_sensitive_files, Some(true));
}
