//! Each environment's own web servers, on hosts of their own.
//!
//! What those servers send is the environment's to decide. Served from
//! Hangar's origin it could call Hangar's API as the signed-in person; on
//! hosts of their own, siblings of Hangar's (<name>-env<short>.<host>),
//! they cannot read Hangar's cookie or its answers, and Hangar refuses
//! their changes as cross-origin. Those hosts cannot see the Hangar
//! session, so they have sessions of their own: Hangar gives the browser a
//! ticket, the host redeems it for a cookie of its own, and the cookie
//! never outlives the Hangar session it came from.
//!
//! A request that arrived over HTTPS goes to the environment's port 443,
//! over TLS whose certificate is not checked (Hangar's is what the browser
//! sees), and one over plain HTTP to its port 80. A name that is a port
//! number reaches that port over plain HTTP: 5173-env<short> is a
//! development server on 5173. A public environment's ports 80 and 443 are
//! anyone's; every other is reached signed in, by whoever may reach the
//! environment.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use chrono::{Duration, Utc};
use rand::RngCore;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

use crate::http::Shared;
use crate::publichost::Public;
use crate::store::rows::{Environment, HostSession};
use crate::store::{Result as StoreResult, Store};
use crate::tunnel::{self, Header as StreamHeader, OpenError};

/// An environment host's session cookie.
pub const COOKIE: &str = "hangar_env";

/// Where an environment's host redeems a ticket, a path its own web
/// servers are unlikely to use.
const SIGN_IN: &str = "/_hangar/sign-in";

/// How long a browser has to redeem a ticket: Hangar opens the link at
/// once.
const TICKET_LIFETIME: Duration = Duration::minutes(1);

/// How long a host's cookie lasts unused; each use extends it.
const COOKIE_LIFETIME: Duration = Duration::hours(24);

/// How stale a cookie's expiry may be before a use extends it.
const COOKIE_REFRESH: Duration = Duration::hours(1);

/// How long the environment has to answer a request.
const ANSWER_WITHIN: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// The label before an environment's short ID on its hosts.
const LABEL: &str = "env";

/// Where on Hangar's own origin a browser is sent to sign in to an
/// environment's hosts, with the URL it asked for as to.
pub fn sign_in_path(env: Uuid) -> String {
    format!("/api/frontend/environments/{env}/ports/sign-in")
}

fn short_id(s: &str) -> bool {
    s.len() == 6
        && s.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// The short ID of the environment whose web servers host is one of:
/// env<short>, alone or after a name and a hyphen.
pub fn owns(public: &Public, host: &str) -> Option<String> {
    let label = public.label(host)?;
    if label.len() < LABEL.len() + 6 || !label.is_char_boundary(label.len() - 6) {
        return None;
    }
    let (rest, short) = label.split_at(label.len() - 6);
    let name = rest.strip_suffix(LABEL)?;
    if !short_id(short) || (!name.is_empty() && (name.len() < 2 || !name.ends_with('-'))) {
        return None;
    }
    Some(short.to_string())
}

/// The port a host names, if its name is a port number: 1 to 65535, with
/// no leading zero.
fn numbered(public: &Public, host: &str) -> Option<u16> {
    owns(public, host)?;
    let label = public.label(host)?;
    let name = label[..label.len() - LABEL.len() - 6].strip_suffix('-')?;
    let n: u16 = name.parse().ok()?;
    (n >= 1 && n.to_string() == name).then_some(n)
}

fn token() -> (String, Vec<u8>) {
    let mut raw = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut raw);
    let t = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
    let hash = Sha256::digest(t.as_bytes()).to_vec();
    (t, hash)
}

/// A ticket to one environment's hosts for the Hangar session whose token
/// is given; whether its person may reach the environment is the caller's
/// to have checked.
async fn ticket(store: &Store, session: &str, env: Uuid) -> StoreResult<String> {
    let (t, hash) = token();
    let mut tx = store.begin().await?;
    tx.insert_host_session(&HostSession {
        token_hash: hash,
        session_hash: Sha256::digest(session.as_bytes()).to_vec(),
        environment_id: env,
        ticket: true,
        expires_at: Utc::now() + TICKET_LIFETIME,
    })
    .await?;
    tx.commit().await?;
    Ok(t)
}

