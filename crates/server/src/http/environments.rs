//! Environments in the web UI's API and the client API, and the client
//! API's stream of their changes.

use std::collections::HashSet;
use std::convert::Infallible;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use hangar_api::{EnvironmentStats, Progress, Repo};
use hangar_openapi::{clientapi, frontendapi};
use serde_json::{Value, json};
use uuid::Uuid;

use super::catalog::spec_json;
use super::{ApiError, Body, List, OptionalBody, Shared, Signed, spec as to_spec};
use crate::environments::{self as envs, Create, Machine, View};
use crate::users::Principal;

pub fn frontend() -> Router<Shared> {
    Router::new()
        .route("/environments", get(list_frontend).post(create_frontend))
        .route(
            "/environments/{id}",
            get(get_frontend).delete(delete_frontend),
        )
        .route("/environments/{id}/start", post(start_frontend))
        .route("/environments/{id}/stop", post(stop_frontend))
        .route("/environments/{id}/suspend", post(suspend_frontend))
        .route("/environments/{id}/settings", put(settings_frontend))
        .route("/environments/{id}/reset-to-template", post(reset_frontend))
        .route("/environments/{id}/image/upgrade", post(upgrade_frontend))
        .route("/environments/{id}/image/rollback", post(rollback_frontend))
        .route("/environments/{id}/image/keep", post(keep_frontend))
        .route("/environments/{id}/image/cancel", post(cancel_frontend))
        .route("/environments/{id}/ports", put(ports_frontend))
        .route("/worker-sizes", get(sizes))
}

pub fn client() -> Router<Shared> {
    Router::new()
        .route("/environments", get(list_client).post(create_client))
        .route("/environments/{id}", get(get_client).delete(delete_client))
        .route("/environments/{id}/start", post(start_client))
        .route("/environments/{id}/stop", post(stop_client))
        .route("/environments/{id}/suspend", post(suspend_client))
        .route("/environments/{id}/reset", post(reset_client))
        .route("/environments/{id}/settings", put(settings_client))
        .route("/environments/{id}/image/{change}", post(image_client))
        .route("/environments/{id}/ports", put(ports_client))
        .route("/environments/{id}/task", put(set_task).delete(end_task))
        .route("/environments/{id}/tasks", get(tasks))
        .route("/environments/{id}/agents/{session}/seen", post(see_agent))
        .route("/worker-sizes", get(client_sizes))
        .route("/events", get(events))
}

fn env_id(s: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(s).map_err(|_| ApiError::not_found("environment"))
}

fn some_if<T: serde::Serialize>(
    o: &mut serde_json::Map<String, Value>,
    key: &str,
    cond: bool,
    v: T,
) {
    if cond {
        o.insert(key.into(), json!(v));
    }
}

fn progress_json(p: &Progress) -> Value {
    let mut out = json!({ "step": p.step });
    if p.total > 0 {
        out["done"] = json!(p.done);
        out["total"] = json!(p.total);
        out["unit"] = json!(p.unit);
        if p.rate > 0.0 {
            out["rate"] = json!(p.rate);
        }
    }
    out
}

/// An environment as the web UI's API shows it.
fn frontend_json(v: &View) -> Value {
    let r = &v.row;
    let mut out = json!({
        "id": r.id.to_string(),
        "short_id": r.short_id,
        "owner_id": r.owner_id.to_string(),
        "owner": v.owner,
        "name": r.name,
        "template": r.template_name,
        "spec": spec_json(&v.spec),
        "image": r.image,
        "cpus": r.cpus,
        "memory_mib": r.memory_mib,
        "desired": r.desired,
        "phase": r.phase,
        "created_at": r.created_at,
        "updated_at": r.updated_at,
    });
    let o = out.as_object_mut().expect("an object");
    if let Some(t) = r.template_id {
        o.insert("template_id".into(), json!(t.to_string()));
    }
    if let Some((renderer, software)) = &v.gpu_renderer {
        o.insert("gpu_renderer".into(), json!(renderer));
        some_if(o, "gpu_software", *software, true);
    }
    some_if(
        o,
        "template_changes",
        !v.template_changes.is_empty(),
        &v.template_changes,
    );
    some_if(o, "template_updated", v.template_updated, true);
    some_if(
        o,
        "image_digest",
        !r.image_digest.is_empty(),
        &r.image_digest,
    );
    if let Some(u) = &v.image_update {
        o.insert("image_update".into(), json!(u));
    }
    if let Some(rb) = &v.image_rollback {
        o.insert("image_rollback".into(), json!(rb));
    }
    if let Some(a) = &r.guest_agent {
        o.insert("guest_agent".into(), a.clone());
    }
    if let Some(c) = v.image_change {
        o.insert("image_change".into(), json!(c));
    }
    some_if(o, "ports_public", r.ports_public, true);
    some_if(o, "reason", !r.reason.is_empty(), &r.reason);
    if let (Some(id), Some(w)) = (r.worker_id, &v.worker) {
        o.insert("worker_id".into(), json!(id.to_string()));
        o.insert("worker".into(), json!(w.name));
        o.insert("worker_online".into(), json!(w.online));
        some_if(o, "monitor", !w.monitor.is_empty(), &w.monitor);
        some_if(o, "suspends", w.suspends, true);
    }
    if let Some(s) = &v.stats {
        o.insert("stats".into(), frontend_stats(s));
    }
    if let Some(p) = &v.progress {
        o.insert("progress".into(), progress_json(p));
    }
    out
}

