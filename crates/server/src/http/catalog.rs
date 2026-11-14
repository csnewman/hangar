//! Templates, teams and the people directory, in the web UI's API and the
//! client API.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, put};
use axum::{Json, Router};
use hangar_api::Spec;
use hangar_openapi::{clientapi, frontendapi::types};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{ApiError, Body, Shared, Signed, spec as to_spec};
use crate::{teams, templates};

pub fn frontend() -> Router<Shared> {
    Router::new()
        .route("/templates", get(list_templates).post(create_template))
        .route(
            "/templates/{id}",
            get(get_template)
                .put(update_template)
                .delete(delete_template),
        )
        .route("/templates/{id}/collaborators", put(set_collaborators))
        .route("/people", get(people))
        .route("/teams", get(list_teams).post(create_team))
        .route(
            "/teams/{id}",
            get(get_team).patch(update_team).delete(delete_team),
        )
        .route(
            "/teams/{id}/members/{user}",
            put(set_member).delete(remove_member),
        )
}

pub fn client() -> Router<Shared> {
    Router::new().route("/templates", get(client_templates))
}

fn id(s: &str, what: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(s).map_err(|_| ApiError::not_found(what))
}

/// A spec as the web UI's API shows it: its own fields and no others, the
/// empty ones left out.
pub fn spec_json(s: &Spec) -> Value {
    let mut out = json!({
        "image": s.image,
        "cpus": s.cpus,
        "memory_mib": s.memory_mib,
        "display": s.display,
        "gpu": s.gpu,
        "repos": s.repos.iter().map(repo_json).collect::<Vec<_>>(),
        "placement": s.placement,
    });
    let o = out.as_object_mut().expect("an object");
    let projects = s.project_folders();
    if !projects.is_empty() {
        o.insert("projects".into(), json!(projects));
    }
    if s.access != hangar_api::Access::default() {
        let a = &s.access;
        let mut access = serde_json::Map::new();
        for (k, v) in [
            ("no_self_attached", a.no_self_attached),
            ("no_sensitive_files", a.no_sensitive_files),
            ("no_ssh_keys", a.no_ssh_keys),
            ("no_registry", a.no_registry),
            ("no_editor_trust", a.no_editor_trust),
        ] {
            if v {
                access.insert(k.into(), json!(true));
            }
        }
        o.insert("access".into(), access.into());
    }
    if s.dax {
        o.insert("dax".into(), json!(true));
    }
    if let Some(m) = s.swap_mib {
        o.insert("swap_mib".into(), json!(m));
    }
    if s.memory_reclaim {
        o.insert("memory_reclaim".into(), json!(true));
    }
    if !s.trusted_folders.is_empty() {
        o.insert("trusted_folders".into(), json!(s.trusted_folders));
    }
    if !s.web_names.is_empty() {
        o.insert("web_names".into(), json!(s.web_names));
    }
    if !s.packs.is_empty() {
        let packs: Vec<Value> = s
            .packs
            .iter()
            .map(|p| {
                let mut r = json!({ "pack": p.pack });
                if !p.copy.is_empty() {
                    r["copy"] = json!(p.copy);
                }
                r
            })
            .collect();
        o.insert("packs".into(), json!(packs));
    }
    out
}

fn repo_json(r: &hangar_api::Repo) -> Value {
    let mut out = json!({ "url": r.url, "path": r.path });
    if !r.r#ref.is_empty() {
        out["ref"] = json!(r.r#ref);
    }
    if !r.branch.is_empty() {
        out["branch"] = json!(r.branch);
    }
    out
}

fn person_json(p: &templates::Person) -> Value {
    json!({ "id": p.id.to_string(), "username": p.username, "display_name": p.display_name })
}

fn team_ref_json(t: &templates::TeamRef) -> Value {
    json!({ "id": t.id.to_string(), "slug": t.slug, "name": t.name })
}

fn template_of(v: &templates::View) -> Result<types::Template, ApiError> {
    let mut out = json!({
        "id": v.row.id.to_string(),
        "name": v.row.name,
        "description": v.row.description,
        "visibility": v.row.visibility,
        "owner": person_json(&v.owner),
        "collaborators": v.collaborators.iter().map(person_json).collect::<Vec<_>>(),
        "collaborator_teams": v.collaborator_teams.iter().map(team_ref_json).collect::<Vec<_>>(),
        "spec": spec_json(&v.spec.spec),
        "environments": v.environments,
        "can_edit": v.can_edit,
        "can_manage": v.can_manage,
        "created_at": v.row.created_at,
        "updated_at": v.row.updated_at,
    });
    if let Some(t) = &v.team {
        out["team"] = team_ref_json(t);
    }
    if !v.spec.name_pattern.is_empty() {
        out["name_pattern"] = json!(v.spec.name_pattern);
    }
    if !v.spec.name_hint.is_empty() {
        out["name_hint"] = json!(v.spec.name_hint);
    }
    to_spec(out)
}

