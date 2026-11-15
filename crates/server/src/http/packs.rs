//! Packs, their copies and files in the web UI's API; what packs an
//! environment has, and its conflicts, in both APIs; and what of the files
//! root a worker serves an environment.

use axum::extract::{Path, RawQuery, State};
use axum::http::StatusCode;
use axum::routing::{get, put};
use axum::{Json, Router};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{ApiError, Body, List, Shared, Signed};
use crate::packs::{self, CopyView, File, PackInput, PackView};

pub fn frontend() -> Router<Shared> {
    Router::new()
        .route("/packs", get(list_packs).post(create_pack))
        .route(
            "/packs/{id}",
            get(get_pack).put(update_pack).delete(delete_pack),
        )
        .route("/packs/{id}/copies", get(list_copies).post(create_copy))
        .route("/packs/{id}/default", put(set_default))
        .route(
            "/copies/{id}",
            get(get_copy).patch(rename_copy).delete(delete_copy),
        )
        .route(
            "/copies/{id}/file",
            get(get_file)
                .put(put_file)
                .patch(chmod_file)
                .delete(delete_file),
        )
        .route("/environments/{id}/packs", get(environment_packs))
        .route("/environments/{id}/packs/{pack}", put(choose_copy))
        .route(
            "/environments/{id}/conflicts",
            get(list_conflicts).post(resolve_conflict),
        )
}

pub fn client() -> Router<Shared> {
    Router::new()
        .route("/environments/{id}/packs", get(client_environment_packs))
        .route("/environments/{id}/packs/{pack}", put(choose_copy))
}

fn id(s: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(s).map_err(|_| ApiError::new(StatusCode::NOT_FOUND, "not found"))
}

fn text<'a>(b: &'a Value, k: &str) -> &'a str {
    b.get(k).and_then(Value::as_str).unwrap_or("")
}

fn path_json(p: &packs::Path) -> Value {
    json!({ "path": p.path, "sensitive": p.sensitive })
}

fn pack_json(k: &PackView, default: Option<Uuid>) -> Value {
    let r = &k.row;
    let mut out = json!({
        "id": r.id.to_string(),
        "name": r.name,
        "description": r.description,
        "builtin": r.builtin,
        "attach": r.attach,
        "squash": r.squash,
        "paths": k.paths.iter().map(path_json).collect::<Vec<_>>(),
        "can_change": k.can_change,
        "can_delete": k.can_delete,
        "created_at": r.created_at,
        "updated_at": r.updated_at,
    });
    if let Some(o) = r.owner_id {
        out["owner_id"] = json!(o.to_string());
    }
    if let Some(o) = &k.owner {
        out["owner"] = json!(o);
    }
    if let Some(t) = r.team_id {
        out["team_id"] = json!(t.to_string());
    }
    if let Some(t) = &k.team {
        out["team"] = json!(t);
    }
    if let Some(d) = &k.defaults {
        out["default_paths"] = json!(d.iter().map(path_json).collect::<Vec<_>>());
    }
    if let Some(c) = default {
        out["default_copy_id"] = json!(c.to_string());
    }
    out
}

fn copy_json(c: &CopyView) -> Value {
    let r = &c.row;
    let mut out = json!({
        "id": r.id.to_string(),
        "pack_id": r.pack_id.to_string(),
        "personal": r.personal,
        "name": r.name,
        "can_write": c.can_write,
        "can_delete": c.can_delete,
        "created_at": r.created_at,
    });
    if let Some(o) = r.owner_id {
        out["owner_id"] = json!(o.to_string());
    }
    if let Some(o) = &c.owner {
        out["owner"] = json!(o);
    }
    if let Some(t) = r.team_id {
        out["team_id"] = json!(t.to_string());
    }
    if let Some(t) = &c.team {
        out["team"] = json!(t);
    }
    out
}

fn file_json(f: &File) -> Value {
    json!({
        "path": f.path,
        "size": f.size,
        "mode": f.mode,
        "sensitive": f.sensitive,
        "shared": f.shared,
        "updated_at": f.updated_at,
    })
}