fn frontend_stats(s: &EnvironmentStats) -> Value {
    let mut out = serde_json::to_value(s).unwrap_or_default();
    let o = out.as_object_mut().expect("an object");
    o.remove("machine_memory_min_mib");
    o.remove("machine_memory_max_mib");
    if s.machine_memory_max_mib > 0 {
        o.insert(
            "machine_memory_min_mib".into(),
            json!(s.machine_memory_min_mib),
        );
        o.insert(
            "machine_memory_max_mib".into(),
            json!(s.machine_memory_max_mib),
        );
    }
    out
}

/// An environment as the client API shows it.
fn client_json(v: &View) -> Value {
    let r = &v.row;
    let s = &v.spec;
    let mut out = json!({
        "id": r.id.to_string(),
        "short_id": r.short_id,
        "name": r.name,
        "owner_id": r.owner_id.to_string(),
        "owner": v.owner,
        "template": r.template_name,
        "phase": r.phase,
        "desired": r.desired,
        "cpus": r.cpus,
        "memory_mib": r.memory_mib,
        "display": s.display,
        "gpu": s.gpu,
        "ports_public": r.ports_public,
        "created_at": r.created_at,
        "updated_at": r.updated_at,
    });
    let o = out.as_object_mut().expect("an object");
    some_if(o, "reason", !r.reason.is_empty(), &r.reason);
    if let (Some(_), Some(w)) = (r.worker_id, &v.worker) {
        o.insert("worker".into(), json!(w.name));
        o.insert("worker_online".into(), json!(w.online));
    }
    some_if(o, "dax", s.dax, true);
    if let Some(m) = s.swap_mib {
        o.insert("swap_mib".into(), json!(m));
    }
    some_if(o, "memory_reclaim", s.memory_reclaim, true);
    if s.access != hangar_api::Access::default() {
        o.insert("access".into(), json!(s.access));
    }
    some_if(o, "placement", !s.placement.is_empty(), &s.placement);
    if let Some(t) = r.template_id {
        o.insert("template_id".into(), json!(t.to_string()));
    }
    some_if(
        o,
        "template_changes",
        !v.template_changes.is_empty(),
        &v.template_changes,
    );
    some_if(o, "image", !r.image.is_empty(), &r.image);
    some_if(
        o,
        "image_digest",
        !r.image_digest.is_empty(),
        &r.image_digest,
    );
    if let Some(c) = v.image_change {
        o.insert("image_change".into(), json!(c));
    }
    if let Some(rb) = &v.image_rollback {
        o.insert("image_rollback".into(), json!(rb));
    }
    if let Some(a) = &r.guest_agent {
        o.insert("guest_agent".into(), a.clone());
    }
    some_if(o, "editor_path", !s.editor_path.is_empty(), &s.editor_path);
    if let Some(st) = &v.stats {
        o.insert("stats".into(), client_stats(st));
    }
    some_if(o, "web_names", !s.web_names.is_empty(), &s.web_names);
    some_if(
        o,
        "workspace_file",
        s.projects.len() > 1,
        hangar_api::WORKSPACE_FILE,
    );
    if let Some(t) = &v.task {
        o.insert("task".into(), task_json(t));
    }
    if let Some(u) = &v.image_update {
        o.insert("image_update".into(), json!(u));
    }
    if !v.agents.is_empty() {
        let agents: Vec<Value> = v
            .agents
            .iter()
            .map(|a| {
                let mut x = json!({
                    "id": a.row.id,
                    "agent": a.row.agent,
                    "state": a.state,
                    "since": a.row.since,
                    "needs_you": a.needs_you(),
                });
                let xo = x.as_object_mut().expect("an object");
                some_if(xo, "terminal", !a.row.terminal.is_empty(), &a.row.terminal);
                some_if(xo, "step", !a.row.step.is_empty(), &a.row.step);
                some_if(xo, "message", !a.row.message.is_empty(), &a.row.message);
                x
            })
            .collect();
        o.insert("agents".into(), json!(agents));
    }
    if let Some(p) = &v.progress {
        o.insert("progress".into(), progress_json(p));
    }
    out
}