/// What the web UI's template body asks for.
fn input(b: types::TemplateInput) -> Result<templates::Input, ApiError> {
    let spec: Spec = serde_json::to_value(&b.spec)
        .and_then(serde_json::from_value)
        .map_err(ApiError::invalid)?;
    Ok(templates::Input {
        name: b.name.to_string(),
        description: b.description.map(|d| d.to_string()).unwrap_or_default(),
        visibility: serde_json::to_value(b.visibility)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default(),
        spec: hangar_api::TemplateSpec {
            spec,
            name_pattern: b.name_pattern.map(|p| p.to_string()).unwrap_or_default(),
            name_hint: b.name_hint.map(|h| h.to_string()).unwrap_or_default(),
        },
        team: b.team_id,
    })
}

async fn list_templates(
    State(app): State<Shared>,
    s: Signed,
) -> Result<Json<Vec<types::Template>>, ApiError> {
    let ts = templates::list(&app.store, &s.principal).await?;
    Ok(Json(ts.iter().map(template_of).collect::<Result<_, _>>()?))
}

async fn get_template(
    State(app): State<Shared>,
    s: Signed,
    Path(t): Path<String>,
) -> Result<Json<types::Template>, ApiError> {
    let v = templates::get(&app.store, &s.principal, id(&t, "template")?)
        .await?
        .ok_or_else(|| ApiError::not_found("template"))?;
    Ok(Json(template_of(&v)?))
}

async fn create_template(
    State(app): State<Shared>,
    s: Signed,
    Body(b): Body<types::TemplateInput>,
) -> Result<(StatusCode, Json<types::Template>), ApiError> {
    let v = templates::create(&app.store, &s.principal, &s.ip, input(b)?).await?;
    Ok((StatusCode::CREATED, Json(template_of(&v)?)))
}

async fn update_template(
    State(app): State<Shared>,
    s: Signed,
    Path(t): Path<String>,
    Body(b): Body<types::TemplateInput>,
) -> Result<Json<types::Template>, ApiError> {
    let v = templates::update(
        &app.store,
        &s.principal,
        &s.ip,
        id(&t, "template")?,
        input(b)?,
    )
    .await?;
    Ok(Json(template_of(&v)?))
}

