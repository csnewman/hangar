//! The registry's repositories in the web UI's API.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, put};
use axum::{Json, Router};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{ApiError, Body, Shared, Signed};
use crate::registry::{self, RepositoryView};

pub fn frontend() -> Router<Shared> {
    Router::new()
        .route("/images", get(list))
        .route("/images/{id}", get(get_one).patch(update).delete(remove))
        .route("/images/{id}/collaborators", put(set_collaborators))
        .route("/images/{id}/tags/{tag}", delete(delete_tag))
}

pub fn client() -> Router<Shared> {
    Router::new()
        .route("/images", get(client_list))
        .route("/images/{id}", get(get_one).patch(update).delete(remove))
        .route("/images/{id}/collaborators", put(set_collaborators))
        .route("/images/{id}/tags/{tag}", delete(delete_tag))
}

async fn client_list(st: State<Shared>, s: Signed) -> Result<Json<Value>, ApiError> {
    Ok(super::items(list(st, s).await?))
}

fn id(s: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(s).map_err(|_| ApiError::new(StatusCode::NOT_FOUND, "repository not found"))
}

/// The registry's host, refusing when the server runs none.
fn host(app: &Shared) -> Result<String, ApiError> {
    registry::host(app)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "this server runs no registry"))
}

fn repo_json(host: &str, x: &RepositoryView) -> Value {
    let person = |p: &crate::templates::Person| json!({ "id": p.id.to_string(), "username": p.username, "display_name": p.display_name });
    let team = |t: &crate::templates::TeamRef| json!({ "id": t.id.to_string(), "slug": t.slug, "name": t.name });
    let mut out = json!({
        "id": x.repo.id.to_string(),
        "path": x.repo.path,
        "name": format!("{host}/{}", x.repo.path),
        "description": x.repo.description,
        "visibility": x.repo.visibility,
        "collaborators": x.collaborators.iter().map(person).collect::<Vec<_>>(),
        "collaborator_teams": x.collaborator_teams.iter().map(team).collect::<Vec<_>>(),
        "tag_count": x.tag_count,
        "can_push": x.can_push,
        "can_manage": x.can_manage,
        "created_at": x.repo.created_at,
        "updated_at": x.repo.updated_at,
    });
    if let Some(o) = &x.owner {
        out["owner"] = person(o);
    }
    if let Some(t) = &x.team {
        out["team"] = team(t);
    }
    if let Some(tags) = &x.tags {
        out["tags"] = json!(
            tags.iter()
                .map(|t| json!({
                    "name": t.name,
                    "digest": t.digest,
                    "media_type": t.media_type,
                    "size_bytes": t.size,
                    "updated_at": t.updated_at,
                }))
                .collect::<Vec<_>>()
        );
    }
    out
}

async fn list(State(app): State<Shared>, s: Signed) -> Result<Json<Vec<Value>>, ApiError> {
    let host = host(&app)?;
    let xs = registry::repositories(&app.store, &s.principal, None).await?;
    Ok(Json(xs.iter().map(|x| repo_json(&host, x)).collect()))
}

async fn get_one(
    State(app): State<Shared>,
    s: Signed,
    Path(r): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let host = host(&app)?;
    let x = registry::repositories(&app.store, &s.principal, Some(id(&r)?))
        .await?
        .pop()
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "repository not found"))?;
    Ok(Json(repo_json(&host, &x)))
}

async fn update(
    State(app): State<Shared>,
    s: Signed,
    Path(r): Path<String>,
    Body(b): Body<Value>,
) -> Result<Json<Value>, ApiError> {
    let host = host(&app)?;
    let visibility = b.get("visibility").and_then(Value::as_str).unwrap_or("");
    let description = b.get("description").and_then(Value::as_str).unwrap_or("");
    let x = registry::update(
        &app.store,
        &s.principal,
        &s.ip,
        id(&r)?,
        visibility,
        description,
    )
    .await?;
    Ok(Json(repo_json(&host, &x)))
}

async fn remove(
    State(app): State<Shared>,
    s: Signed,
    Path(r): Path<String>,
) -> Result<StatusCode, ApiError> {
    host(&app)?;
    registry::delete(&app.store, &s.principal, &s.ip, id(&r)?).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn set_collaborators(
    State(app): State<Shared>,
    s: Signed,
    Path(r): Path<String>,
    Body(b): Body<Value>,
) -> Result<Json<Value>, ApiError> {
    let host = host(&app)?;
    let ids = |k: &str| -> Vec<String> {
        b.get(k)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let x = registry::set_collaborators(
        &app.store,
        &s.principal,
        &s.ip,
        id(&r)?,
        &ids("user_ids"),
        &ids("team_ids"),
    )
    .await?;
    Ok(Json(repo_json(&host, &x)))
}

async fn delete_tag(
    State(app): State<Shared>,
    s: Signed,
    Path((r, tag)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let host = host(&app)?;
    let x = registry::delete_tag(&app.store, &s.principal, &s.ip, id(&r)?, &tag).await?;
    Ok(Json(repo_json(&host, &x)))
}
