//! The client API, /api/v1 (api/client.yaml): what the apps
//! and tools ask of the server, versioned.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use hangar_openapi::clientapi::types;

use super::{ApiError, Body, List, Shared, Signed, Via, spec};
use crate::users;

/// The versions of this API the server serves.
const VERSIONS: &[&str] = &["v1"];

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/version", get(version))
        .route("/me", get(me))
        .route("/me/password", post(change_password))
        .route("/me/sign-in-link", post(create_sign_in_link))
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
        has_password: Some(u.password_hash.is_some()),
        id: u.id.to_string(),
        ssh: app.ssh_gateway().map(spec).transpose()?,
        username: u.username,
    }))
}

async fn list_tokens(
    State(app): State<Shared>,
    s: Signed,
) -> Result<List<types::AccessToken>, ApiError> {
    let current = match &s.via {
        Via::Token(t) => Some(t.clone()),
        Via::Session(_) => None,
    };
    let ts = users::tokens(&app.store, &s.principal).await?;
    Ok(List::all(
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

async fn change_password(
    State(app): State<Shared>,
    s: Signed,
    Body(b): Body<types::ChangePassword>,
) -> Result<StatusCode, ApiError> {
    let session = match &s.via {
        Via::Session(t) => Some(t.as_str()),
        Via::Token(_) => None,
    };
    users::change_password(
        &app.store,
        &s.principal,
        &s.ip,
        session,
        b.current_password.as_deref(),
        &b.new_password,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn create_sign_in_link(
    State(app): State<Shared>,
    s: Signed,
    Body(b): Body<types::NewSignInLink>,
) -> Result<(StatusCode, Json<types::SignInLink>), ApiError> {
    let to = b.to.unwrap_or_default();
    if !to.is_empty() && !super::frontend::local_path(&to) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "to is a path on this server, starting with one /",
        ));
    }
    let (token, expires_at) = users::sign_in_link(&app.store, &s.principal, &s.ip).await?;
    let mut path = format!("/api/frontend/auth/link/{token}");
    if !to.is_empty() {
        path.push('?');
        path.push_str(
            &url::form_urlencoded::Serializer::new(String::new())
                .append_pair("to", &to)
                .finish(),
        );
    }
    Ok((
        StatusCode::CREATED,
        Json(types::SignInLink { path, expires_at }),
    ))
}