fn client_stats(s: &EnvironmentStats) -> Value {
    let mut out = json!({
        "cpu_percent": s.cpu_percent,
        "memory_used_mib": s.memory_used_mib,
        "memory_total_mib": s.memory_total_mib,
        "memory_cached_mib": s.memory_cached_mib,
        "host_memory_mib": s.host_memory_mib,
        "swap_total_mib": s.swap_total_mib,
        "swap_used_mib": s.swap_used_mib,
        "swap_compressed_mib": s.swap_compressed_mib,
        "swap_pool_mib": s.swap_pool_mib,
        "swap_disk_bytes": s.swap_disk_bytes,
        "swap_in_bps": s.swap_in_bps,
        "swap_out_bps": s.swap_out_bps,
        "disk_used_bytes": s.disk_used_bytes,
        "disk_read_bps": s.disk_read_bps,
        "disk_write_bps": s.disk_write_bps,
        "net_rx_bps": s.net_rx_bps,
        "net_tx_bps": s.net_tx_bps,
    });
    if s.machine_memory_mib > 0 {
        out["machine_memory_mib"] = json!(s.machine_memory_mib);
    }
    if s.machine_memory_max_mib > 0 {
        out["machine_memory_min_mib"] = json!(s.machine_memory_min_mib);
        out["machine_memory_max_mib"] = json!(s.machine_memory_max_mib);
    }
    out
}

fn task_json(t: &crate::store::rows::Task) -> Value {
    let mut out = json!({
        "id": t.id.to_string(),
        "text": t.text,
        "set_by": t.set_by,
        "set_at": t.set_at,
    });
    if let Some(d) = t.done_at {
        out["done_at"] = json!(d);
    }
    out
}

fn frontend_env(v: &View) -> Result<Json<frontendapi::types::Environment>, ApiError> {
    Ok(Json(to_spec(frontend_json(v))?))
}

fn client_env(v: &View) -> Result<Json<clientapi::types::Environment>, ApiError> {
    Ok(Json(to_spec(client_json(v))?))
}

/// A machine's settings as either API's body gives them.
#[derive(serde::Deserialize)]
struct MachineBody {
    cpus: i64,
    memory_mib: i64,
    display: hangar_api::Display,
    gpu: hangar_api::Gpu,
    #[serde(default)]
    dax: Option<bool>,
    #[serde(default)]
    swap_mib: Option<i64>,
    #[serde(default)]
    memory_reclaim: Option<bool>,
}

impl MachineBody {
    fn machine(self) -> Machine {
        Machine {
            cpus: self.cpus,
            memory_mib: self.memory_mib,
            display: self.display,
            gpu: self.gpu,
            dax: self.dax.unwrap_or(false),
            swap_mib: self.swap_mib,
            memory_reclaim: self.memory_reclaim.unwrap_or(false),
        }
    }
}

/// What a create body asks for, from either API's: repositories given,
/// even none, take the template's place.
fn create_of(body: &Value) -> Result<Create, ApiError> {
    #[derive(serde::Deserialize)]
    struct CreateBody {
        template_id: String,
        name: String,
        #[serde(default)]
        settings: Option<MachineBody>,
        #[serde(default)]
        repos: Option<Vec<Repo>>,
    }
    let b: CreateBody = serde_json::from_value(body.clone()).map_err(ApiError::invalid)?;
    Ok(Create {
        template_id: b.template_id,
        name: b.name,
        machine: b.settings.map(MachineBody::machine),
        repos: b.repos,
    })
}

/// A body checked against the spec's type T, then read as what the
/// server takes from it.
fn checked<T: serde::de::DeserializeOwned>(body: &Value) -> Result<(), ApiError> {
    serde_json::from_value::<T>(body.clone())
        .map(|_| ())
        .map_err(ApiError::invalid)
}

