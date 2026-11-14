//! What the server serves over HTTP: the web UI's API (/api/frontend), the
//! client API (/api/v1), and what is shared between them: who is signed in,
//! and how errors are told.

mod access;
mod catalog;
mod client;
mod environments;
mod frontend;
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

/// The session cookie, the Go server's.
pub const SESSION_COOKIE: &str = "hangar_session";

/// What every handler shares.
pub struct App {
    pub store: Store,
    pub cfg: Config,
    pub rules: access::Rules,
    pub tunnels: crate::tunnel::Tunnels,
}

impl App {
    pub fn new(store: Store, cfg: Config) -> App {
        App {
            store,
            cfg,
            rules: access::Rules::from_specs(),
            tunnels: crate::tunnel::Tunnels::default(),
        }
    }
}

pub type Shared = Arc<App>;

/// Everything the server serves.
pub fn router(app: Shared) -> Router {
    Router::new()
        .route("/api/healthz", get(|| async { "ok" }))
        .nest(
            "/api/frontend",
            frontend::routes()
                .merge(catalog::frontend())
                .merge(environments::frontend()),
        )
        .nest(
            "/api/v1",
            client::routes()
                .merge(catalog::client())
                .merge(environments::client()),
        )
        .nest("/api/worker/v1", worker::routes())
        .route_layer(axum::middleware::from_fn_with_state(
            app.clone(),
            access::enforce,
        ))
        .fallback(|| async { ApiError::new(StatusCode::NOT_FOUND, "not found") })
        .with_state(app)
}

/// An answer refusing a request: its status and the spec's Error body.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> ApiError {
        ApiError {
            status,
            message: message.into(),
        }
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
            Json(serde_json::json!({ "error": self.message })),
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
        let ip = client_ip(&parts.headers);
        if let Some(token) = bearer(&parts.headers) {
            return match users::token(&app.store, &token).await? {
                Some(principal) => Ok(Signed {
                    principal,
                    via: Via::Token(token),
                    ip,
                }),
                None => Err(ApiError::unauthorized()),
            };
        }
        let Some(token) = cookie(&parts.headers, SESSION_COOKIE) else {
            return Err(ApiError::unauthorized());
        };
        match users::session(&app.store, &token).await? {
            Some(principal) => Ok(Signed {
                principal,
                via: Via::Session(token),
                ip,
            }),
            None => Err(ApiError::unauthorized()),
        }
    }
}

/// The address a request came from, as the request says it: the first
/// X-Forwarded-For, which the proxy in front sets.
pub fn client_ip(headers: &HeaderMap) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|v| v.trim().to_string())
        .unwrap_or_default()
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
