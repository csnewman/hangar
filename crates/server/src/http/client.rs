//! The client API, /api/v1 (api/client.yaml): what the apps
//! and tools ask of the server, versioned.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use hangar_openapi::clientapi::types;

use super::{ApiError, App, Body, List, Shared, Signed, Via, spec};
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
        .route("/me/links", get(list_links))
        .route("/me/links/{place}", put(put_link).delete(delete_link))
}

/// The link API's view of a link.
fn link_spec(l: crate::store::rows::Link) -> types::Link {
    types::Link {
        place: l.place_id.to_string(),
        name: l.name,
        url: l.url,
        public_key: l.public_key,
        created_at: l.created_at,
        reached_at: l.reached_at,
        reach_error: Some(l.reach_error).filter(|e| !e.is_empty()),
        heard_at: l.heard_at,
        synced_at: l.synced_at,
        sync_error: Some(l.sync_error).filter(|e| !e.is_empty()),
    }
}

fn this_place(app: &App) -> Result<&crate::link::Place, ApiError> {
    app.place.as_deref().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "this place has no link key; its log says why",
        )
    })
}

async fn list_links(State(app): State<Shared>, s: Signed) -> Result<Json<types::Links>, ApiError> {
    let place = this_place(&app)?;
    let mut tx = app.store.begin().await?;
    let person = tx.person(s.principal.user_id).await?;
    let links = tx.links(Some(s.principal.user_id)).await?;
    Ok(Json(types::Links {
        place: types::LinkPlace {
            id: place.id.to_string(),
            public_key: place.public_key(),
            url: app.cfg.public.as_ref().map(|p| p.origin()),
            levels: crate::link::LEVELS.iter().map(|&l| l.into()).collect(),
        },
        person: person.to_string(),
        allowed: app.cfg.personal_links,
        items: links.into_iter().map(link_spec).collect(),
        next: None,
    }))
}

async fn put_link(
    State(app): State<Shared>,
    s: Signed,
    Path(other): Path<String>,
    Body(b): Body<types::NewLink>,
) -> Result<Json<types::Link>, ApiError> {
    if !app.cfg.personal_links {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "this place's administrators do not allow personal links",
        )
        .code(super::code::LINKS_NOT_ALLOWED));
    }
    let place = this_place(&app)?;
    let invalid = |m: &str| ApiError::new(StatusCode::BAD_REQUEST, m);
    let other = uuid::Uuid::parse_str(&other).map_err(|_| invalid("place is a place's ID"))?;
    if other == place.id {
        return Err(invalid("that is this place"));
    }
    let person =
        uuid::Uuid::parse_str(&b.person).map_err(|_| invalid("person is a person's ID"))?;
    if !crate::link::is_public_key(&b.public_key) {
        return Err(invalid("public_key is an Ed25519 public key, base64"));
    }
    let url = b.url.trim().trim_end_matches('/').to_string();
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(invalid("url is the place's address, https:// or http://"));
    }
    let link = crate::store::rows::Link {
        user_id: s.principal.user_id,
        place_id: other,
        name: b.name.unwrap_or_default().trim().to_string(),
        public_key: b.public_key,
        url,
        created_at: chrono::Utc::now(),
        reached_at: None,
        reach_error: String::new(),
        heard_at: None,
        synced_at: None,
        sync_error: String::new(),
    };
    let kept = crate::link::put(&app.store, &s.principal, &s.ip, link, person)
        .await
        .map_err(|e| match e {
            crate::link::Unmade::Differs => ApiError::new(
                StatusCode::CONFLICT,
                "your account here is linked as another person; unlink it from its places first",
            )
            .code(super::code::PERSON_DIFFERS),
            crate::link::Unmade::Taken => ApiError::new(
                StatusCode::CONFLICT,
                "another account here is linked as that person",
            )
            .code(super::code::PERSON_TAKEN),
            crate::link::Unmade::Store(e) => e.into(),
        })?;
    app.links_wake.notify_one();
    Ok(Json(link_spec(kept)))
}

async fn delete_link(
    State(app): State<Shared>,
    s: Signed,
    Path(other): Path<String>,
) -> Result<StatusCode, ApiError> {
    let other = uuid::Uuid::parse_str(&other).map_err(|_| ApiError::not_found("link"))?;
    if crate::link::remove(&app.store, &s.principal, &s.ip, other).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("link"))
    }
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
