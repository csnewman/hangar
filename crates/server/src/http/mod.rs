//! What the server serves over HTTP: the web UI's API (/api/frontend), the
//! client API (/api/v1), and what is shared between them: who is signed in,
//! and how errors are told.

mod access;
mod audit;
mod catalog;
mod client;
mod environments;
mod frontend;
mod images;
mod keys;
mod link;
mod packs;
mod signin;
mod streams;
mod web;
mod worker;

use std::sync::Arc;

use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::{HeaderMap, StatusCode, header, request::Parts};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};

use crate::config::Config;
use crate::store::Store;
use crate::users::{self, Principal};

/// The session cookie.
pub const SESSION_COOKIE: &str = "hangar_session";

/// What every handler shares.
pub struct App {
    pub store: Store,
    pub cfg: Config,
    pub rules: access::Rules,
    pub tunnels: crate::tunnel::Tunnels,
    /// The SSH gateway's host key, in known_hosts form, when it runs.
    pub ssh_host_key: Option<String>,
    /// The files root, where copies keep their files.
    pub root: crate::root::Root,
    /// The registry's blob store, when it runs.
    pub blobs: Option<crate::blob::Blobs>,
    /// This place as linked places know it, and what wakes its hellos to
    /// them; none when its key could not be had.
    pub place: Option<Arc<crate::link::Place>>,
    pub links_wake: Arc<tokio::sync::Notify>,
    /// The connections to linked places this place holds.
    pub link_conns: Arc<crate::linkconn::Conns>,
    /// The linked copies changed here, and the people whose files are
    /// being linked.
    pub link_files: Arc<crate::linkfiles::Files>,
}

impl App {
    pub fn new(
        store: Store,
        cfg: Config,
        host_key: Option<&russh::keys::PrivateKey>,
        root: crate::root::Root,
        blobs: Option<crate::blob::Blobs>,
        place: Option<Arc<crate::link::Place>>,
    ) -> App {
        App {
            root,
            blobs,
            place,
            links_wake: Arc::default(),
            link_conns: Arc::default(),
            link_files: Arc::default(),
            store,
            cfg,
            rules: access::Rules::from_specs(),
            tunnels: crate::tunnel::Tunnels::default(),
            ssh_host_key: host_key.map(crate::keys::host_key_line),
        }
    }

    /// Where the SSH gateway is reached, for Me: host, port and host key.
    pub fn ssh_gateway(&self) -> Option<serde_json::Value> {
        let s = self.cfg.ssh.as_ref()?;
        Some(serde_json::json!({
            "host": s.host,
            "port": s.port,
            "host_key": self.ssh_host_key.clone().unwrap_or_default(),
        }))
    }
}

pub type Shared = Arc<App>;

/// Everything the server serves.
/// A list as the client API gives one: a single page, its items and no
/// next.
pub fn items<T: serde::Serialize>(list: Json<Vec<T>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "items": list.0 }))
}

pub fn router(app: Shared) -> Router {
    Router::new()
        .route("/api/healthz", get(healthz))
        .nest(
            "/api/frontend",
            frontend::routes()
                .merge(audit::frontend())
                .merge(catalog::frontend())
                .merge(environments::frontend())
                .merge(streams::frontend())
                .merge(keys::frontend())
                .merge(packs::frontend())
                .merge(images::frontend())
                .merge(signin::frontend()),
        )
        .nest(
            "/api/v1",
            client::routes()
                .merge(audit::client())
                .merge(catalog::client())
                .merge(environments::client())
                .merge(streams::client())
                .merge(keys::client())
                .merge(packs::client())
                .merge(images::client())
                .merge(frontend::admin_client())
                .merge(signin::client()),
        )
        .nest("/api/worker/v1", worker::routes())
        .nest("/api/link/v1", link::routes())
        .route_layer(axum::middleware::from_fn_with_state(
            app.clone(),
            access::enforce,
        ))
        .fallback(|uri: axum::http::Uri| async move {
            if uri.path().starts_with("/api/") {
                return ApiError::new(StatusCode::NOT_FOUND, "no such endpoint").into_response();
            }
            web::serve(uri).await
        })
        .layer(axum::middleware::from_fn(log_requests))
        .layer(axum::middleware::from_fn(name_ui))
        .layer(axum::middleware::from_fn_with_state(app.clone(), hosts))
        .with_state(app)
}