async fn delete_template(
    State(app): State<Shared>,
    s: Signed,
    Path(t): Path<String>,
) -> Result<StatusCode, ApiError> {
    templates::delete(&app.store, &s.principal, &s.ip, id(&t, "template")?).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn set_collaborators(
    State(app): State<Shared>,
    s: Signed,
    Path(t): Path<String>,
    Body(b): Body<types::Collaborators>,
) -> Result<Json<types::Template>, ApiError> {
    let v = templates::set_collaborators(
        &app.store,
        &s.principal,
        &s.ip,
        id(&t, "template")?,
        &b.user_ids,
        &b.team_ids,
    )
    .await?;
    Ok(Json(template_of(&v)?))
}

async fn people(State(app): State<Shared>) -> Result<Json<Vec<types::Person>>, ApiError> {
    let people = app.store.begin().await?.people().await?;
    Ok(Json(
        people
            .into_iter()
            .filter(|u| u.disabled_at.is_none())
            .map(|u| types::Person {
                id: u.id.to_string(),
                username: u.username,
                display_name: u.display_name,
            })
            .collect(),
    ))
}

fn team_of(v: &teams::View, members: bool) -> Result<types::Team, ApiError> {
    let mut out = json!({
        "id": v.row.id.to_string(),
        "slug": v.row.slug,
        "name": v.row.name,
        "description": v.row.description,
        "member_count": v.members.len(),
        "templates": v.templates,
        "can_manage": v.can_manage,
        "created_at": v.row.created_at,
    });
    if let Some(r) = &v.role {
        out["role"] = json!(r);
    }
    if members {
        out["members"] = json!(
            v.members
                .iter()
                .map(|(p, role)| json!({ "person": person_json(p), "role": role }))
                .collect::<Vec<_>>()
        );
    }
    to_spec(out)
}

async fn list_teams(
    State(app): State<Shared>,
    s: Signed,
) -> Result<Json<Vec<types::Team>>, ApiError> {
    let ts = teams::list(&app.store, &s.principal).await?;
    Ok(Json(
        ts.iter()
            .map(|t| team_of(t, false))
            .collect::<Result<_, _>>()?,
    ))
}

async fn get_team(
    State(app): State<Shared>,
    s: Signed,
    Path(t): Path<String>,
) -> Result<Json<types::Team>, ApiError> {
    let v = teams::get(&app.store, &s.principal, id(&t, "team")?)
        .await?
        .ok_or_else(|| ApiError::not_found("team"))?;
    Ok(Json(team_of(&v, true)?))
}

async fn create_team(
    State(app): State<Shared>,
    s: Signed,
    Body(b): Body<types::CreateTeam>,
) -> Result<(StatusCode, Json<types::Team>), ApiError> {
    let v = teams::create(
        &app.store,
        &s.principal,
        &s.ip,
        teams::Input {
            slug: b.slug.to_string(),
            name: b.name.to_string(),
            description: b.description.map(|d| d.to_string()).unwrap_or_default(),
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(team_of(&v, true)?)))
}

async fn update_team(
    State(app): State<Shared>,
    s: Signed,
    Path(t): Path<String>,
    Body(b): Body<types::TeamInput>,
) -> Result<Json<types::Team>, ApiError> {
    let v = teams::update(
        &app.store,
        &s.principal,
        &s.ip,
        id(&t, "team")?,
        teams::Input {
            slug: String::new(),
            name: b.name.to_string(),
            description: b.description.map(|d| d.to_string()).unwrap_or_default(),
        },
    )
    .await?;
    Ok(Json(team_of(&v, true)?))
}

async fn delete_team(
    State(app): State<Shared>,
    s: Signed,
    Path(t): Path<String>,
) -> Result<StatusCode, ApiError> {
    teams::delete(&app.store, &s.principal, &s.ip, id(&t, "team")?).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn set_member(
    State(app): State<Shared>,
    s: Signed,
    Path((t, user)): Path<(String, String)>,
    Body(b): Body<types::TeamMembership>,
) -> Result<Json<types::Team>, ApiError> {
    let role = serde_json::to_value(b.role)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    let v = teams::set_member(
        &app.store,
        &s.principal,
        &s.ip,
        id(&t, "team")?,
        &user,
        &role,
    )
    .await?;
    Ok(Json(team_of(&v, true)?))
}

async fn remove_member(
    State(app): State<Shared>,
    s: Signed,
    Path((t, user)): Path<(String, String)>,
) -> Result<Json<types::Team>, ApiError> {
    let v = teams::remove_member(&app.store, &s.principal, &s.ip, id(&t, "team")?, &user).await?;
    Ok(Json(team_of(&v, true)?))
}

async fn client_templates(
    State(app): State<Shared>,
    s: Signed,
) -> Result<Json<Vec<clientapi::types::Template>>, ApiError> {
    let ts = templates::list(&app.store, &s.principal).await?;
    let out = ts
        .iter()
        .map(|v| {
            let sp = &v.spec.spec;
            let mut t = json!({
                "id": v.row.id.to_string(),
                "name": v.row.name,
                "description": v.row.description,
                "owner": v.owner.username,
                "cpus": sp.cpus,
                "memory_mib": sp.memory_mib,
                "display": sp.display,
                "gpu": sp.gpu,
                "image": sp.image,
                "repos": sp.repos.iter().map(repo_json).collect::<Vec<_>>(),
            });
            if let Some(m) = sp.swap_mib {
                t["swap_mib"] = json!(m);
            }
            if !v.spec.name_pattern.is_empty() {
                t["name_pattern"] = json!(v.spec.name_pattern);
            }
            if !v.spec.name_hint.is_empty() {
                t["name_hint"] = json!(v.spec.name_hint);
            }
            if let Some(team) = &v.team {
                t["team"] = json!(team.name);
            }
            if sp.dax {
                t["dax"] = json!(true);
            }
            if sp.memory_reclaim {
                t["memory_reclaim"] = json!(true);
            }
            if !sp.projects.is_empty() {
                t["projects"] = json!(sp.projects);
            }
            to_spec(t)
        })
        .collect::<Result<_, _>>()?;
    Ok(Json(out))
}
