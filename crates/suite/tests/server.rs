//! What the server is besides its APIs' operations: its health, the web
//! UI on every other path, people's preferences, and the development
//! stack's automatic sign-in.

use hangar_suite::{
    ADMIN, ADMIN_PASSWORD, Server, body, clientapi, eventually, frontendapi, status,
};
use serde_json::json;
use sha2::Digest;

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
    assert_eq!(body["code"], "not_found");

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
    let cache = header(&r, "cache-control");
    let page = r.text().await.unwrap();
    assert!(page.contains("<title>"), "{page}");
    // The UI's page is read afresh each time, so a new build reaches
    // browsers at once; a binary built without the UI only says so.
    if !page.contains("not built into this binary") {
        assert_eq!(cache, "no-cache");
    }

    // The page names its build, as every answer of the web UI's API does,
    // so a page older than its server can tell.
    let named = page
        .split("<meta name=\"hangar-ui\" content=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap_or_default()
        .to_string();
    let r = http
        .get(format!("{}/api/frontend/me", s.url()))
        .send()
        .await
        .unwrap();
    assert_eq!(
        header(&r, "hangar-ui"),
        named,
        "the API names another build"
    );
    if !page.contains("not built into this binary") {
        assert!(!named.is_empty(), "the page names no build: {page}");
    }

    // An asset the UI does not have, such as one a page from an earlier
    // build asks for, is not found, rather than the page.
    let r = http
        .get(format!("{}/assets/index-gone.js", s.url()))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
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

/// A server deployed for someone who keeps its first access token makes
/// its first administrator with that token, of which it is given only the
/// hash, and the token signs them in.
#[tokio::test]
async fn the_initial_administrator_can_be_made_with_a_token() {
    const TOKEN: &str = "hgr_the-token-whoever-deployed-the-server-keeps";
    let Some(s) = Server::start_with(&[
        ("HANGAR_INITIAL_ADMIN_USERNAME", "owner".into()),
        (
            "HANGAR_INITIAL_ADMIN_TOKEN_SHA256",
            "882ddafd298b36913e6dc3d653a82871a229d89ce3ddb7af6fd43011e94ec1e4".into(),
        ),
    ]) else {
        return;
    };
    let http = s.http();
    let me = |token: &'static str| {
        http.get(format!("{}/api/frontend/me", s.url()))
            .bearer_auth(token)
            .send()
    };
    let r = me(TOKEN).await.expect("me");
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.expect("JSON");
    assert_eq!(body["username"], "owner");
    assert_eq!(body["admin"], true);
    let r = me("hgr_another-token").await.expect("me");
    assert_eq!(r.status(), 401);
}

