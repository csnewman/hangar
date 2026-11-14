//! The web UI's API, /api/frontend (internal/frontendapi/openapi.yaml):
//! what the browser asks of the server.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use hangar_openapi::frontendapi::types;

use super::{ApiError, App, Body, Shared, Signed, Via, client_ip, session_cookie, spec};
use crate::store::rows::{AccessToken, User};
use crate::users;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/me", get(me))
        .route("/me/tokens", get(list_tokens).post(create_token))
        .route("/me/tokens/{id}", delete(revoke_token))
}

/// The signed-in user, as the web UI shows them.
pub fn me_of(app: &App, u: &User) -> Result<types::Me, ApiError> {
    Ok(types::Me {
        admin: u.is_admin,
        display_name: u.display_name.clone(),
        environment_host_suffix: app.cfg.public.as_ref().map(|p| format!(".{}", p.host)),
        has_password: u.password_hash.is_some(),
        id: u.id.to_string(),
        registry: app
            .cfg
            .public
            .as_ref()
            .filter(|_| app.cfg.registry)
            .map(|p| p.name("registry")),
        ssh: None,
        theme: spec(serde_json::json!(u.theme))?,
        username: u.username.clone(),
    })
}

fn token_of(t: &AccessToken) -> types::AccessToken {
    types::AccessToken {
        created_at: t.created_at,
        expires_at: t.expires_at,
        id: t.id.to_string(),
        last_used_at: t.last_used_at,
        name: t.name.clone(),
    }
}

async fn login(
    State(app): State<Shared>,
    headers: HeaderMap,
    Body(b): Body<types::Login>,
) -> Result<Response, ApiError> {
    let ip = client_ip(&headers);
    let Some((token, user)) = users::login(&app.store, &ip, &b.username, &b.password).await? else {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "wrong username or password",
        ));
    };
    let me = me_of(&app, &user)?;
    Ok((
        [(header::SET_COOKIE, session_cookie(&app, Some(&token)))],
        Json(me),
    )
        .into_response())
}

async fn logout(State(app): State<Shared>, s: Signed) -> Result<Response, ApiError> {
    if let Via::Session(token) = &s.via {
        users::logout(&app.store, &s.principal, &s.ip, token).await?;
    }
    Ok((
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, session_cookie(&app, None))],
    )
        .into_response())
}

async fn me(State(app): State<Shared>, s: Signed) -> Result<Json<types::Me>, ApiError> {
    let u = users::me(&app.store, &s.principal)
        .await?
        .ok_or_else(ApiError::unauthorized)?;
    Ok(Json(me_of(&app, &u)?))
}

async fn list_tokens(
    State(app): State<Shared>,
    s: Signed,
) -> Result<Json<Vec<types::AccessToken>>, ApiError> {
    let ts = users::tokens(&app.store, &s.principal).await?;
    Ok(Json(ts.iter().map(token_of).collect()))
}

async fn create_token(
    State(app): State<Shared>,
    s: Signed,
    Body(b): Body<types::CreateToken>,
) -> Result<(StatusCode, Json<types::NewAccessToken>), ApiError> {
    let name = b.name.trim();
    if name.is_empty() || name.len() > 100 {
        return Err(ApiError::invalid(
            "a token needs a name of up to 100 characters",
        ));
    }
    let days = b.expires_in_days.map(|d| d.get());
    let (token, t) = users::create_token(&app.store, &s.principal, &s.ip, name, days).await?;
    Ok((
        StatusCode::CREATED,
        Json(types::NewAccessToken {
            info: token_of(&t),
            token,
        }),
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