fn input(b: &Value) -> Result<PackInput, ApiError> {
    let paths = b
        .get("paths")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::invalid("a pack needs paths"))?
        .iter()
        .map(|p| packs::Path {
            path: text(p, "path").to_string(),
            sensitive: p.get("sensitive").and_then(Value::as_bool).unwrap_or(false),
        })
        .collect();
    Ok(PackInput {
        name: text(b, "name").to_string(),
        description: text(b, "description").to_string(),
        team: b.get("team_id").and_then(Value::as_str).map(str::to_string),
        attach: text(b, "attach").to_string(),
        squash: text(b, "squash").to_string(),
        paths,
    })
}

/// The file a query's path names.
fn file_path(q: Option<String>) -> Result<String, ApiError> {
    url::form_urlencoded::parse(q.unwrap_or_default().as_bytes())
        .find(|(k, _)| k == "path")
        .map(|(_, v)| v.into_owned())
        .ok_or_else(|| ApiError::invalid("a file's path is needed"))
}

async fn list_packs(State(app): State<Shared>, s: Signed) -> Result<Json<Vec<Value>>, ApiError> {
    let ks = packs::packs(&app.store, &s.principal).await?;
    let d = packs::defaults(&app.store, &s.principal).await?;
    Ok(Json(
        ks.iter()
            .map(|k| pack_json(k, d.get(&k.row.id).copied()))
            .collect(),
    ))
}

async fn get_pack(
    State(app): State<Shared>,
    s: Signed,
    Path(k): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let k = packs::pack(&app.store, &s.principal, id(&k)?)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not found"))?;
    let d = packs::defaults(&app.store, &s.principal).await?;
    Ok(Json(pack_json(&k, d.get(&k.row.id).copied())))
}

async fn create_pack(
    State(app): State<Shared>,
    s: Signed,
    Body(b): Body<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let k = packs::create_pack(&app.store, &s.principal, &s.ip, input(&b)?).await?;
    Ok((StatusCode::CREATED, Json(pack_json(&k, None))))
}

async fn update_pack(
    State(app): State<Shared>,
    s: Signed,
    Path(k): Path<String>,
    Body(b): Body<Value>,
) -> Result<Json<Value>, ApiError> {
    let k = packs::update_pack(&app.store, &s.principal, &s.ip, id(&k)?, input(&b)?).await?;
    Ok(Json(pack_json(&k, None)))
}

async fn delete_pack(
    State(app): State<Shared>,
    s: Signed,
    Path(k): Path<String>,
) -> Result<StatusCode, ApiError> {
    packs::delete_pack(&app.store, &app.root, &s.principal, &s.ip, id(&k)?).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_copies(
    State(app): State<Shared>,
    s: Signed,
    Path(k): Path<String>,
) -> Result<Json<Vec<Value>>, ApiError> {
    let cs = packs::copies(&app.store, &s.principal, id(&k)?).await?;
    Ok(Json(cs.iter().map(copy_json).collect()))
}

async fn create_copy(
    State(app): State<Shared>,
    s: Signed,
    Path(k): Path<String>,
    Body(b): Body<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let team = b.get("team_id").and_then(Value::as_str);
    let c = packs::create_copy(
        &app.store,
        &s.principal,
        &s.ip,
        id(&k)?,
        text(&b, "name"),
        team,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(copy_json(&c))))
}

