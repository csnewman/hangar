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