/// Names the UI's build in every answer of the web UI's API, refusals
/// included (`web`).
async fn name_ui(req: Request, next: axum::middleware::Next) -> Response {
    let api = req.uri().path().starts_with("/api/frontend/");
    let mut resp = next.run(req).await;
    if api {
        web::name_build(&mut resp);
    }
    resp
}

/// Whether the server can reach its store.
async fn healthz(axum::extract::State(app): axum::extract::State<Shared>) -> Response {
    let reached = async {
        let tx = app.store.read().await?;
        tx.commit().await
    };
    match tokio::time::timeout(std::time::Duration::from_secs(2), reached).await {
        Ok(Ok(())) => Json(serde_json::json!({ "status": "ok" })).into_response(),
        _ => ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "database unreachable").into_response(),
    }
}

/// Logs each API request but the long polls, health checks and reads that
/// arrive constantly and would drown everything else, unless it failed.
async fn log_requests(req: Request, next: axum::middleware::Next) -> Response {
    let path = req.uri().path().to_string();
    if !path.starts_with("/api/") {
        return next.run(req).await;
    }
    let method = req.method().clone();
    let start = std::time::Instant::now();
    let resp = next.run(req).await;
    let status = resp.status();
    let quiet = matches!(
        path.as_str(),
        "/api/worker/v1/desired" | "/api/healthz" | "/api/worker/v1/status"
    ) || method == axum::http::Method::GET;
    if status.as_u16() >= 400 || !quiet {
        eprintln!(
            "hangar-server: {method} {path} {} {}ms",
            status.as_u16(),
            start.elapsed().as_millis()
        );
    }
    resp
}

/// Sends a request for a host of its own, one of an environment's web
/// servers', there: everything on those hosts is the environment's.
async fn hosts(
    axum::extract::State(app): axum::extract::State<Shared>,
    req: Request,
    next: axum::middleware::Next,
) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if crate::registry::host(&app).is_some_and(|r| r.eq_ignore_ascii_case(&host)) {
        let path = req.uri().path().to_string();
        return crate::registry::serve(app.clone(), req, &path).await;
    }
    if app.cfg.registry
        && let Some(path) = req.uri().path().strip_prefix(crate::registry::WORKER_PATH)
    {
        let path = path.to_string();
        return crate::registry::serve(app.clone(), req, &path).await;
    }
    if let Some(public) = &app.cfg.public
        && crate::ports::owns(public, &host).is_some()
    {
        return crate::ports::serve(app.clone(), req).await;
    }
    next.run(req).await
}

/// An answer refusing a request: its status and the spec's Error body,
/// whose code a client can act on where its message is for people.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

/// The codes the specs' Error has beyond those its status gives
/// (`status_code`).
pub mod code {
    /// The environment is not running, and what was asked needs it to be.
    pub const NOT_RUNNING: &str = "environment_not_running";
    /// The environment's worker is not connected to this server.
    pub const UNREACHABLE: &str = "worker_unreachable";
    /// Its worker is, but the environment did not answer.
    pub const ENVIRONMENT_UNREACHABLE: &str = "environment_unreachable";
    /// The environment is on no worker yet.
    pub const NOT_PLACED: &str = "environment_not_placed";
    /// A sign-in waits to be approved (202, not a refusal).
    pub const SIGN_IN_PENDING: &str = "sign_in_pending";
    /// The worker speaks a worker API older than this server serves.
    pub const WORKER_TOO_OLD: &str = "worker_too_old";
    /// The worker speaks a worker API newer than this server serves.
    pub const SERVER_TOO_OLD: &str = "server_too_old";
    /// This place's administrators do not allow personal links.
    pub const LINKS_NOT_ALLOWED: &str = "links_not_allowed";
    /// The user is linked as another person already.
    pub const PERSON_DIFFERS: &str = "person_differs";
    /// Another user here is that person already.
    pub const PERSON_TAKEN: &str = "person_taken";
}

