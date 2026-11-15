//! Apps signing in through the browser, in the shape of OAuth's device
//! authorization grant: the app asks for a grant and shows its code
//! (the client API's /sign-in), the person approves it on the server's own
//! page, signed in as themselves (the web UI's /sign-in/{code}), and the
//! app collects an access token made for them (/sign-in/token).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use hangar_openapi::{clientapi, frontendapi};

use super::{ApiError, Body, ClientIp, Shared, Signed};
use crate::users::{self, Collected};

/// How often an app asks whether its sign-in is approved, in seconds.
const INTERVAL: i64 = 2;

const NO_GRANT: &str = "no app is waiting under that code";

pub fn frontend() -> Router<Shared> {
    Router::new()
        .route("/sign-in/{code}", get(get_grant))
        .route("/sign-in/{code}/approve", post(approve))
        .route("/sign-in/{code}/refuse", post(refuse))
}

pub fn client() -> Router<Shared> {
    Router::new()
        .route("/sign-in", post(start))
        .route("/sign-in/token", post(finish))
}

async fn get_grant(
    State(app): State<Shared>,
    Path(code): Path<String>,
) -> Result<Json<frontendapi::types::SignInRequest>, ApiError> {
    let g = users::waiting_grant(&app.store, &code)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, NO_GRANT))?;
    Ok(Json(frontendapi::types::SignInRequest {
        code: g.user_code,
        name: g.name,
        created_at: g.created_at,
        expires_at: g.expires_at,
    }))
}

async fn decide(
    app: &Shared,
    s: &Signed,
    code: &str,
    refuse: bool,
) -> Result<StatusCode, ApiError> {
    if !users::decide_sign_in(&app.store, &s.principal, &s.ip, code, refuse).await? {
        return Err(ApiError::new(StatusCode::NOT_FOUND, NO_GRANT));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn approve(
    State(app): State<Shared>,
    s: Signed,
    Path(code): Path<String>,
) -> Result<StatusCode, ApiError> {
    decide(&app, &s, &code, false).await
}

async fn refuse(
    State(app): State<Shared>,
    s: Signed,
    Path(code): Path<String>,
) -> Result<StatusCode, ApiError> {
    decide(&app, &s, &code, true).await
}

async fn start(
    State(app): State<Shared>,
    Body(b): Body<clientapi::types::StartSignIn>,
) -> Result<(StatusCode, Json<clientapi::types::SignInGrant>), ApiError> {
    let g = users::start_sign_in(&app.store, &b.name).await?;
    let path = format!(
        "/connect/desktop?{}",
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("code", &g.row.user_code)
            .finish()
    );
    Ok((
        StatusCode::CREATED,
        Json(clientapi::types::SignInGrant {
            device_code: g.device,
            user_code: g.row.user_code,
            verification_path: path,
            expires_at: g.row.expires_at,
            interval: INTERVAL,
        }),
    ))
}

async fn finish(
    State(app): State<Shared>,
    ClientIp(ip): ClientIp,
    Body(b): Body<clientapi::types::FinishSignIn>,
) -> Result<Response, ApiError> {
    Ok(
        match users::finish_sign_in(&app.store, &ip, &b.device_code).await? {
            Collected::Token(token, t) => Json(clientapi::types::SignInToken {
                token,
                name: t.name,
            })
            .into_response(),
            Collected::Pending => ApiError::new(
                StatusCode::ACCEPTED,
                "waiting for the sign-in to be approved",
            )
            .code(super::code::SIGN_IN_PENDING)
            .into_response(),
            Collected::Refused => {
                ApiError::new(StatusCode::FORBIDDEN, "the sign-in was refused").into_response()
            }
            Collected::Gone => ApiError::new(
                StatusCode::NOT_FOUND,
                "no such sign-in: it expired, was collected, or never was",
            )
            .into_response(),
        },
    )
}
