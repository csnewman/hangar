//! An environment's own web servers, on hosts of their own: a public
//! environment's site is anyone's, a private one's and every numbered port
//! need signing in, and the sign-in reaches them through the tunnel, which
//! carries many requests at once, slow or large, over HTTP/1.1 and HTTP/2.

use hangar_suite::{ADMIN, ADMIN_PASSWORD, Server, body, eventually, frontendapi, machine};
use serde_json::json;

/// A running environment, its short ID, and a session of its owner's on
/// http, which reaches hosts.
async fn running(
    s: &Server,
    http: reqwest::Client,
) -> (frontendapi::Client, frontendapi::types::Environment) {
    running_at(&s.url(), http).await
}

/// As running, through the server at url.
async fn running_at(
    url: &str,
    http: reqwest::Client,
) -> (frontendapi::Client, frontendapi::types::Environment) {
    let admin = frontendapi::Client::new_with_client(url, http);
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

#[tokio::test]
async fn many_slow_requests_at_once_are_all_answered() {
    let Some(s) = Server::start() else { return };
    let _w = s.worker("w");
    let (admin, e) = running(&s, s.http()).await;
    admin
        .set_environment_ports(&e.id, &body(json!({ "public": true })))
        .await
        .unwrap();
    let site = s.environment_host("site", &e.short_id);
    let anyone = s.http_to(&[&site]);
    let n = 40;
    let mut asks = Vec::new();
    for i in 0..n {
        let url = format!("http://{site}:{}/slow/3000?{i}", s.port());
        let http = anyone.clone();
        asks.push(tokio::spawn(async move {
            let r = http.get(&url).send().await.map_err(|e| e.to_string())?;
            let status = r.status().as_u16();
            let text = r.text().await.map_err(|e| e.to_string())?;
            Ok::<_, String>((status, text))
        }));
    }
    let mut failed = Vec::new();
    for a in asks {
        match a.await.unwrap() {
            Ok((200, _)) => {}
            Ok((status, text)) => failed.push(format!("{status}: {text}")),
            Err(e) => failed.push(e),
        }
    }
    assert!(
        failed.is_empty(),
        "{} of {n} failed: {failed:#?}",
        failed.len()
    );
}

#[tokio::test]
async fn many_large_answers_read_slowly_at_once_all_arrive() {
    use futures_util::StreamExt;
    let Some(s) = Server::start() else { return };
    let _w = s.worker("w");
    let (admin, e) = running(&s, s.http()).await;
    admin
        .set_environment_ports(&e.id, &body(json!({ "public": true })))
        .await
        .unwrap();
    let site = s.environment_host("site", &e.short_id);
    let anyone = s.http_to(&[&site]);
    let n = 40;
    let size = 4 << 20;
    let mut asks = Vec::new();
    for i in 0..n {
        let url = format!("http://{site}:{}/big/{size}/{i}", s.port());
        let http = anyone.clone();
        asks.push(tokio::spawn(async move {
            let r = http
                .get(&url)
                .send()
                .await
                .map_err(|e| format!("{i}: {e}"))?;
            let status = r.status().as_u16();
            let mut got = 0;
            let mut body = r.bytes_stream();
            while let Some(c) = body.next().await {
                got += c.map_err(|e| format!("{i} after {got}: {e}"))?.len();
                if i % 2 == 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            }
            Ok::<_, String>((status, got))
        }));
    }
    let mut failed = Vec::new();
    for a in asks {
        match a.await.unwrap() {
            Ok((200, got)) if got == size => {}
            Ok((status, got)) => failed.push(format!("{status}, {got} bytes")),
            Err(e) => failed.push(e),
        }
    }
    assert!(
        failed.is_empty(),
        "{} of {n} failed: {failed:#?}",
        failed.len()
    );
}

/// A CA and a certificate it issued for the zone and every name under it,
/// written to dir: the chain and key files, and the CA.
fn issue(dir: &std::path::Path) -> (String, String, rustls::pki_types::CertificateDer<'static>) {
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let mut ca = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    ca.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let ca_cert = ca.self_signed(&ca_key).unwrap();
    let issuer = rcgen::Issuer::new(ca, ca_key);
    let key = rcgen::KeyPair::generate().unwrap();
    let host = hangar_suite::HOST;
    let leaf = rcgen::CertificateParams::new(vec![host.to_string(), format!("*.{host}")])
        .unwrap()
        .signed_by(&key, &issuer)
        .unwrap();
    let (cert_file, key_file) = (dir.join("cert.pem"), dir.join("key.pem"));
    std::fs::write(&cert_file, leaf.pem() + &ca_cert.pem()).unwrap();
    std::fs::write(&key_file, key.serialize_pem()).unwrap();
    (
        cert_file.display().to_string(),
        key_file.display().to_string(),
        ca_cert.der().clone(),
    )
}

#[tokio::test]
async fn many_requests_at_once_over_http2_are_all_answered() {
    use futures_util::StreamExt;
    let dir = std::env::temp_dir().join(format!("hangar-suite-ports-h2-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (cert, key, root) = issue(&dir);
    let tls = hangar_suite::free_port();
    let host = hangar_suite::HOST;
    let Some(s) = Server::start_with(&[
        ("HANGAR_PUBLIC_URL", format!("https://{host}:{tls}")),
        ("HANGAR_TLS_LISTEN", format!("127.0.0.1:{tls}")),
        ("HANGAR_TLS_CERT_FILE", cert),
        ("HANGAR_TLS_KEY_FILE", key),
    ]) else {
        return;
    };
    let _w = s.worker("w");
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], tls));
    let mut roots = rustls::RootCertStore::empty();
    roots.add(root).unwrap();
    let mut cfg = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    cfg.alpn_protocols = vec![b"h2".to_vec()];
    let http = reqwest::Client::builder()
        .tls_backend_preconfigured(cfg.clone())
        .resolve(host, addr)
        .cookie_store(true)
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .unwrap();
    let origin = format!("https://{host}:{tls}");
    hangar_suite::eventually("HTTPS served", || async {
        http.get(format!("{origin}/api/healthz"))
            .send()
            .await
            .ok()
            .map(|_| ())
    })
    .await;
    let (_, e) = running_at(&origin, http.clone()).await;
    let numbered = s.environment_host("8080", &e.short_id);
    let http = reqwest::Client::builder()
        .tls_backend_preconfigured(cfg)
        .resolve(host, addr)
        .resolve(&numbered, addr)
        .cookie_store(true)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .unwrap();
    let r = http
        .post(format!("{origin}/api/frontend/auth/login"))
        .header("Origin", &origin)
        .json(&json!({ "username": ADMIN, "password": ADMIN_PASSWORD }))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "{}", r.status());
    let site = format!("https://{numbered}:{tls}");
    let start = format!(
        "{origin}/api/frontend/environments/{}/ports/sign-in?to={}",
        e.id,
        url_escape(&format!("{site}/"))
    );
    let (status, _, page) = get(&http, &start).await;
    assert_eq!(status, 200, "{page}");

    let n = 40;
    let size = 4 << 20;
    let mut asks = Vec::new();
    for i in 0..n {
        let url = if i % 2 == 0 {
            format!("{site}/slow/3000/{i}")
        } else {
            format!("{site}/big/{size}/{i}")
        };
        let http = http.clone();
        asks.push(tokio::spawn(async move {
            let r = http
                .get(&url)
                .send()
                .await
                .map_err(|e| format!("{url}: {e:?}"))?;
            let (status, version) = (r.status().as_u16(), r.version());
            let mut got = 0;
            let mut body = r.bytes_stream();
            while let Some(c) = body.next().await {
                got += c.map_err(|e| format!("{url} after {got}: {e:?}"))?.len();
            }
            Ok::<_, String>((url, status, version, got))
        }));
    }
    let mut failed = Vec::new();
    for a in asks {
        match a.await.unwrap() {
            Ok((_, 200, reqwest::Version::HTTP_2, _)) => {}
            Ok((url, status, version, got)) => {
                failed.push(format!("{url}: {status} {version:?}, {got} bytes"))
            }
            Err(e) => failed.push(e),
        }
    }
    assert!(
        failed.is_empty(),
        "{} of {n} failed: {failed:#?}",
        failed.len()
    );
}