fn settings_of(body: &Value) -> Result<Machine, ApiError> {
    let b: MachineBody = serde_json::from_value(body.clone()).map_err(ApiError::invalid)?;
    Ok(b.machine())
}

fn force_of(body: &Option<Value>) -> bool {
    body.as_ref()
        .and_then(|b| b.get("force"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn public_of(body: &Value) -> Result<bool, ApiError> {
    body.get("public")
        .and_then(Value::as_bool)
        .ok_or_else(|| ApiError::invalid("public is a boolean"))
}

async fn list_frontend(State(app): State<Shared>, s: Signed) -> Result<Response, ApiError> {
    let list = envs::list(&app.store, &s.principal).await?;
    let out: Vec<frontendapi::types::Environment> = list
        .iter()
        .map(|v| to_spec(frontend_json(v)))
        .collect::<Result<_, _>>()?;
    Ok(Json(out).into_response())
}

async fn list_client(State(app): State<Shared>, s: Signed) -> Result<Response, ApiError> {
    let list = envs::list(&app.store, &s.principal).await?;
    let out: Vec<clientapi::types::Environment> = list
        .iter()
        .filter(|v| v.row.desired != "deleted")
        .map(|v| to_spec(client_json(v)))
        .collect::<Result<_, _>>()?;
    Ok(List::<clientapi::types::Environment>::all(out).into_response())
}

async fn found(app: &Shared, p: &Principal, id: &str) -> Result<View, ApiError> {
    envs::get(&app.store, p, env_id(id)?)
        .await?
        .ok_or_else(|| ApiError::not_found("environment"))
}

async fn get_frontend(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    Ok(frontend_env(&found(&app, &s.principal, &id).await?)?.into_response())
}

async fn get_client(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    Ok(client_env(&found(&app, &s.principal, &id).await?)?.into_response())
}

async fn create_frontend(
    State(app): State<Shared>,
    s: Signed,
    Body(b): Body<Value>,
) -> Result<Response, ApiError> {
    checked::<frontendapi::types::CreateEnvironment>(&b)?;
    let v = envs::create(&app.store, &s.principal, &s.ip, create_of(&b)?).await?;
    Ok((StatusCode::CREATED, frontend_env(&v)?).into_response())
}

async fn create_client(
    State(app): State<Shared>,
    s: Signed,
    Body(b): Body<Value>,
) -> Result<Response, ApiError> {
    checked::<clientapi::types::CreateEnvironment>(&b)?;
    let v = envs::create(&app.store, &s.principal, &s.ip, create_of(&b)?).await?;
    Ok((StatusCode::CREATED, client_env(&v)?).into_response())
}

/// Asks for an environment to be in a state, and gives it as it then
/// stands, or None when that removed it.
async fn desire(
    app: &Shared,
    s: &Signed,
    id: &str,
    desired: &str,
) -> Result<Option<View>, ApiError> {
    let id = env_id(id)?;
    if !envs::set_desired(&app.store, &s.principal, &s.ip, id, desired).await? {
        return Ok(None);
    }
    Ok(envs::get(&app.store, &s.principal, id).await?)
}

async fn desired_frontend(
    app: Shared,
    s: Signed,
    id: String,
    desired: &str,
) -> Result<Response, ApiError> {
    match desire(&app, &s, &id, desired).await? {
        Some(v) => Ok(frontend_env(&v)?.into_response()),
        None if desired == "deleted" => Ok(StatusCode::NO_CONTENT.into_response()),
        None => Err(ApiError::not_found("environment")),
    }
}

async fn desired_client(
    app: Shared,
    s: Signed,
    id: String,
    desired: &str,
) -> Result<Response, ApiError> {
    match desire(&app, &s, &id, desired).await? {
        Some(v) => Ok(client_env(&v)?.into_response()),
        None if desired == "deleted" => Ok(StatusCode::NO_CONTENT.into_response()),
        None => Err(ApiError::new(StatusCode::NOT_FOUND, "no such environment")),
    }
}

async fn start_frontend(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    desired_frontend(app, s, id, "running").await
}

async fn stop_frontend(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    desired_frontend(app, s, id, "stopped").await
}

async fn suspend_frontend(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    desired_frontend(app, s, id, "suspended").await
}

async fn delete_frontend(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    desired_frontend(app, s, id, "deleted").await
}

async fn start_client(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    desired_client(app, s, id, "running").await
}

async fn stop_client(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    desired_client(app, s, id, "stopped").await
}

async fn suspend_client(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    desired_client(app, s, id, "suspended").await
}

async fn delete_client(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    desired_client(app, s, id, "deleted").await
}

async fn settings_frontend(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
    Body(b): Body<Value>,
) -> Result<Response, ApiError> {
    checked::<frontendapi::types::EnvironmentSettings>(&b)?;
    let v = envs::update_settings(
        &app.store,
        &s.principal,
        &s.ip,
        env_id(&id)?,
        settings_of(&b)?,
    )
    .await?;
    Ok(frontend_env(&v)?.into_response())
}

async fn settings_client(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
    Body(b): Body<Value>,
) -> Result<Response, ApiError> {
    checked::<clientapi::types::EnvironmentSettings>(&b)?;
    let v = envs::update_settings(
        &app.store,
        &s.principal,
        &s.ip,
        env_id(&id)?,
        settings_of(&b)?,
    )
    .await?;
    Ok(client_env(&v)?.into_response())
}

async fn reset_frontend(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let v = envs::reset_to_template(&app.store, &s.principal, &s.ip, env_id(&id)?).await?;
    Ok(frontend_env(&v)?.into_response())
}

async fn reset_client(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let v = envs::reset_to_template(&app.store, &s.principal, &s.ip, env_id(&id)?).await?;
    Ok(client_env(&v)?.into_response())
}

async fn upgrade_frontend(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
    OptionalBody(b): OptionalBody<Value>,
) -> Result<Response, ApiError> {
    let v =
        envs::upgrade_image(&app.store, &s.principal, &s.ip, env_id(&id)?, force_of(&b)).await?;
    Ok(frontend_env(&v)?.into_response())
}

async fn rollback_frontend(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let v = envs::rollback_image(&app.store, &s.principal, &s.ip, env_id(&id)?).await?;
    Ok(frontend_env(&v)?.into_response())
}

async fn keep_frontend(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let v = envs::keep_image(&app.store, &s.principal, &s.ip, env_id(&id)?).await?;
    Ok(frontend_env(&v)?.into_response())
}

async fn cancel_frontend(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let v = envs::cancel_image_change(&app.store, &s.principal, &s.ip, env_id(&id)?).await?;
    Ok(frontend_env(&v)?.into_response())
}

async fn image_client(
    State(app): State<Shared>,
    s: Signed,
    Path((id, change)): Path<(String, String)>,
    OptionalBody(b): OptionalBody<Value>,
) -> Result<Response, ApiError> {
    let (st, p, ip, id) = (&app.store, &s.principal, s.ip.as_str(), env_id(&id)?);
    let v = match change.as_str() {
        "upgrade" => envs::upgrade_image(st, p, ip, id, force_of(&b)).await?,
        "rollback" => envs::rollback_image(st, p, ip, id).await?,
        "keep" => envs::keep_image(st, p, ip, id).await?,
        "cancel" => envs::cancel_image_change(st, p, ip, id).await?,
        _ => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "change is upgrade, rollback, keep or cancel",
            ));
        }
    };
    Ok(client_env(&v)?.into_response())
}

