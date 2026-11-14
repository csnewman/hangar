//! Who may do each operation, as the specs say: `security: []` makes one
//! public, `x-hangar-admin: true` an administrator's, and every other needs
//! someone signed in. The Go server enforces the same rules from the same
//! specs (internal/frontendapi auth.go), and a route the specs do not have
//! is refused, so the server serves the specs and nothing else under them.

use std::collections::HashMap;

use axum::extract::{MatchedPath, Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use super::{ApiError, Shared, Signed};

const FRONTEND: &str = include_str!("../../../../internal/frontendapi/openapi.yaml");
const CLIENT: &str = include_str!("../../../../internal/clientapi/openapi.yaml");

/// What an operation needs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Access {
    Public,
    SignedIn,
    Admin,
}

/// Each operation's access, by method and path as the router names it.
pub struct Rules(HashMap<(Method, String), Access>);

impl Rules {
    /// The rules the two specs set: the web UI's paths as they are, the
    /// client API's under /api/v1.
    pub fn from_specs() -> Rules {
        let mut r = HashMap::new();
        for (spec, prefix) in [(FRONTEND, ""), (CLIENT, "/api/v1")] {
            let doc: serde_json::Value = serde_saphyr::from_str(spec).expect("the specs parse");
            let everyone = doc["security"].as_array().is_some_and(|s| s.is_empty());
            for (path, item) in doc["paths"].as_object().expect("the specs have paths") {
                for (method, op) in item.as_object().into_iter().flatten() {
                    let Ok(method) = method.to_uppercase().parse::<Method>() else {
                        continue;
                    };
                    let public = match op.get("security") {
                        Some(s) => s.as_array().is_some_and(|s| s.is_empty()),
                        None => everyone,
                    };
                    let access = if public {
                        Access::Public
                    } else if op["x-hangar-admin"] == serde_json::json!(true) {
                        Access::Admin
                    } else {
                        Access::SignedIn
                    };
                    r.insert((method, format!("{prefix}{path}")), access);
                }
            }
        }
        // The WebSockets, which the specs describe in words: each needs
        // someone signed in who may reach the environment.
        for prefix in ["/api/frontend", "/api/v1"] {
            for socket in ["terminal", "code", "desktop"] {
                r.insert(
                    (
                        Method::GET,
                        format!("{prefix}/environments/{{id}}/{socket}"),
                    ),
                    Access::SignedIn,
                );
            }
        }
        Rules(r)
    }

    pub fn get(&self, method: &Method, path: &str) -> Option<Access> {
        self.0.get(&(method.clone(), path.to_string())).copied()
    }
}

/// Enforces the rule of the operation a request was routed to, and gives
/// the handler who is signed in.
pub async fn enforce(
    State(app): State<Shared>,
    matched: Option<MatchedPath>,
    mut req: Request,
    next: Next,
) -> Response {
    let Some(matched) = matched else {
        return next.run(req).await;
    };
    let path = matched.as_str();
    // A browser sends the session's cookie with requests other sites
    // cause, so a change it carries must come from this server's pages:
    // every change to the web UI's API, and the client API's by cookie.
    let by_cookie = path.starts_with("/api/v1/")
        && super::bearer(req.headers()).is_none()
        && super::cookie(req.headers(), super::SESSION_COOKIE).is_some();
    if (path.starts_with("/api/frontend/") || by_cookie)
        && let Some(why) = cross_site(&req)
    {
        return ApiError::new(StatusCode::FORBIDDEN, why).into_response();
    }
    let Some(access) = app.rules.get(req.method(), path) else {
        if path.starts_with("/api/frontend/") || path.starts_with("/api/v1/") {
            eprintln!("hangar-server: no access rule for {} {path}", req.method());
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
                .into_response();
        }
        return next.run(req).await;
    };
    if access != Access::Public {
        let (mut parts, body) = req.into_parts();
        let signed = match Signed::authenticate(&mut parts, &app).await {
            Ok(s) => s,
            Err(e) => return e.into_response(),
        };
        if access == Access::Admin && !signed.principal.admin {
            return ApiError::new(StatusCode::FORBIDDEN, "forbidden").into_response();
        }
        parts.extensions.insert(signed);
        req = Request::from_parts(parts, body);
    }
    next.run(req).await
}

/// Why a request that changes something is refused as another site's: the
/// browser says so, or its Origin is not this server. The session's cookie
/// is SameSite=Lax, which keeps it off such requests already; this is the
/// second lock, which does not depend on the browser honouring that.
fn cross_site(req: &Request) -> Option<&'static str> {
    if matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        return None;
    }
    let h = req.headers();
    let header = |name: &str| h.get(name).and_then(|v| v.to_str().ok());
    if let Some(site) = header("sec-fetch-site")
        && !site.is_empty()
        && site != "same-origin"
        && site != "none"
    {
        return Some("cross-site request refused");
    }
    if let Some(origin) = header("origin")
        && !origin.is_empty()
    {
        let host = origin.split_once("://").map(|(_, rest)| rest);
        if host.is_none() || host != header("host") {
            return Some("cross-origin request refused");
        }
    }
    None
}
