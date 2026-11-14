//! Users: made by an administrator, signing in, kept out when disabled,
//! their passwords changed, and deleted.

use hangar_suite::{ADMIN_PASSWORD, Server, body, status};
use serde_json::json;

#[tokio::test]
async fn an_administrator_makes_a_user_who_signs_in() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let bob = s.user(&admin, "bob").await;
    let me = bob.get_me().await.expect("bob signed in").into_inner();
    assert_eq!(me.username, "bob");
    assert!(!me.admin);
    let users = admin.list_users().await.expect("the users").into_inner();
    assert!(users.iter().any(|u| u.username == "bob" && !u.admin));
    assert_eq!(
        status(
            admin
                .create_user(&body(
                    json!({"username": "bob", "password": "another-password"})
                ))
                .await
        ),
        Some(409),
        "a second bob"
    );
}

#[tokio::test]
async fn a_user_who_is_not_an_administrator_cannot_run_users() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let bob = s.user(&admin, "bob").await;
    assert_eq!(status(bob.list_users().await), Some(403));
    assert_eq!(
        status(
            bob.create_user(&body(
                json!({"username": "eve", "password": "eve-password"})
            ))
            .await
        ),
        Some(403)
    );
}

#[tokio::test]
async fn a_disabled_user_is_signed_out_and_kept_out() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let bob = s.user(&admin, "bob").await;
    let id = bob.get_me().await.unwrap().into_inner().id;
    admin
        .update_user(&id, &body(json!({"disabled": true})))
        .await
        .expect("disabling bob");
    assert_eq!(
        status(bob.get_me().await),
        Some(401),
        "bob's session lives on"
    );
    let again = s
        .frontend()
        .login(&body(
            json!({"username": "bob", "password": "bob-password"}),
        ))
        .await;
    assert_eq!(status(again), Some(401));
    admin
        .update_user(&id, &body(json!({"disabled": false})))
        .await
        .expect("enabling bob");
    s.sign_in("bob", "bob-password").await;
}

#[tokio::test]
async fn a_password_change_ends_the_other_sessions() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let other = s.admin().await;
    admin
        .change_password(&body(json!({
            "current_password": ADMIN_PASSWORD,
            "new_password": "a-new-admin-password",
        })))
        .await
        .expect("changing the password");
    assert_eq!(
        status(other.get_me().await),
        Some(401),
        "the other session lives on"
    );
    admin.get_me().await.expect("the session that changed it");
    s.sign_in("admin", "a-new-admin-password").await;
}

#[tokio::test]
async fn a_deleted_user_is_gone() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let bob = s.user(&admin, "bob").await;
    let id = bob.get_me().await.unwrap().into_inner().id;
    admin.delete_user(&id).await.expect("deleting bob");
    assert_eq!(status(bob.get_me().await), Some(401));
    let users = admin.list_users().await.unwrap().into_inner();
    assert!(users.iter().all(|u| u.username != "bob"));
}
