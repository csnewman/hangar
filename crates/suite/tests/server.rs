//! What the server is besides its APIs' operations: its health, the web
//! UI on every other path, people's preferences, and the development
//! stack's automatic sign-in.

use hangar_suite::{ADMIN, Server, frontendapi, status};

/// The server says it is healthy, knows no API endpoint it does not serve,
/// and serves the web UI's page on every other path, so the UI's own
/// routes survive a reload.
#[tokio::test]
async fn health_endpoints_and_the_web_ui() {
    let Some(s) = Server::start() else { return };
    let http = s.http();
    let r = http
        .get(format!("{}/api/healthz", s.url()))
        .send()
        .await
        .expect("healthz");
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.expect("JSON");
    assert_eq!(body, serde_json::json!({ "status": "ok" }));

    let r = http
        .get(format!("{}/api/no/such/thing", s.url()))
        .send()
        .await
        .expect("an unknown endpoint");
    assert_eq!(r.status(), 404);
    let body: serde_json::Value = r.json().await.expect("JSON");
    assert_eq!(body["error"], "no such endpoint");

    let r = http
        .get(format!("{}/environments/somewhere", s.url()))
        .send()
        .await
        .expect("a page of the UI");
    assert_eq!(r.status(), 200);
    let header = |r: &reqwest::Response, h: &str| {
        r.headers()
            .get(h)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string()
    };
    assert!(
        header(&r, "content-type").starts_with("text/html"),
        "{}",
        header(&r, "content-type")
    );
    assert_eq!(header(&r, "cache-control"), "no-cache");
    assert!(r.text().await.unwrap().contains("<title>"));
}

/// A person's theme is theirs wherever they sign in; one that is not a
/// theme is refused.
#[tokio::test]
async fn preferences_follow_the_person() {
    let Some(s) = Server::start() else { return };
    let c = s.admin().await;
    c.set_preferences(&frontendapi::types::Preferences {
        theme: frontendapi::types::Theme::Dark,
    })
    .await
    .expect("setting the theme");
    let again = s.admin().await;
    let me = again.get_me().await.expect("who").into_inner();
    assert_eq!(me.theme, frontendapi::types::Theme::Dark);

    let http = s.http();
    let r = http
        .post(format!("{}/api/frontend/auth/login", s.url()))
        .header("Origin", s.url())
        .json(&serde_json::json!({ "username": ADMIN, "password": hangar_suite::ADMIN_PASSWORD }))
        .send()
        .await
        .expect("signing in");
    assert_eq!(r.status(), 200);
    let r = http
        .put(format!("{}/api/frontend/me/preferences", s.url()))
        .header("Origin", s.url())
        .json(&serde_json::json!({ "theme": "sepia" }))
        .send()
        .await
        .expect("an unknown theme");
    assert_eq!(r.status(), 400);
    assert_eq!(
        status(
            s.frontend()
                .set_preferences(&frontendapi::types::Preferences {
                    theme: frontendapi::types::Theme::Light,
                })
                .await
        ),
        Some(401)
    );
}

/// With automatic sign-in on, the web UI's API signs a visitor with no
/// session in as the person named, once, and their session's cookie does
/// the rest; the client API still asks for a token.
#[tokio::test]
async fn the_development_stack_signs_everyone_in() {
    let Some(s) = Server::start_with(&[("HANGAR_DEV_AUTO_SIGN_IN", ADMIN.into())]) else {
        return;
    };
    let http = s.http();
    let me = |http: reqwest::Client| {
        let url = format!("{}/api/frontend/me", s.url());
        async move { http.get(url).send().await.expect("who") }
    };
    let first = me(http.clone()).await;
    assert_eq!(first.status(), 200);
    assert!(
        first.headers().get("set-cookie").is_some(),
        "a session's cookie is set"
    );
    let body: serde_json::Value = first.json().await.unwrap();
    assert_eq!(body["username"], ADMIN);
    let second = me(http.clone()).await;
    assert_eq!(second.status(), 200);
    assert!(
        second.headers().get("set-cookie").is_none(),
        "the cookie signs the next request in"
    );

    assert_eq!(status(s.anonymous().get_me().await), Some(401));
}