/// A cookie for a ticket, which works once; it takes the place of any the
/// same Hangar session had for the environment.
async fn redeem(store: &Store, ticket: &str, env: Uuid) -> StoreResult<Option<String>> {
    if ticket.is_empty() {
        return Ok(None);
    }
    let mut tx = store.begin().await?;
    let hash = Sha256::digest(ticket.as_bytes()).to_vec();
    let Some(session) = tx.redeem_host_ticket(&hash, env, Utc::now()).await? else {
        return Ok(None);
    };
    let (cookie, hash) = token();
    tx.insert_host_session(&HostSession {
        token_hash: hash,
        session_hash: session,
        environment_id: env,
        ticket: false,
        expires_at: Utc::now() + COOKIE_LIFETIME,
    })
    .await?;
    tx.commit().await?;
    Ok(Some(cookie))
}

/// The environment a host's cookie reaches, while it and the Hangar
/// session behind it last and its person is not disabled.
async fn authenticate(store: &Store, cookie: &str, env: Uuid) -> StoreResult<Option<Environment>> {
    if cookie.is_empty() {
        return Ok(None);
    }
    let hash = Sha256::digest(cookie.as_bytes()).to_vec();
    let now = Utc::now();
    let mut tx = store.begin().await?;
    let Some((e, expires)) = tx.host_cookie(&hash, env, now).await? else {
        return Ok(None);
    };
    if expires - now < COOKIE_LIFETIME - COOKIE_REFRESH {
        tx.extend_host_cookie(&hash, now + COOKIE_LIFETIME).await?;
        tx.commit().await?;
    }
    Ok(Some(e))
}

/// The URL on the environment's host that redeems a new ticket for the
/// Hangar session and goes on to to, an absolute URL on one of the
/// environment's hosts; Err says why to is not one.
pub async fn sign_in_url(
    store: &Store,
    public: &Public,
    session: &str,
    env: Uuid,
    short: &str,
    to: &str,
) -> Result<StoreResult<String>, String> {
    let bad = || format!("{to:?} is not a URL on the environment's host");
    let u = url::Url::parse(to).map_err(|_| bad())?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err(bad());
    }
    let host = match (u.host_str(), u.port()) {
        (Some(h), Some(p)) => format!("{h}:{p}"),
        (Some(h), None) => h.to_string(),
        _ => return Err(bad()),
    };
    if owns(public, &host).as_deref() != Some(short) {
        return Err(bad());
    }
    let t = match ticket(store, session, env).await {
        Ok(t) => t,
        Err(e) => return Ok(Err(e)),
    };
    let mut back = u.path().to_string();
    if let Some(q) = u.query() {
        back.push('?');
        back.push_str(q);
    }
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("ticket", &t)
        .append_pair("to", &back)
        .finish();
    Ok(Ok(format!("{}://{host}{SIGN_IN}?{query}", u.scheme())))
}

fn text(status: StatusCode, msg: &str) -> Response {
    (
        status,
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        format!("{msg}\n"),
    )
        .into_response()
}

fn see_other(to: &str) -> Response {
    let mut r = StatusCode::SEE_OTHER.into_response();
    if let Ok(v) = HeaderValue::from_str(to) {
        r.headers_mut().insert(header::LOCATION, v);
    }
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

fn get<'a>(h: &'a HeaderMap, name: &str) -> &'a str {
    h.get(name).and_then(|v| v.to_str().ok()).unwrap_or("")
}

/// Whether a private environment's host takes a request: another site,
/// another environment's host among them, may open a page on it and do
/// nothing else, since every such host is one site with Hangar and each
/// other, and the SameSite cookie alone would let one environment's pages
/// act on another's.
fn allowed(h: &HeaderMap, host: &str, secure: bool) -> bool {
    let navigate = get(h, "sec-fetch-mode") == "navigate";
    if !matches!(get(h, "sec-fetch-site"), "" | "same-origin" | "none") && !navigate {
        return false;
    }
    let scheme = if secure { "https" } else { "http" };
    let origin = get(h, "origin");
    origin.is_empty() || navigate || origin == format!("{scheme}://{host}")
}