/// The code a refusal with this status has unless it is given its own.
fn status_code(status: StatusCode) -> &'static str {
    match status {
        StatusCode::BAD_REQUEST => "invalid",
        StatusCode::UNAUTHORIZED => "unauthorized",
        StatusCode::FORBIDDEN => "forbidden",
        StatusCode::NOT_FOUND => "not_found",
        StatusCode::CONFLICT => "conflict",
        StatusCode::PAYLOAD_TOO_LARGE => "too_large",
        StatusCode::SERVICE_UNAVAILABLE => "unavailable",
        s if s.is_client_error() => "refused",
        _ => "internal",
    }
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> ApiError {
        ApiError {
            status,
            code: status_code(status),
            message: message.into(),
        }
    }

    /// The same refusal under a code of its own (`code`).
    pub fn code(mut self, code: &'static str) -> ApiError {
        self.code = code;
        self
    }

    pub fn invalid(why: impl std::fmt::Display) -> ApiError {
        ApiError::new(StatusCode::BAD_REQUEST, format!("invalid: {why}"))
    }

    pub fn unauthorized() -> ApiError {
        ApiError::new(StatusCode::UNAUTHORIZED, "not signed in")
    }

    pub fn not_found(what: &str) -> ApiError {
        ApiError::new(StatusCode::NOT_FOUND, format!("{what} not found"))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message, "code": self.code })),
        )
            .into_response()
    }
}

impl From<crate::store::Error> for ApiError {
    fn from(e: crate::store::Error) -> ApiError {
        eprintln!("hangar-server: {e}");
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
    }
}

impl From<crate::fail::Fail> for ApiError {
    fn from(f: crate::fail::Fail) -> ApiError {
        use crate::fail::Fail;
        match f {
            Fail::Invalid(m) => ApiError::new(StatusCode::BAD_REQUEST, m),
            Fail::Conflict(m) => ApiError::new(StatusCode::CONFLICT, m),
            Fail::NotFound(m) => ApiError::new(StatusCode::NOT_FOUND, m),
            Fail::Forbidden(m) => ApiError::new(StatusCode::FORBIDDEN, m),
            Fail::Store(e) => e.into(),
        }
    }
}

/// A list as the client API answers it (api/client.yaml's Lists): a page
/// of items, and the path and query of the page after it, if there is one.
pub struct List<T> {
    pub items: Vec<T>,
    pub next: Option<String>,
}

impl<T> List<T> {
    /// A whole list, in one page.
    pub fn all(items: Vec<T>) -> List<T> {
        List { items, next: None }
    }
}

impl<T: serde::Serialize> IntoResponse for List<T> {
    fn into_response(self) -> Response {
        let mut o = serde_json::json!({ "items": self.items });
        if let Some(n) = self.next {
            o["next"] = n.into();
        }
        Json(o).into_response()
    }
}

/// A JSON body, refused as invalid (400) when it is not what the spec says.
pub struct Body<T>(pub T);

impl<S: Send + Sync, T: serde::de::DeserializeOwned> FromRequest<S> for Body<T> {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(v)) => Ok(Body(v)),
            Err(e) => Err(ApiError::invalid(e.body_text())),
        }
    }
}

/// A JSON body that may be absent: None for an empty one.
pub struct OptionalBody<T>(pub Option<T>);

impl<S: Send + Sync, T: serde::de::DeserializeOwned> FromRequest<S> for OptionalBody<T> {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
        let bytes = axum::body::Bytes::from_request(req, state)
            .await
            .map_err(|e| ApiError::invalid(e.body_text()))?;
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(OptionalBody(None));
        }
        serde_json::from_slice(&bytes)
            .map(|v| OptionalBody(Some(v)))
            .map_err(ApiError::invalid)
    }
}

/// How a request signed in.
#[derive(Debug, Clone)]
pub enum Via {
    /// A browser's session, by its cookie's token.
    Session(String),
    /// An access token.
    Token(String),
}

/// Who a request is signed in as, refused (401) when no one.
#[derive(Clone)]
pub struct Signed {
    pub principal: Principal,
    pub via: Via,
    pub ip: String,
    /// A session opened for this request, by the development stack's
    /// automatic sign-in, whose cookie the response sets.
    pub issued: Option<String>,
}

impl FromRequestParts<Shared> for Signed {
    type Rejection = ApiError;

