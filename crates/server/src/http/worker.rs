//! The worker API, /api/worker/v1, which the workers speak
//! (crates/worker's client): registering with the bootstrap token, then
//! with their own credential asking what to run, reporting what they run,
//! and holding the tunnel the server reaches environments through.

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header, request::Parts};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use hangar_api::{DesiredSet, EnvironmentFiles, RegisterWorker, WorkerCredential, WorkerStatus};
use uuid::Uuid;

use super::{ApiError, Body, Shared};
use crate::workers;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/register", post(register))
        .route("/desired", get(desired))
        .route("/status", put(status))
        .route("/environments/{id}/files", get(files))
        .route("/tunnel", get(tunnel))
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .filter(|t| !t.is_empty())
}

/// The worker a request's credential is. Only a credential refused is a
/// 401: a worker takes that as being revoked and powers its environments
/// off, which a store failure must not make it do (503).
pub struct WorkerId(pub Uuid);

impl FromRequestParts<Shared> for WorkerId {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, app: &Shared) -> Result<Self, ApiError> {
        let Some(credential) = bearer(&parts.headers) else {
            return Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "missing credential",
            ));
        };
        match workers::authenticate(&app.store, credential).await {
            Ok(Some(id)) => Ok(WorkerId(id)),
            Ok(None) => Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "invalid credential",
            )),
            Err(e) => {
                eprintln!("hangar-server: authenticating a worker: {e}");
                Err(ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "unavailable",
                ))
            }
        }
    }
}

async fn register(
    State(app): State<Shared>,
    headers: HeaderMap,
    Body(req): Body<RegisterWorker>,
) -> Result<(StatusCode, Json<WorkerCredential>), ApiError> {
    let given = bearer(&headers).unwrap_or_default();
    let ok = app
        .cfg
        .bootstrap_token
        .as_deref()
        .is_some_and(|t| !t.is_empty() && same(t.as_bytes(), given.as_bytes()));
    if !ok {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid bootstrap token",
        ));
    }
    let cred = workers::register(&app.store, &req.name, &req.labels).await?;
    eprintln!("hangar-server: worker registered: {} {}", req.name, cred.id);
    Ok((StatusCode::CREATED, Json(cred)))
}

/// a and b compared whole, however early they differ.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

#[derive(serde::Deserialize)]
struct After {
    after: Option<String>,
}

async fn desired(
    State(app): State<Shared>,
    WorkerId(id): WorkerId,
    Query(q): Query<After>,
) -> Result<Json<DesiredSet>, ApiError> {
    let after = match q.after.as_deref() {
        None | Some("") => 0,
        Some(v) => v
            .parse()
            .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "after must be an integer"))?,
    };
    workers::touch(&app.store, id).await?;
    let registry = app
        .cfg
        .public
        .as_ref()
        .filter(|_| app.cfg.registry)
        .map(|p| p.name("registry"))
        .unwrap_or_default();
    match workers::desired(&app.store, id, after, &registry).await? {
        Some(set) => Ok(Json(set)),
        None => Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid credential",
        )),
    }
}

async fn status(
    State(app): State<Shared>,
    WorkerId(id): WorkerId,
    Body(st): Body<WorkerStatus>,
) -> Result<StatusCode, ApiError> {
    workers::report(&app.store, id, &st).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn files(
    State(app): State<Shared>,
    WorkerId(worker): WorkerId,
    Path(env): Path<String>,
) -> Result<Json<EnvironmentFiles>, ApiError> {
    let on_it = match Uuid::parse_str(&env) {
        Ok(env) => app
            .store
            .begin()
            .await?
            .desired(worker)
            .await?
            .is_some_and(|(_, envs, _, _)| envs.iter().any(|e| e.id == env)),
        Err(_) => false,
    };
    if !on_it {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "no such environment on this worker",
        ));
    }
    Ok(Json(EnvironmentFiles::default()))
}

async fn tunnel(
    State(app): State<Shared>,
    WorkerId(id): WorkerId,
    ws: WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(move |socket| async move {
        eprintln!("hangar-server: worker tunnel opened: {id}");
        app.tunnels.serve(id, socket).await;
        eprintln!("hangar-server: worker tunnel closed: {id}");
    })
    .into_response()
}