/// Serves a request to one of an environment's hosts, which owns has said
/// it is.
pub async fn serve(app: Shared, req: Request) -> Response {
    let Some(public) = app.cfg.public.clone() else {
        return text(StatusCode::NOT_FOUND, "There is no such environment.");
    };
    let host = get(req.headers(), "host").to_string();
    let Some(short) = owns(&public, &host) else {
        return text(StatusCode::NOT_FOUND, "There is no such environment.");
    };
    let numbered = numbered(&public, &host);
    let secure = get(req.headers(), "x-forwarded-proto") == "https";
    let found = async {
        app.store
            .begin()
            .await?
            .environment_by_short_id(&short)
            .await
    };
    let mut e = match found.await {
        Ok(Some(e)) => e,
        Ok(None) => return text(StatusCode::NOT_FOUND, "There is no such environment."),
        Err(err) => {
            eprintln!("hangar-server: looking up an environment for its host: {err}");
            return text(StatusCode::INTERNAL_SERVER_ERROR, "Internal error.");
        }
    };
    if !e.ports_public || numbered.is_some() {
        if !allowed(req.headers(), &host, secure) {
            return text(
                StatusCode::FORBIDDEN,
                "Cross-site request refused: this environment is private.",
            );
        }
        if req.uri().path() == SIGN_IN {
            let query = req.uri().query().unwrap_or("").to_string();
            return redeem_here(&app, &query, e.id, secure).await;
        }
        let cookie = crate::http::cookie(req.headers(), COOKIE).unwrap_or_default();
        match authenticate(&app.store, &cookie, e.id).await {
            Ok(Some(found)) => e = found,
            Ok(None) => {
                if req.method() == Method::GET && get(req.headers(), "sec-fetch-mode") != "cors" {
                    // A page the browser is opening: Hangar signs it in
                    // and sends it back.
                    let scheme = if secure { "https" } else { "http" };
                    let uri = req
                        .uri()
                        .path_and_query()
                        .map(|p| p.as_str())
                        .unwrap_or("/");
                    let to = format!("{scheme}://{host}{uri}");
                    let q = url::form_urlencoded::Serializer::new(String::new())
                        .append_pair("to", &to)
                        .finish();
                    return see_other(&format!("{}{}?{q}", public.origin(), sign_in_path(e.id)));
                }
                return text(
                    StatusCode::UNAUTHORIZED,
                    "This environment is private: open it in a browser signed in to Hangar.",
                );
            }
            Err(err) => {
                eprintln!(
                    "hangar-server: authenticating a request to an environment's host: {err}"
                );
                return text(StatusCode::INTERNAL_SERVER_ERROR, "Internal error.");
            }
        }
    }
    let Some(worker) = e.worker_id.filter(|_| e.phase == "running") else {
        return text(
            StatusCode::SERVICE_UNAVAILABLE,
            "The environment is not running.",
        );
    };
    let port = numbered.unwrap_or(if secure { 443 } else { 80 });
    proxy(app, req, e.id, worker, port, &host, secure).await
}