async fn set_default(
    State(app): State<Shared>,
    s: Signed,
    Path(k): Path<String>,
    Body(b): Body<Value>,
) -> Result<StatusCode, ApiError> {
    packs::set_default(
        &app.store,
        &s.principal,
        id(&k)?,
        b.get("copy_id").and_then(Value::as_str),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_copy(
    State(app): State<Shared>,
    s: Signed,
    Path(c): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let not_found = || ApiError::new(StatusCode::NOT_FOUND, "not found");
    let c = packs::copy(&app.store, &s.principal, id(&c)?)
        .await?
        .ok_or_else(not_found)?;
    let k = packs::pack(&app.store, &s.principal, c.row.pack_id)
        .await?
        .ok_or_else(not_found)?;
    let files = packs::files(&app.store, &app.root, &s.principal, c.row.id).await?;
    Ok(Json(json!({
        "copy": copy_json(&c),
        "pack": pack_json(&k, None),
        "files": files.iter().map(file_json).collect::<Vec<_>>(),
    })))
}

async fn rename_copy(
    State(app): State<Shared>,
    s: Signed,
    Path(c): Path<String>,
    Body(b): Body<Value>,
) -> Result<Json<Value>, ApiError> {
    let c = packs::rename_copy(&app.store, &s.principal, id(&c)?, text(&b, "name")).await?;
    Ok(Json(copy_json(&c)))
}

async fn delete_copy(
    State(app): State<Shared>,
    s: Signed,
    Path(c): Path<String>,
) -> Result<StatusCode, ApiError> {
    packs::delete_copy(&app.store, &app.root, &s.principal, &s.ip, id(&c)?).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_file(
    State(app): State<Shared>,
    s: Signed,
    Path(c): Path<String>,
    RawQuery(q): RawQuery,
) -> Result<Json<Value>, ApiError> {
    let key = file_path(q)?;
    let f = packs::file(&app.store, &app.root, &s.principal, id(&c)?, &key)
        .await
        .map_err(|e| match e {
            crate::fail::Fail::NotFound(_) => {
                ApiError::new(StatusCode::NOT_FOUND, "no such file in the copy")
            }
            e => e.into(),
        })?;
    let data = f.data.unwrap_or_default();
    Ok(Json(json!({
        "path": f.path,
        "content": String::from_utf8_lossy(&data),
        "mode": f.mode,
        "sensitive": f.sensitive,
        "shared": f.shared,
        "updated_at": f.updated_at,
    })))
}

async fn put_file(
    State(app): State<Shared>,
    s: Signed,
    Path(c): Path<String>,
    RawQuery(q): RawQuery,
    Body(b): Body<Value>,
) -> Result<Json<Value>, ApiError> {
    let key = file_path(q)?;
    let mode = b.get("mode").and_then(Value::as_u64).unwrap_or(0) as u32;
    let content = b
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::invalid("a file needs content"))?;
    let f = packs::put(
        &app.store,
        &app.root,
        &s.principal,
        &s.ip,
        id(&c)?,
        &key,
        content.as_bytes(),
        mode,
    )
    .await?;
    Ok(Json(file_json(&f)))
}

async fn chmod_file(
    State(app): State<Shared>,
    s: Signed,
    Path(c): Path<String>,
    RawQuery(q): RawQuery,
    Body(b): Body<Value>,
) -> Result<Json<Value>, ApiError> {
    let key = file_path(q)?;
    let mode = b
        .get("mode")
        .and_then(Value::as_u64)
        .ok_or_else(|| ApiError::invalid("a file needs a mode"))? as u32;
    let f = packs::chmod(
        &app.store,
        &app.root,
        &s.principal,
        &s.ip,
        id(&c)?,
        &key,
        mode,
    )
    .await?;
    Ok(Json(file_json(&f)))
}

async fn delete_file(
    State(app): State<Shared>,
    s: Signed,
    Path(c): Path<String>,
    RawQuery(q): RawQuery,
) -> Result<StatusCode, ApiError> {
    let key = file_path(q)?;
    packs::delete_file(&app.store, &app.root, &s.principal, &s.ip, id(&c)?, &key).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// An environment the asker may reach.
async fn visible(app: &Shared, s: &Signed, env: &str) -> Result<Uuid, ApiError> {
    let no = || ApiError::new(StatusCode::NOT_FOUND, "no such environment");
    let env = Uuid::parse_str(env).map_err(|_| no())?;
    crate::environments::get(&app.store, &s.principal, env)
        .await?
        .map(|v| v.row.id)
        .ok_or_else(no)
}

async fn environment_packs(
    State(app): State<Shared>,
    s: Signed,
    Path(env): Path<String>,
) -> Result<Json<Vec<Value>>, ApiError> {
    let env = visible(&app, &s, &env).await?;
    let attached = packs::environment_packs(&app.store, env).await?;
    Ok(Json(
        attached
            .iter()
            .map(|a| {
                json!({
                    "pack": pack_json(&a.pack, None),
                    "copy": copy_json(&a.copy),
                    "from": a.from.as_str(),
                    "listed": a.listed,
                })
            })
            .collect(),
    ))
}

/// A copy as the client API lists it.
fn pack_copy_json(c: &CopyView) -> Value {
    let mut out =
        json!({ "id": c.row.id.to_string(), "name": c.row.name, "personal": c.row.personal });
    if let Some(o) = &c.owner {
        out["owner"] = json!(o);
    }
    if let Some(t) = &c.team {
        out["team"] = json!(t);
    }
    out
}

/// An environment's packs, each with the copy it uses and those the asker
/// could choose instead.
async fn client_environment_packs(
    State(app): State<Shared>,
    s: Signed,
    Path(env): Path<String>,
) -> Result<List<Value>, ApiError> {
    let env = visible(&app, &s, &env).await?;
    let attached = packs::environment_packs(&app.store, env).await?;
    let mut out = Vec::with_capacity(attached.len());
    for a in attached {
        let copies = match packs::copies(&app.store, &s.principal, a.pack.row.id).await {
            Ok(cs) => cs,
            Err(crate::fail::Fail::NotFound(_)) => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        let mut x = json!({
            "id": a.pack.row.id.to_string(),
            "name": a.pack.row.name,
            "description": a.pack.row.description,
            "builtin": a.pack.row.builtin,
            "copy": pack_copy_json(&a.copy),
            "from": a.from.as_str(),
            "listed": a.listed,
            "copies": copies.iter().map(pack_copy_json).collect::<Vec<_>>(),
        });
        if let Some(o) = &a.pack.owner {
            x["owner"] = json!(o);
        }
        if let Some(t) = &a.pack.team {
            x["team"] = json!(t);
        }
        out.push(x);
    }
    Ok(List::all(out))
}

async fn choose_copy(
    State(app): State<Shared>,
    s: Signed,
    Path((env, pack)): Path<(String, String)>,
    super::OptionalBody(b): super::OptionalBody<Value>,
) -> Result<StatusCode, ApiError> {
    let env = visible(&app, &s, &env).await?;
    let pack = id(&pack)?;
    let copy = b
        .as_ref()
        .and_then(|b| b.get("copy_id"))
        .and_then(Value::as_str);
    packs::choose_copy(&app.store, &s.principal, &s.ip, env, pack, copy).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_conflicts(
    State(app): State<Shared>,
    s: Signed,
    Path(env): Path<String>,
) -> Result<Json<Vec<Value>>, ApiError> {
    let env = visible(&app, &s, &env).await?;
    let cs = packs::conflicts(&app.store, env).await?;
    Ok(Json(
        cs.iter()
            .map(|c| {
                let mut x = json!({ "copy_id": c.copy_id.to_string(), "path": c.path, "found_at": c.found_at });
                if let Some(r) = &c.resolution {
                    x["resolution"] = json!(r);
                }
                x
            })
            .collect(),
    ))
}

async fn resolve_conflict(
    State(app): State<Shared>,
    s: Signed,
    Path(env): Path<String>,
    Body(b): Body<Value>,
) -> Result<StatusCode, ApiError> {
    let env = visible(&app, &s, &env).await?;
    packs::resolve_conflict(
        &app.store,
        &s.principal,
        &s.ip,
        env,
        text(&b, "copy_id"),
        text(&b, "path"),
        text(&b, "resolution"),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