fn sha256_hex(s: &str) -> String {
    sha2::Sha256::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn client_status<T, E>(r: Result<T, clientapi::Error<E>>) -> Option<u16> {
    r.err().and_then(|e| e.status()).map(|s| s.as_u16())
}

/// The initial administrator's token follows the setting: a new hash
/// replaces the token they were given and ends their sessions, whatever
/// name the setting gives them by then, and a token revoked is not given
/// back by the next start.
#[tokio::test]
async fn the_initial_administrators_token_follows_the_setting() {
    const FIRST: &str = "hgr_the-token-its-maker-kept-first";
    const SECOND: &str = "hgr_the-token-its-maker-made-afresh";
    let Some(mut s) = Server::start_with(&[
        ("HANGAR_INITIAL_ADMIN_USERNAME", "owner".into()),
        ("HANGAR_INITIAL_ADMIN_TOKEN_SHA256", sha256_hex(FIRST)),
    ]) else {
        return;
    };
    let me = s.client(FIRST).get_me().await.expect("the first token");
    assert_eq!(me.username, "owner");
    let session = s.sign_in("owner", ADMIN_PASSWORD).await;

    s.restart_with(&[
        ("HANGAR_INITIAL_ADMIN_USERNAME", "someone-else".into()),
        ("HANGAR_INITIAL_ADMIN_TOKEN_SHA256", sha256_hex(SECOND)),
    ]);
    assert_eq!(client_status(s.client(FIRST).get_me().await), Some(401));
    let second = s.client(SECOND);
    let me = second.get_me().await.expect("the second token");
    assert_eq!(
        me.username, "owner",
        "the token goes to whom it went before"
    );
    assert_eq!(
        status(session.get_me().await),
        Some(401),
        "their sessions end"
    );
    let users: serde_json::Value = s
        .http()
        .get(format!("{}/api/frontend/users", s.url()))
        .bearer_auth(SECOND)
        .send()
        .await
        .expect("users")
        .json()
        .await
        .expect("JSON");
    assert!(
        !users.to_string().contains("someone-else"),
        "no one else is made: {users}"
    );

    let tokens = second.list_tokens().await.expect("tokens").into_inner();
    let current = tokens
        .items
        .iter()
        .find(|t| t.current)
        .expect("the token used");
    second
        .revoke_token(&current.id)
        .await
        .expect("revoking the token");
    s.restart_with(&[]);
    assert_eq!(
        client_status(s.client(SECOND).get_me().await),
        Some(401),
        "a token revoked stays revoked"
    );
}

/// An app holding a token signs its person in to the web UI with a link
/// it opens in the browser: the link signs a browser in once and sends it
/// on, and a used one goes to the sign-in page with the same destination.
#[tokio::test]
async fn a_sign_in_link_signs_a_browser_in_once() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let app = s.client(&s.token(&admin, "the app").await);
    let link = app
        .create_sign_in_link(&body(json!({ "to": "/environments" })))
        .await
        .expect("a link")
        .into_inner();
    assert!(
        link.path.starts_with("/api/frontend/auth/link/"),
        "{}",
        link.path
    );

    let browser = s.http_to(&[]);
    let r = browser
        .get(format!("{}{}", s.url(), link.path))
        .send()
        .await
        .expect("following the link");
    assert_eq!(r.status(), 303);
    assert_eq!(r.headers()["location"], "/environments");
    let me = browser
        .get(format!("{}/api/frontend/me", s.url()))
        .send()
        .await
        .expect("me");
    assert_eq!(me.status(), 200, "the browser is signed in");
    let me: serde_json::Value = me.json().await.expect("JSON");
    assert_eq!(me["username"], ADMIN);

    let other = s.http_to(&[]);
    let r = other
        .get(format!("{}{}", s.url(), link.path))
        .send()
        .await
        .expect("following the link again");
    assert_eq!(r.status(), 303);
    assert_eq!(r.headers()["location"], "/login?next=%2Fenvironments");
    let me = other
        .get(format!("{}/api/frontend/me", s.url()))
        .send()
        .await
        .expect("me");
    assert_eq!(me.status(), 401, "a used link signs no one in");

    assert_eq!(
        client_status(
            app.create_sign_in_link(&body(json!({ "to": "//elsewhere.example" })))
                .await
        ),
        Some(400),
        "a link goes on only to a path on the server"
    );
}

/// An administrator made with a token alone, as the app makes a Hangar's,
/// sets a first password with it, and changes it only with the current one.
#[tokio::test]
async fn an_administrator_without_a_password_sets_one() {
    const TOKEN: &str = "hgr_the-token-of-an-owner-without-a-password";
    let Some(s) = Server::start_with(&[
        ("HANGAR_INITIAL_ADMIN_USERNAME", "owner".into()),
        ("HANGAR_INITIAL_ADMIN_PASSWORD", String::new()),
        ("HANGAR_INITIAL_ADMIN_TOKEN_SHA256", sha256_hex(TOKEN)),
    ]) else {
        return;
    };
    let app = s.client(TOKEN);
    assert_eq!(app.get_me().await.expect("me").has_password, Some(false));
    app.change_password(&body(json!({ "new_password": "the-owners-password" })))
        .await
        .expect("setting a first password");
    assert_eq!(app.get_me().await.expect("me").has_password, Some(true));
    s.sign_in("owner", "the-owners-password").await;
    assert_eq!(
        client_status(
            app.change_password(&body(json!({ "new_password": "another-password" })))
                .await
        ),
        Some(400),
        "changing it needs the current one"
    );
    app.change_password(&body(json!({
        "current_password": "the-owners-password",
        "new_password": "another-password",
    })))
    .await
    .expect("changing it with the current one");
}

