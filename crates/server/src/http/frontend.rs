//! The web UI's API, /api/frontend (api/frontend.yaml):
//! what the browser asks of the server.

use axum::extract::{Path, RawQuery, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use hangar_openapi::frontendapi::types;

use super::{ApiError, App, Body, ClientIp, Shared, Signed, Via, session_cookie, spec};
use crate::store::rows::{AccessToken, User};
use crate::users;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/auth/login", post(login))
        .route("/auth/link/{token}", get(follow_link))
        .route("/auth/logout", post(logout))
        .route("/me", get(me))
        .route("/me/tokens", get(list_tokens).post(create_token))
        .route("/me/tokens/{id}", delete(revoke_token))
        .route("/me/password", post(change_password))
        .route("/users", get(list_users).post(create_user))
        .route(
            "/users/{id}",
            axum::routing::patch(update_user).delete(delete_user),
        )
        .route("/workers", get(list_workers))
        .route("/workers/{id}", get(get_worker).delete(delete_worker))
        .route("/workers/{id}/revoke", post(revoke_worker))
        .route("/workers/{id}/images/remove", post(remove_worker_image))
        .route("/workers/{id}/unknown/remove", post(remove_unknown))
        .route("/me/preferences", axum::routing::put(set_preferences))
}

/// The administrators' users and workers, in the client API as here.
pub fn admin_client() -> Router<Shared> {
    Router::new()
        .route("/users", get(client_users).post(create_user))
        .route(
            "/users/{id}",
            axum::routing::patch(update_user).delete(delete_user),
        )
        .route("/workers", get(client_workers))
        .route("/workers/{id}", get(get_worker).delete(delete_worker))
        .route("/workers/{id}/revoke", post(revoke_worker))
        .route("/workers/{id}/images/remove", post(remove_worker_image))
        .route("/workers/{id}/unknown/remove", post(remove_unknown))
}

async fn client_users(st: State<Shared>) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(super::items(list_users(st).await?))
}

async fn client_workers(st: State<Shared>) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(super::items(list_workers(st).await?))
}

async fn remove_worker_image(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
    Body(b): Body<types::ImageCopy>,
) -> Result<StatusCode, ApiError> {
    let c = hangar_api::ImageCopy {
        r#ref: b.ref_.to_string(),
        digest: b.digest,
    };
    crate::workers::remove_image(&app.store, &s.principal, &s.ip, worker_id(&id)?, &c).await?;
    Ok(StatusCode::ACCEPTED)
}

async fn remove_unknown(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
    Body(b): Body<types::RemoveUnknownEnvironments>,
) -> Result<StatusCode, ApiError> {
    crate::workers::remove_unknown(
        &app.store,
        &s.principal,
        &s.ip,
        worker_id(&id)?,
        &b.environments,
    )
    .await?;
    Ok(StatusCode::ACCEPTED)
}

async fn set_preferences(
    State(app): State<Shared>,
    s: Signed,
    Body(b): Body<types::Preferences>,
) -> Result<StatusCode, ApiError> {
    if !users::set_theme(&app.store, &s.principal, &b.theme.to_string()).await? {
        return Err(ApiError::unauthorized());
    }
    Ok(StatusCode::NO_CONTENT)
}

