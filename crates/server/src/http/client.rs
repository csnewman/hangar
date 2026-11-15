//! The client API, /api/v1 (api/client.yaml): what the apps
//! and tools ask of the server, versioned.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get};
use axum::{Json, Router};
use hangar_openapi::clientapi::types;

use super::{ApiError, Shared, Signed, Via, spec};
use crate::users;

/// The versions of this API the server serves.
const VERSIONS: &[&str] = &["v1"];

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/version", get(version))
        .route("/me", get(me))
        .route("/me/tokens", get(list_tokens))
        .route("/me/tokens/{id}", delete(revoke_token))
}

async fn version() -> Json<types::Version> {
    Json(types::Version {
        server: format!(
            "hangar-server {}",
            option_env!("HANGAR_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
        ),
        versions: VERSIONS.iter().map(|v| v.to_string()).collect(),
    })
}

async fn me(State(app): State<Shared>, s: Signed) -> Result<Json<types::Me>, ApiError> {
    let u = users::me(&app.store, &s.principal)
        .await?
        .ok_or_else(ApiError::unauthorized)?;
    Ok(Json(types::Me {
        admin: u.is_admin,
        display_name: u.display_name,
        environment_host_suffix: app.cfg.public.as_ref().map(|p| p.name("")),
        id: u.id.to_string(),
        ssh: app.ssh_gateway().map(spec).transpose()?,
        username: u.username,
    }))
}

async fn list_tokens(
    State(app): State<Shared>,
    s: Signed,
) -> Result<Json<Vec<types::AccessToken>>, ApiError> {
    let current = match &s.via {
        Via::Token(t) => Some(t.clone()),
        Via::Session(_) => None,
    };
    let ts = users::tokens(&app.store, &s.principal).await?;
    Ok(Json(
        ts.into_iter()
            .map(|t| types::AccessToken {
                current: current.as_deref().is_some_and(|c| users::is_token(c, &t)),
                created_at: t.created_at,
                expires_at: t.expires_at,
                id: t.id.to_string(),
                last_used_at: t.last_used_at,
                name: t.name,
            })
            .collect(),
    ))
}

async fn revoke_token(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = uuid::Uuid::parse_str(&id).map_err(|_| ApiError::not_found("token"))?;
    if users::revoke_token(&app.store, &s.principal, &s.ip, id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("token"))
    }
}