async fn ports_frontend(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
    Body(b): Body<Value>,
) -> Result<Response, ApiError> {
    let v = envs::set_ports_public(
        &app.store,
        &s.principal,
        &s.ip,
        env_id(&id)?,
        public_of(&b)?,
    )
    .await?;
    Ok(frontend_env(&v)?.into_response())
}

async fn ports_client(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
    Body(b): Body<Value>,
) -> Result<Response, ApiError> {
    let v = envs::set_ports_public(
        &app.store,
        &s.principal,
        &s.ip,
        env_id(&id)?,
        public_of(&b)?,
    )
    .await?;
    Ok(client_env(&v)?.into_response())
}

async fn set_task(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
    Body(b): Body<clientapi::types::SetTask>,
) -> Result<Response, ApiError> {
    let t = envs::set_task(&app.store, &s.principal, env_id(&id)?, &b.text).await?;
    Ok(Json(task_json(&t)).into_response())
}

async fn end_task(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    envs::end_task(&app.store, &s.principal, env_id(&id)?).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn tasks(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let ts = envs::tasks(&app.store, &s.principal, env_id(&id)?).await?;
    Ok(List::all(ts.iter().map(task_json).collect::<Vec<_>>()).into_response())
}

async fn see_agent(
    State(app): State<Shared>,
    s: Signed,
    Path((id, session)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let id = Uuid::parse_str(&id)
        .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, "no such session"))?;
    envs::see_agent(&app.store, &s.principal, id, &session).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// How big an environment may be: each online worker's capacity, largest
/// first, not saying which is which.
async fn sizes(State(app): State<Shared>, _s: Signed) -> Result<Response, ApiError> {
    Ok(Json(worker_sizes(&app).await?).into_response())
}

async fn client_sizes(State(app): State<Shared>, _s: Signed) -> Result<Response, ApiError> {
    Ok(List::all(worker_sizes(&app).await?).into_response())
}

async fn worker_sizes(app: &Shared) -> Result<Vec<Value>, ApiError> {
    let mut out: Vec<(i64, i64)> = crate::workers::list(&app.store)
        .await?
        .iter()
        .filter(|l| crate::workers::online(&l.worker))
        .map(|l| (l.worker.cpus as i64, l.worker.memory_mib as i64))
        .collect();
    out.sort_by(|a, b| b.cmp(a));
    Ok(out
        .into_iter()
        .map(|(cpus, memory_mib)| json!({ "cpus": cpus, "memory_mib": memory_mib }))
        .collect())
}

/// How often an idle stream says it is alive.
const STREAM_PING: Duration = Duration::from_secs(30);

/// How long a change waits for others close behind it, to go as one.
const STREAM_GATHER: Duration = Duration::from_millis(50);

fn event(name: &str, data: &Value) -> Bytes {
    Bytes::from(format!("event: {name}\ndata: {data}\n\n"))
}

/// The signed-in person's environments as server-sent events: each as it
/// is, then each again as it changes, removed as it goes.
async fn events(State(app): State<Shared>, s: Signed) -> Response {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, Infallible>>(64);
    // Watching before the first listing, so nothing between is missed.
    let watch = app.store.signals.watch_changes();
    tokio::spawn(async move {
        if let Err(e) = stream(&app, &s.principal, &watch, &tx).await {
            eprintln!("hangar-server: the event stream: {e}");
        }
    });
    (
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        axum::body::Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx)),
    )
        .into_response()
}