/// A worker as the web UI shows it.
fn worker_of(l: &crate::store::workers::Listed) -> Result<types::Worker, ApiError> {
    let w = &l.worker;
    let monitor = (!w.monitor.is_empty()).then(|| w.monitor.clone());
    spec(serde_json::json!({
        "id": w.id.to_string(),
        "name": w.name,
        "labels": w.labels,
        "capacity": { "cpus": w.cpus, "memory_mib": w.memory_mib },
        "allocated": { "cpus": l.allocated_cpus, "memory_mib": l.allocated_memory_mib },
        "unknown": w.unknown,
        "last_seen_at": w.last_seen_at,
        "revoked": w.revoked_at.is_some(),
        "online": crate::workers::online(w),
        "created_at": w.created_at,
        "stats": w.stats,
        "images": w.images,
        "gpu": w.gpu,
        "monitor": monitor,
        "version": (!w.version.is_empty()).then_some(&w.version),
        "features": w.features,
        "pending_removals": l.removals.iter()
            .map(|r| serde_json::json!({ "ref": r.r#ref, "digest": r.digest }))
            .collect::<Vec<_>>(),
    }))
}

fn worker_id(id: &str) -> Result<uuid::Uuid, ApiError> {
    uuid::Uuid::parse_str(id).map_err(|_| ApiError::not_found("worker"))
}

async fn list_workers(State(app): State<Shared>) -> Result<Json<Vec<types::Worker>>, ApiError> {
    let ws = crate::workers::list(&app.store).await?;
    Ok(Json(ws.iter().map(worker_of).collect::<Result<_, _>>()?))
}

async fn get_worker(
    State(app): State<Shared>,
    Path(id): Path<String>,
) -> Result<Json<types::Worker>, ApiError> {
    let w = crate::workers::get(&app.store, worker_id(&id)?)
        .await?
        .ok_or_else(|| ApiError::not_found("worker"))?;
    Ok(Json(worker_of(&w)?))
}

async fn revoke_worker(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    crate::workers::revoke(&app.store, &s.principal, &s.ip, worker_id(&id)?).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_worker(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    crate::workers::delete(&app.store, &s.principal, &s.ip, worker_id(&id)?).await?;
    Ok(StatusCode::NO_CONTENT)
}

fn user_of(l: &users::Listed) -> types::User {
    let u = &l.user;
    types::User {
        admin: u.is_admin,
        created_at: u.created_at,
        disabled: u.disabled_at.is_some(),
        display_name: u.display_name.clone(),
        environments: l.environments,
        has_password: u.password_hash.is_some(),
        id: u.id.to_string(),
        username: u.username.clone(),
    }
}

/// A user's ID from a path: one that is not a UUID names no one.
fn user_id(id: &str) -> Result<uuid::Uuid, ApiError> {
    uuid::Uuid::parse_str(id).map_err(|_| ApiError::not_found("user"))
}

async fn list_users(State(app): State<Shared>) -> Result<Json<Vec<types::User>>, ApiError> {
    Ok(Json(
        users::list(&app.store).await?.iter().map(user_of).collect(),
    ))
}

async fn create_user(
    State(app): State<Shared>,
    s: Signed,
    Body(b): Body<types::CreateUser>,
) -> Result<(StatusCode, Json<types::User>), ApiError> {
    let display = b
        .display_name
        .as_ref()
        .map(|d| d.to_string())
        .unwrap_or_default();
    let u = users::create(
        &app.store,
        &s.principal,
        &s.ip,
        users::NewUser {
            username: &b.username,
            display_name: &display,
            password: &b.password,
            admin: b.admin.unwrap_or(false),
        },
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(user_of(&users::Listed {
            user: u,
            environments: 0,
        })),
    ))
}

async fn update_user(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
    Body(b): Body<types::UpdateUser>,
) -> Result<Json<types::User>, ApiError> {
    let id = user_id(&id)?;
    let display = b.display_name.as_ref().map(|d| d.to_string());
    let password = b.password.as_ref().map(|p| p.to_string());
    let u = users::update(
        &app.store,
        &s.principal,
        &s.ip,
        id,
        users::Update {
            display_name: display.as_deref(),
            admin: b.admin,
            disabled: b.disabled,
            password: password.as_deref(),
        },
    )
    .await?;
    let environments = app.store.read().await?.owned_environments(u.id).await?;
    Ok(Json(user_of(&users::Listed {
        user: u,
        environments,
    })))
}

async fn delete_user(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = user_id(&id)?;
    // Found before their rows go with the person; the files are on the
    // files root, where nothing else would delete them.
    let copies = crate::packs::copies_of(&app.store, id).await;
    users::delete(&app.store, &s.principal, &s.ip, id).await?;
    if let Err(e) = copies
        .map_err(crate::fail::Fail::from)
        .and_then(|c| crate::packs::remove_copies(&app.root, &c))
    {
        eprintln!("hangar-server: deleting a deleted user's files: {e:?}");
    }
    Ok(StatusCode::NO_CONTENT)
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

/// The signed-in user, as the web UI shows them.
pub fn me_of(app: &App, u: &User) -> Result<types::Me, ApiError> {
    Ok(types::Me {
        admin: u.is_admin,
        display_name: u.display_name.clone(),
        environment_host_suffix: app.cfg.public.as_ref().map(|p| p.name("")),
        has_password: u.password_hash.is_some(),
        id: u.id.to_string(),
        registry: app
            .cfg
            .public
            .as_ref()
            .filter(|_| app.cfg.registry)
            .map(|p| p.name("registry")),
        ssh: app.ssh_gateway().map(spec).transpose()?,
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
    ClientIp(ip): ClientIp,
    Body(b): Body<types::Login>,
) -> Result<Response, ApiError> {
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

/// Whether to is a path on this server: one / and then anything but a
/// second, which a browser takes for another host.
pub fn local_path(to: &str) -> bool {
    to.starts_with('/') && !to.starts_with("//") && !to.starts_with("/\\")
}

async fn follow_link(
    State(app): State<Shared>,
    ClientIp(ip): ClientIp,
    Path(token): Path<String>,
    RawQuery(q): RawQuery,
) -> Result<Response, ApiError> {
    let to = url::form_urlencoded::parse(q.unwrap_or_default().as_bytes())
        .find(|(k, _)| k == "to")
        .map(|(_, v)| v.into_owned())
        .filter(|t| local_path(t))
        .unwrap_or_else(|| "/".into());
    let see = |loc: &str| {
        let mut r = StatusCode::SEE_OTHER.into_response();
        if let Ok(v) = header::HeaderValue::from_str(loc) {
            r.headers_mut().insert(header::LOCATION, v);
        }
        r
    };
    Ok(match users::follow_link(&app.store, &ip, &token).await? {
        Some(session) => {
            let mut r = see(&to);
            if let Ok(v) = header::HeaderValue::from_str(&session_cookie(&app, Some(&session))) {
                r.headers_mut().append(header::SET_COOKIE, v);
            }
            r
        }
        None => see(&format!(
            "/login?{}",
            url::form_urlencoded::Serializer::new(String::new())
                .append_pair("next", &to)
                .finish()
        )),
    })
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
