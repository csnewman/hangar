//! Signing in: a session through the web UI's API, and access tokens for
//! the client API.

use hangar_suite::clientapi::ClientInfo;
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

/// A change a browser says another site caused is refused when it would
/// ride on the session's cookie; an access token is no browser's to send.
#[tokio::test]
async fn cross_site_changes_are_refused() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let http = admin.client();
    let made = std::sync::atomic::AtomicU32::new(0);
    let post = |headers: &[(&str, &str)]| {
        let n = made.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let template = hangar_suite::machine(&format!("Cross-site {n}"), "private");
        let mut r = http
            .post(format!("{}/api/frontend/templates", s.url()))
            .json(&template);
        for (k, v) in headers {
            r = r.header(*k, *v);
        }
        async move { r.send().await.expect("the request").status().as_u16() }
    };
    assert_eq!(post(&[("Sec-Fetch-Site", "cross-site")]).await, 403);
    assert_eq!(post(&[("Sec-Fetch-Site", "same-site")]).await, 403);
    assert_eq!(post(&[("Origin", "https://evil.example")]).await, 403);
    assert_eq!(post(&[("Sec-Fetch-Site", "same-origin")]).await, 201);
    assert_eq!(post(&[("Origin", &s.url())]).await, 201);
    assert_eq!(post(&[]).await, 201);

    // The client API takes the cookie too, under the same rule.
    let out = http
        .delete(format!(
            "{}/api/v1/me/tokens/00000000-0000-4000-8000-000000000001",
            s.url()
        ))
        .header("Sec-Fetch-Site", "cross-site")
        .send()
        .await
        .unwrap();
    assert_eq!(out.status().as_u16(), 403);
    let token = s.token(&admin, "cross-site").await;
    let out = reqwest::Client::new()
        .delete(format!(
            "{}/api/v1/me/tokens/00000000-0000-4000-8000-000000000001",
            s.url()
        ))
        .header("Sec-Fetch-Site", "cross-site")
        .bearer_auth(&token);
    let out = s.http().execute(out.build().unwrap()).await.unwrap();
    assert_eq!(
        out.status().as_u16(),
        404,
        "a token's request refused as cross-site"
    );
}