/// Two places link a person's accounts: each gives the other its key and
/// address, the second takes the first's person, and each then reaches the
/// other, proving itself by its key. A third person ID is refused once
/// linked, and unlinking removes the link.
#[tokio::test]
async fn places_link_and_reach_each_other() {
    let (Some(a), Some(b)) = (Server::start(), Server::start()) else {
        return;
    };
    let ca = a.client(&a.token(&a.admin().await, "the app").await);
    let cb = b.client(&b.token(&b.admin().await, "the app").await);
    let la = ca.list_links().await.expect("a's links").into_inner();
    let lb = cb.list_links().await.expect("b's links").into_inner();
    assert!(la.allowed && la.items.is_empty());
    assert_ne!(la.place.id, lb.place.id);
    assert_ne!(la.person, lb.person);

    ca.put_link(
        &lb.place.id,
        &body(json!({
            "name": "B",
            "public_key": lb.place.public_key,
            "url": b.loopback_url(),
            "person": la.person,
        })),
    )
    .await
    .expect("a links to b");
    let waiting = eventually("a to find b not linked back", || async {
        let l = ca.list_links().await.ok()?.into_inner();
        l.items.into_iter().next()?.reach_error
    })
    .await;
    assert_eq!(waiting, "it has not linked back to this place");

    cb.put_link(
        &la.place.id,
        &body(json!({
            "name": "A",
            "public_key": la.place.public_key,
            "url": a.loopback_url(),
            "person": la.person,
        })),
    )
    .await
    .expect("b links to a");
    for (who, c) in [("a", &ca), ("b", &cb)] {
        let l = eventually(&format!("{who} to reach and be reached"), || async {
            let l = c.list_links().await.ok()?.into_inner();
            let link = l.items.into_iter().next()?;
            // One connection between them: reached by the side that
            // dialled, heard from by the other.
            ((link.reached_at.is_some() || link.heard_at.is_some()) && link.reach_error.is_none())
                .then_some(link)
        })
        .await;
        assert_eq!(l.name, if who == "a" { "B" } else { "A" });
    }
    let lb = cb.list_links().await.expect("b's links").into_inner();
    assert_eq!(lb.person, la.person, "b takes a's person");

    cb.put_link(
        &la.place.id,
        &body(json!({
            "public_key": la.place.public_key,
            "url": a.loopback_url(),
            "person": "6f0a3d52-9a1c-4f7e-8b2d-0c5e4a1b9d33",
        })),
    )
    .await
    .expect("the only link may change person");
    let third = cb
        .put_link(
            "0b6f2d7c-3e1a-4c9b-a5f8-2d7e9c1b4a60",
            &body(json!({
                "public_key": la.place.public_key,
                "url": a.loopback_url(),
                "person": la.person,
            })),
        )
        .await;
    assert_eq!(
        client_status(third),
        Some(409),
        "a second link as another person"
    );

    ca.delete_link(&lb.place.id).await.expect("a unlinks b");
    let l = ca.list_links().await.expect("a's links").into_inner();
    assert!(l.items.is_empty());
}

/// A place whose administrators do not allow personal links says so, and
/// refuses one.
#[tokio::test]
async fn a_place_may_refuse_personal_links() {
    let Some(s) = Server::start_with(&[("HANGAR_PERSONAL_LINKS", "false".into())]) else {
        return;
    };
    let c = s.client(&s.token(&s.admin().await, "the app").await);
    let l = c.list_links().await.expect("links").into_inner();
    assert!(!l.allowed);
    let refused = c
        .put_link(
            "0b6f2d7c-3e1a-4c9b-a5f8-2d7e9c1b4a60",
            &body(json!({
                "public_key": l.place.public_key,
                "url": "https://elsewhere.example",
                "person": l.person,
            })),
        )
        .await;
    assert_eq!(client_status(refused), Some(403));
}