/// Exchanges a sign-in ticket for a host's cookie and goes on to the path
/// the ticket was issued for.
async fn redeem_here(app: &Shared, query: &str, env: Uuid, secure: bool) -> Response {
    let (mut ticket, mut to) = (String::new(), String::new());
    for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
        match k.as_ref() {
            "ticket" => ticket = v.into_owned(),
            "to" => to = v.into_owned(),
            _ => {}
        }
    }
    let cookie = match redeem(&app.store, &ticket, env).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            return text(
                StatusCode::UNAUTHORIZED,
                "This link has expired. Open it again.",
            );
        }
        Err(err) => {
            eprintln!("hangar-server: redeeming a sign-in ticket: {err}");
            return text(StatusCode::INTERNAL_SERVER_ERROR, "Internal error.");
        }
    };
    if !to.starts_with('/') || to.starts_with("//") || to.starts_with("/\\") {
        to = "/".into();
    }
    let mut r = see_other(&to);
    let mut c = format!(
        "{COOKIE}={cookie}; Path=/; Max-Age={}; HttpOnly; SameSite=Lax",
        COOKIE_LIFETIME.num_seconds()
    );
    if secure {
        c.push_str("; Secure");
    }
    if let Ok(v) = HeaderValue::from_str(&c) {
        r.headers_mut().insert(header::SET_COOKIE, v);
    }
    r
}

/// Why the environment's port could not be reached.
enum Unreached {
    NoTunnel,
    Refused,
    Failed(String),
}

/// Answers the guest's port forwarder gives (crates/agent/src/guestport.rs).
const CONNECTED: u8 = 0;
const REFUSED: u8 = 1;

/// Asks the environment's port forwarder for a connection to port.
async fn dial(app: &Shared, worker: Uuid, env: Uuid, port: u16) -> Result<tunnel::Conn, Unreached> {
    let env = env.to_string();
    let mut conn = app
        .tunnels
        .open(
            worker,
            &StreamHeader {
                kind: tunnel::KIND_PORTS,
                environment: &env,
            },
        )
        .await
        .map_err(|e| match e {
            OpenError::NoTunnel => Unreached::NoTunnel,
            OpenError::Failed(e) => Unreached::Failed(e),
        })?;
    let failed = |e: std::io::Error| {
        Unreached::Failed(format!("asking the environment for port {port}: {e}"))
    };
    conn.write_all(&port.to_be_bytes()).await.map_err(failed)?;
    let mut answer = [0u8; 1];
    conn.read_exact(&mut answer).await.map_err(failed)?;
    match answer[0] {
        CONNECTED => Ok(conn),
        REFUSED => Err(Unreached::Refused),
        _ => Err(Unreached::Failed(format!(
            "the environment could not connect to its port {port}"
        ))),
    }
}

/// Accepts whatever certificate an environment's port 443 shows: it is
/// the environment's own business, often self-signed, and the browser has
/// been shown Hangar's.
#[derive(Debug)]
struct AnyCertificate(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn tls() -> tokio_rustls::TlsConnector {
    static CONFIG: std::sync::OnceLock<Arc<rustls::ClientConfig>> = std::sync::OnceLock::new();
    let cfg = CONFIG.get_or_init(|| {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut cfg = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .expect("TLS versions")
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AnyCertificate(provider)))
            .with_no_client_auth();
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        Arc::new(cfg)
    });
    tokio_rustls::TlsConnector::from(cfg.clone())
}

/// The headers that belong to one connection, not the request.
const HOP: [&str; 8] = [
    "connection",
    "keep-alive",
    "proxy-connection",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
];

/// Removes the hop-by-hop headers, and any the Connection header names,
/// keeping an upgrade if asked to.
fn strip_hop(h: &mut HeaderMap, keep_upgrade: bool) {
    let named: Vec<String> = h
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    let upgrade = h.get(header::UPGRADE).cloned();
    for name in HOP.iter().copied().chain(named.iter().map(String::as_str)) {
        h.remove(name);
    }
    h.remove(header::UPGRADE);
    if keep_upgrade && let Some(u) = upgrade {
        h.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
        h.insert(header::UPGRADE, u);
    }
}

fn upgrading(h: &HeaderMap) -> bool {
    h.get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|v| {
            v.split(',')
                .any(|t| t.trim().eq_ignore_ascii_case("upgrade"))
        })
        && h.contains_key(header::UPGRADE)
}

/// The request's cookies, less the host's own: the guest has no business
/// holding it.
fn without_host_cookie(h: &mut HeaderMap) {
    let kept: Vec<String> = h
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .map(str::trim)
        .filter(|c| !c.is_empty() && c.split('=').next() != Some(COOKIE))
        .map(str::to_string)
        .collect();
    h.remove(header::COOKIE);
    if !kept.is_empty()
        && let Ok(v) = HeaderValue::from_str(&kept.join("; "))
    {
        h.insert(header::COOKIE, v);
    }
}

