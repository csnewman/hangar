//! The link API (`/api/link/v1`), which linked places speak to each other:
//! not in the specs, since no person calls it; each request proves the
//! place sending it by its own key (`crate::link`).

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};

use super::{ApiError, Shared};
use crate::link::{self, Answer, Hello, Refused};

pub fn routes() -> Router<Shared> {
    Router::new().route("/hello", post(hello))
}

async fn hello(State(app): State<Shared>, Json(h): Json<Hello>) -> Result<Json<Answer>, ApiError> {
    let Some(place) = &app.place else {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "this place has no link key",
        ));
    };
    link::answer(&app.store, place, &app.links_wake, &h)
        .await
        .map(Json)
        .map_err(|r| match r {
            Refused::Elsewhere => {
                ApiError::new(StatusCode::NOT_FOUND, "this is another place").code("elsewhere")
            }
            Refused::Stale => ApiError::new(
                StatusCode::BAD_REQUEST,
                "the hello's time is more than 5 minutes from this place's clock",
            )
            .code("stale"),
            Refused::NotLinked => ApiError::new(
                StatusCode::NOT_FOUND,
                "no one here is linked with that place",
            )
            .code("not_linked"),
            Refused::Unproven => ApiError::new(
                StatusCode::UNAUTHORIZED,
                "the hello is not signed by the key this place was given",
            )
            .code("unproven"),
        })
}
