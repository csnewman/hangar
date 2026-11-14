//! Signing in: a session through the web UI's API, and access tokens for
//! the client API.

use hangar_suite::{ADMIN, Server, frontendapi};

/// The administrator signs in with their password and is who the server
/// says; a wrong password is refused, and signing out ends the session.
#[tokio::test]
async fn a_session_is_signed_in_and_out() {
    let Some(s) = Server::start() else { return };
    let c = s.admin().await;
    let me = c.get_me().await.expect("who is signed in").into_inner();
    assert_eq!(me.username, ADMIN);
    assert!(me.admin);
    assert!(me.has_password);

    let wrong = s
        .frontend()
        .login(&frontendapi::types::Login {
            username: ADMIN.parse().unwrap(),
            password: "not-the-password".parse().unwrap(),
        })
        .await
        .expect_err("a wrong password");
    assert_eq!(wrong.status().map(|s| s.as_u16()), Some(401));

    c.logout().await.expect("signing out");
    let after = c.get_me().await.expect_err("signed out");
    assert_eq!(after.status().map(|s| s.as_u16()), Some(401));
}

/// Nobody is signed in without a session.
#[tokio::test]
async fn nothing_is_shown_to_no_one() {
    let Some(s) = Server::start() else { return };
    let e = s.frontend().get_me().await.expect_err("no session");
    assert_eq!(e.status().map(|s| s.as_u16()), Some(401));
    let e = s.anonymous().get_me().await.expect_err("no token");
    assert_eq!(e.status().map(|s| s.as_u16()), Some(401));
}

/// A token made through a session signs the client API in as its maker,
/// shows in the list of tokens, and stops working once revoked.
#[tokio::test]
async fn a_token_signs_in_until_revoked() {
    let Some(s) = Server::start() else { return };
    let session = s.admin().await;
    let token = s.token(&session, "suite").await;
    assert!(token.starts_with("hgr_"), "{token}");

    let c = s.client(&token);
    let me = c
        .get_me()
        .await
        .expect("signed in by the token")
        .into_inner();
    assert_eq!(me.username, ADMIN);
    let version = s
        .anonymous()
        .get_version()
        .await
        .expect("the version")
        .into_inner();
    assert!(version.versions.iter().any(|v| v == "v1"), "{version:?}");

    let tokens = c.list_tokens().await.expect("the tokens").into_inner();
    let mine = tokens
        .iter()
        .find(|t| t.name == "suite")
        .expect("the token listed");
    c.revoke_token(&mine.id).await.expect("revoking it");
    let e = c.get_me().await.expect_err("a revoked token");
    assert_eq!(e.status().map(|s| s.as_u16()), Some(401));
}