/// Sends a request on to the environment's port as it was made, and its
/// answer back.
async fn proxy(
    app: Shared,
    mut req: Request,
    env: Uuid,
    worker: Uuid,
    port: u16,
    host: &str,
    secure: bool,
) -> Response {
    let failed = |e: Unreached| match e {
        Unreached::Refused => text(
            StatusCode::BAD_GATEWAY,
            &format!("Nothing in the environment is listening on port {port}."),
        ),
        Unreached::NoTunnel => text(
            StatusCode::SERVICE_UNAVAILABLE,
            "The environment's worker is not connected to Hangar just now. Reload in a moment.",
        ),
        Unreached::Failed(why) => {
            eprintln!("hangar-server: reaching {host}'s port {port}: {why}");
            text(
                StatusCode::BAD_GATEWAY,
                &format!("The environment's port {port} could not be reached: {why}"),
            )
        }
    };
    let conn = match dial(&app, worker, env, port).await {
        Ok(c) => c,
        Err(e) => return failed(e),
    };
    let io = if port == 443 {
        let name = crate::publichost::hostname(host).to_string();
        let Ok(name) = rustls::pki_types::ServerName::try_from(name) else {
            return failed(Unreached::Failed(format!(
                "{host} is not a TLS server name"
            )));
        };
        match tls().connect(name, conn).await {
            Ok(t) => tokio_util::either::Either::Right(t),
            Err(e) => {
                return failed(Unreached::Failed(format!(
                    "TLS with the environment's port 443: {e}"
                )));
            }
        }
    } else {
        tokio_util::either::Either::Left(conn)
    };

    let upgrade = upgrading(req.headers());
    let downstream = upgrade.then(|| hyper::upgrade::on(&mut req));
    let ip = crate::http::client_ip_of(req.headers(), req.extensions());
    let (mut parts, body) = req.into_parts();
    strip_hop(&mut parts.headers, upgrade);
    without_host_cookie(&mut parts.headers);
    let set = |h: &mut HeaderMap, k: &'static str, v: &str| {
        if let Ok(v) = HeaderValue::from_str(v) {
            h.insert(HeaderName::from_static(k), v);
        }
    };
    set(&mut parts.headers, "x-forwarded-for", &ip);
    set(&mut parts.headers, "x-forwarded-host", host);
    set(
        &mut parts.headers,
        "x-forwarded-proto",
        if secure { "https" } else { "http" },
    );
    parts.uri = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/")
        .parse()
        .unwrap_or_default();
    let out = Request::from_parts(parts, body);

    let (mut sender, connection) =
        match hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(io)).await {
            Ok(x) => x,
            Err(e) => return failed(Unreached::Failed(e.to_string())),
        };
    tokio::spawn(async move {
        let _ = connection.with_upgrades().await;
    });
    let mut resp = match tokio::time::timeout(ANSWER_WITHIN, sender.send_request(out)).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => return failed(Unreached::Failed(e.to_string())),
        Err(_) => {
            return failed(Unreached::Failed(
                "it did not answer within 5 minutes".into(),
            ));
        }
    };
    let switching = resp.status() == StatusCode::SWITCHING_PROTOCOLS;
    if switching && let Some(downstream) = downstream {
        let upstream = hyper::upgrade::on(&mut resp);
        tokio::spawn(async move {
            let (Ok(down), Ok(up)) = (downstream.await, upstream.await) else {
                return;
            };
            let mut down = hyper_util::rt::TokioIo::new(down);
            let mut up = hyper_util::rt::TokioIo::new(up);
            let _ = tokio::io::copy_bidirectional(&mut down, &mut up).await;
        });
    }
    let (mut parts, body) = resp.into_parts();
    strip_hop(&mut parts.headers, switching);
    Response::from_parts(parts, Body::new(body))
}