    /// Who access::enforce found signed in, or for a route outside the
    /// specs, who the request signs in as.
    async fn from_request_parts(parts: &mut Parts, app: &Shared) -> Result<Self, ApiError> {
        if let Some(s) = parts.extensions.get::<Signed>() {
            return Ok(s.clone());
        }
        Signed::authenticate(parts, app).await
    }
}

impl Signed {
    /// Who a request's bearer token or session cookie signs in.
    pub async fn authenticate(parts: &mut Parts, app: &Shared) -> Result<Signed, ApiError> {
        let ip = client_ip(parts);
        if let Some(token) = bearer(&parts.headers) {
            return match users::token(&app.store, &token).await? {
                Some(principal) => Ok(Signed {
                    principal,
                    via: Via::Token(token),
                    ip,
                    issued: None,
                }),
                None => Err(ApiError::unauthorized()),
            };
        }
        if let Some(token) = cookie(&parts.headers, SESSION_COOKIE)
            && let Some(principal) = users::session(&app.store, &token).await?
        {
            return Ok(Signed {
                principal,
                via: Via::Session(token),
                ip,
                issued: None,
            });
        }
        let web_ui = parts
            .extensions
            .get::<axum::extract::MatchedPath>()
            .is_some_and(|m| m.as_str().starts_with("/api/frontend/"));
        match &app.cfg.auto_sign_in {
            Some(name) if web_ui => match users::sign_in_as(&app.store, &ip, name).await? {
                Some((token, principal)) => Ok(Signed {
                    principal,
                    via: Via::Session(token.clone()),
                    ip,
                    issued: Some(token),
                }),
                None => {
                    eprintln!(
                        "hangar-server: signing in automatically as {name}: there is no such enabled person"
                    );
                    Err(ApiError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "automatic sign-in failed",
                    ))
                }
            },
            _ => Err(ApiError::unauthorized()),
        }
    }

    /// Sets the cookie of a session opened for the request on its response.
    pub fn set_issued(&self, app: &App, resp: &mut Response) {
        if let Some(token) = &self.issued
            && let Ok(v) = header::HeaderValue::from_str(&session_cookie(app, Some(token)))
        {
            resp.headers_mut().append(header::SET_COOKIE, v);
        }
    }
}

/// The address a request came from: the first X-Forwarded-For, which the
/// proxy in front sets, else the connection's.
pub fn client_ip(parts: &Parts) -> String {
    client_ip_of(&parts.headers, &parts.extensions)
}

/// client_ip, of a request's headers and extensions.
pub fn client_ip_of(headers: &HeaderMap, extensions: &axum::http::Extensions) -> String {
    if let Some(first) = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
    {
        return first.trim().to_string();
    }
    extensions
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0.ip().to_string())
        .unwrap_or_default()
}

/// The address a request came from, as client_ip has it.
pub struct ClientIp(pub String);

impl<S: Send + Sync> FromRequestParts<S> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(ClientIp(client_ip(parts)))
    }
}

pub fn bearer(headers: &HeaderMap) -> Option<String> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = v.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| token.trim().to_string())
}

/// The value of the cookie named name.
pub fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|kv| {
            let (k, v) = kv.trim().split_once('=')?;
            (k == name).then(|| v.to_string())
        })
}

/// A Set-Cookie for the session: the token, or its end when None.
pub fn session_cookie(app: &App, token: Option<&str>) -> String {
    let secure = app.cfg.public.as_ref().is_some_and(|p| p.secure());
    let mut c = match token {
        Some(t) => format!(
            "{SESSION_COOKIE}={t}; Path=/; Max-Age={}; HttpOnly; SameSite=Lax",
            users::SESSION_LIFETIME.num_seconds()
        ),
        None => format!("{SESSION_COOKIE}=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax"),
    };
    if secure {
        c.push_str("; Secure");
    }
    c
}

/// A value of the spec's from JSON: for the spec's enums, from the string a
/// row keeps.
pub fn spec<T: serde::de::DeserializeOwned>(v: serde_json::Value) -> Result<T, ApiError> {
    serde_json::from_value(v.clone()).map_err(|e| {
        eprintln!("hangar-server: {v} is not what the spec allows: {e}");
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
    })
}
