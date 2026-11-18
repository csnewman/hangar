//! The link API (`/api/link/v1`), which linked places speak to each other:
//! not in the specs, since no person calls it. A place dials another's
//! `connect` and keeps the WebSocket open; every message on it proves the
//! place sending it by its own key (`crate::linkconn`).

use axum::Router;
use axum::extract::State;
use axum::extract::ws::WebSocketUpgrade;
use axum::response::Response;
use axum::routing::get;

use super::Shared;

pub fn routes() -> Router<Shared> {
    Router::new().route("/connect", get(connect))
}

async fn connect(State(app): State<Shared>, ws: WebSocketUpgrade) -> Response {
    ws.max_message_size(16 << 20)
        .on_upgrade(move |socket| crate::linkconn::accept(app, socket))
}
