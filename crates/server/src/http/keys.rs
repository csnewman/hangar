//! A person's SSH keys and sign-in keys: the web UI's profile page, and
//! the client API's account.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{ApiError, Body, List, Shared, Signed};
use crate::keys::{self, View};

pub fn frontend() -> Router<Shared> {
    Router::new()
        .route("/me/profile", get(profile))
        .route("/me/profile/keys", post(add_ssh_key))
        .route("/me/profile/keys/{id}", delete(delete_ssh_key))
        .route("/me/profile/login-keys", post(add_login_key_full))
        .route("/me/profile/login-keys/{id}", delete(delete_login_key))
}

pub fn client() -> Router<Shared> {
    Router::new()
        .route("/me/ssh-keys", get(list_ssh_keys).post(add_ssh_key))
        .route("/me/ssh-keys/{id}", delete(delete_ssh_key))
        .route("/me/login-keys", get(list_login_keys).post(add_login_key))
        .route("/me/login-keys/{id}", delete(delete_login_key))
}

fn key_json(k: &View) -> Value {
    json!({
        "id": k.id.to_string(),
        "name": k.name,
        "public_key": k.public_key,
        "fingerprint": k.fingerprint,
        "created_at": k.created_at,
    })
}

/// A sign-in key as the client API shows it: without its public key.
fn login_json(k: &View) -> Value {
    json!({
        "id": k.id.to_string(),
        "name": k.name,
        "fingerprint": k.fingerprint,
        "created_at": k.created_at,
    })
}

fn id(s: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(s).map_err(|_| ApiError::new(StatusCode::NOT_FOUND, "no such key"))
}

fn text<'a>(b: &'a Value, k: &str) -> &'a str {
    b.get(k).and_then(Value::as_str).unwrap_or("")
}

async fn profile(State(app): State<Shared>, s: Signed) -> Result<Json<Value>, ApiError> {
    let keys = keys::ssh_keys(&app.store, &s.principal).await?;
    let login = keys::login_keys(&app.store, &s.principal).await?;
    Ok(Json(json!({
        "keys": keys.iter().map(key_json).collect::<Vec<_>>(),
        "login_keys": login.iter().map(key_json).collect::<Vec<_>>(),
        "secrets": app.cfg.sealer.is_some(),
    })))
}

async fn list_ssh_keys(State(app): State<Shared>, s: Signed) -> Result<List<Value>, ApiError> {
    let keys = keys::ssh_keys(&app.store, &s.principal).await?;
    Ok(List::all(keys.iter().map(key_json).collect()))
}

/// Keeps an SSH key: the private key given, or a new one.
async fn add_ssh_key(
    State(app): State<Shared>,
    s: Signed,
    Body(b): Body<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let private = b
        .get("private_key")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty());
    let k = keys::add_ssh_key(
        &app.store,
        app.cfg.sealer.as_deref(),
        &s.principal,
        &s.ip,
        text(&b, "name"),
        private,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(key_json(&k))))
}

async fn delete_ssh_key(
    State(app): State<Shared>,
    s: Signed,
    Path(k): Path<String>,
) -> Result<StatusCode, ApiError> {
    keys::delete_ssh_key(&app.store, &s.principal, &s.ip, id(&k)?).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_login_keys(State(app): State<Shared>, s: Signed) -> Result<List<Value>, ApiError> {
    let keys = keys::login_keys(&app.store, &s.principal).await?;
    Ok(List::all(keys.iter().map(login_json).collect()))
}

async fn add_login_key(
    State(app): State<Shared>,
    s: Signed,
    Body(b): Body<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let k = keys::add_login_key(
        &app.store,
        &s.principal,
        &s.ip,
        text(&b, "name"),
        text(&b, "public_key"),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(login_json(&k))))
}

async fn add_login_key_full(
    State(app): State<Shared>,
    s: Signed,
    Body(b): Body<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let k = keys::add_login_key(
        &app.store,
        &s.principal,
        &s.ip,
        text(&b, "name"),
        text(&b, "public_key"),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(key_json(&k))))
}

async fn delete_login_key(
    State(app): State<Shared>,
    s: Signed,
    Path(k): Path<String>,
) -> Result<StatusCode, ApiError> {
    keys::delete_login_key(&app.store, &s.principal, &s.ip, id(&k)?).await?;
    Ok(StatusCode::NO_CONTENT)
}