type Sender = tokio::sync::mpsc::Sender<Result<Bytes, Infallible>>;

/// Writes p's environments' changes to tx until whoever reads it goes.
async fn stream(
    app: &Shared,
    p: &Principal,
    watch: &crate::store::signals::Watch,
    tx: &Sender,
) -> Result<(), crate::store::Error> {
    let gone = |_| crate::store::Error("closed".into());
    let mut sent: HashSet<Uuid> = HashSet::new();
    let (mut ids, mut all, mut synced) = (Vec::new(), true, false);
    let mut ping = tokio::time::interval(STREAM_PING);
    ping.tick().await;
    loop {
        if all {
            let list = envs::list(&app.store, p).await?;
            let mut now = HashSet::new();
            for v in list.iter().filter(|v| v.row.desired != "deleted") {
                if tx
                    .send(Ok(event("environment", &client_json(v))))
                    .await
                    .is_err()
                {
                    return Ok(());
                }
                now.insert(v.row.id);
            }
            for id in sent.difference(&now) {
                if tx
                    .send(Ok(event("removed", &json!({ "id": id.to_string() }))))
                    .await
                    .is_err()
                {
                    return Ok(());
                }
            }
            sent = now;
            if !synced {
                if tx.send(Ok(event("synced", &json!({})))).await.is_err() {
                    return Ok(());
                }
                synced = true;
            }
        } else {
            for id in ids.drain(..) {
                match envs::get(&app.store, p, id).await? {
                    Some(v) if v.row.desired != "deleted" => {
                        sent.insert(id);
                        tx.send(Ok(event("environment", &client_json(&v))))
                            .await
                            .map_err(gone)?;
                    }
                    _ => {
                        if sent.remove(&id) {
                            tx.send(Ok(event("removed", &json!({ "id": id.to_string() }))))
                                .await
                                .map_err(gone)?;
                        }
                    }
                }
            }
        }
        all = false;
        while ids.is_empty() && !all {
            tokio::select! {
                _ = tx.closed() => return Ok(()),
                _ = ping.tick() => {
                    if tx.send(Ok(Bytes::from_static(b": ping\n\n"))).await.is_err() {
                        return Ok(());
                    }
                }
                _ = watch.ready() => {
                    tokio::time::sleep(STREAM_GATHER).await;
                    (ids, all) = watch.take();
                }
            }
        }
    }
}
