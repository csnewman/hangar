//! An environment's own web servers, on hosts of their own: a public
//! environment's site is anyone's, a private one's and every numbered port
//! need signing in, and the sign-in reaches them through the tunnel.

use hangar_suite::{ADMIN, ADMIN_PASSWORD, Server, body, eventually, frontendapi, machine};
use serde_json::json;

/// A running environment, its short ID, and a session of its owner's on
/// http, which reaches hosts.
async fn running(
    s: &Server,
    http: reqwest::Client,
) -> (frontendapi::Client, frontendapi::types::Environment) {
    let admin = frontendapi::Client::new_with_client(&s.url(), http);
    admin
        .login(&body(
            json!({ "username": ADMIN, "password": ADMIN_PASSWORD }),
        ))
        .await
        .expect("signing in");
    let t = admin
        .create_template(&body(machine("Web", "private")))
        .await
        .unwrap()
        .into_inner();
    let e = admin
        .create_environment(&body(json!({ "template_id": t.id, "name": "web" })))
        .await
        .unwrap()
        .into_inner();
    let id = e.id.clone();
    eventually("the environment running", || async {
        let e = admin.get_environment(&id).await.ok()?.into_inner();
        (serde_json::to_value(e.phase).ok()? == json!("running")).then_some(())
    })
    .await;
    (admin, e)
}

/// GETs url, following up to five redirects by hand, and gives the last
/// answer's status, where it pointed and its body.
async fn get(http: &reqwest::Client, url: &str) -> (u16, String, String) {
    let mut url = url.to_string();
    for _ in 0..5 {
        let r = http.get(&url).send().await.expect("a request");
        let status = r.status().as_u16();
        let to = r
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        match to {
            Some(to) if (300..400).contains(&status) => {
                url = if to.starts_with("http") {
                    to
                } else {
                    let base = reqwest::Url::parse(&url).unwrap();
                    base.join(&to).unwrap().to_string()
                };
            }
            _ => return (status, url, r.text().await.unwrap_or_default()),
        }
    }
    panic!("more than five redirects from {url}");
}

#[tokio::test]
async fn a_public_environments_site_is_anyones() {
    let Some(s) = Server::start() else { return };
    let _w = s.worker("w");
    let (admin, e) = running(&s, s.http()).await;
    let site = s.environment_host("site", &e.short_id);
    let url = format!("http://{site}:{}/hello", s.port());

    let anyone = s.http_to(&[&site]);
    let r = anyone.get(&url).send().await.unwrap();
    assert!(
        (300..400).contains(&r.status().as_u16()),
        "a private environment's site answered {} to no one",
        r.status()
    );

    admin
        .set_environment_ports(&e.id, &body(json!({ "public": true })))
        .await
        .unwrap();
    let r = anyone.get(&url).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 200);
    assert_eq!(
        r.text().await.unwrap(),
        format!("guest port 80, {site}:{}, /hello", s.port())
    );

    let numbered = s.environment_host("8080", &e.short_id);
    let anyone = s.http_to(&[&numbered]);
    let r = anyone
        .get(format!("http://{numbered}:{}/", s.port()))
        .send()
        .await
        .unwrap();
    assert!(
        (300..400).contains(&r.status().as_u16()),
        "a numbered port answered {} to no one",
        r.status()
    );
}

#[tokio::test]
async fn its_owner_signs_in_to_a_numbered_port() {
    let Some(s) = Server::start() else { return };
    let _w = s.worker("w");
    let web = s.http();
    let (_, e) = running(&s, web).await;
    let numbered = s.environment_host("8080", &e.short_id);
    let closed = s.environment_host("9", &e.short_id);
    let http = s.http_to(&[&numbered, &closed]);
    let owner = frontendapi::Client::new_with_client(&s.url(), http.clone());
    owner
        .login(&body(
            json!({ "username": ADMIN, "password": ADMIN_PASSWORD }),
        ))
        .await
        .unwrap();

    let to = format!("http://{numbered}:{}/app", s.port());
    let start = format!(
        "{}/api/frontend/environments/{}/ports/sign-in?to={}",
        s.url(),
        e.id,
        url_escape(&to)
    );
    let (status, at, page) = get(&http, &start).await;
    assert_eq!((status, at.as_str()), (200, to.as_str()), "{page}");
    assert_eq!(
        page,
        format!("guest port 8080, {numbered}:{}, /app", s.port())
    );

    let (status, _, page) = get(&http, &format!("http://{closed}:{}/", s.port())).await;
    assert_ne!(status, 200, "a port nothing listens on answered: {page}");
}

fn url_escape(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            b => format!("%{b:02X}"),
        })
        .collect()
}
