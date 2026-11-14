//! The audit log: the web UI's audit page, and the client API's activity
//! of one environment.

use axum::extract::{Path, RawQuery, State};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};

use super::{ApiError, Shared, Signed};
use crate::audit::{self, Entry};

pub fn frontend() -> Router<Shared> {
    Router::new().route("/audit", get(list))
}

pub fn client() -> Router<Shared> {
    Router::new().route("/environments/{id}/activity", get(activity))
}

/// What a query asks of the log.
#[derive(Default)]
struct Params {
    subjects: Vec<String>,
    before: Option<i64>,
    limit: Option<i64>,
}

/// A query's subject (repeated), before and limit.
fn params(query: Option<String>) -> Result<Params, ApiError> {
    let mut p = Params::default();
    for (k, v) in form_urlencoded::parse(query.unwrap_or_default().as_bytes()) {
        let number = || {
            v.parse::<i64>()
                .map_err(|_| ApiError::invalid(format!("{k} is a number")))
        };
        match k.as_ref() {
            "subject" => p.subjects.push(v.to_string()),
            "before" => p.before = Some(number()?),
            "limit" => p.limit = Some(number()?),
            _ => {}
        }
    }
    Ok(p)
}

fn actor_json(e: &Entry) -> Value {
    let ev = &e.event;
    let mut actor = json!({ "kind": e.actor_kind, "name": ev.actor_name });
    if let Some(id) = ev.actor_worker.or(ev.actor_user) {
        actor["id"] = json!(id.to_string());
    }
    actor
}

fn target_json(e: &Entry) -> Value {
    let ev = &e.event;
    json!({ "type": ev.target_type, "id": ev.target_id, "name": ev.target_name })
}

fn details(e: &Entry) -> Value {
    match &e.event.details {
        Value::Object(_) => e.event.details.clone(),
        _ => json!({}),
    }
}

async fn list(
    State(app): State<Shared>,
    s: Signed,
    RawQuery(q): RawQuery,
) -> Result<Json<Vec<Value>>, ApiError> {
    let q = params(q)?;
    let entries = audit::list(&app.store, &s.principal, &q.subjects, q.before, q.limit).await?;
    Ok(Json(
        entries
            .iter()
            .map(|e| {
                let mut out = json!({
                    "id": e.event.id,
                    "at": e.event.at,
                    "actor": actor_json(e),
                    "action": e.event.action,
                    "target": target_json(e),
                    "subjects": e.event.subjects,
                    "details": details(e),
                });
                if s.principal.admin && !e.event.source_ip.is_empty() {
                    out["ip"] = json!(e.event.source_ip);
                }
                out
            })
            .collect(),
    ))
}

/// The events about an environment, as its page shows them.
async fn activity(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
    RawQuery(q): RawQuery,
) -> Result<Json<Vec<Value>>, ApiError> {
    let id = uuid::Uuid::parse_str(&id).map_err(|_| ApiError::not_found("environment"))?;
    if crate::environments::get(&app.store, &s.principal, id)
        .await?
        .is_none()
    {
        return Err(ApiError::not_found("environment"));
    }
    let q = params(q)?;
    let subject = [format!("environment:{id}")];
    let entries = audit::list(&app.store, &s.principal, &subject, q.before, q.limit).await?;
    Ok(Json(
        entries
            .iter()
            .map(|e| {
                json!({
                    "id": e.event.id,
                    "at": e.event.at,
                    "actor": actor_json(e),
                    "action": e.event.action,
                    "target": target_json(e),
                    "details": details(e),
                })
            })
            .collect(),
    ))
}
