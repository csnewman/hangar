//! hangar-server: Hangar's control plane. Configured by its environment as
//! the Go server is (cmd/hangar-server), so either serves a deployment.

mod audit;
mod config;
mod fail;
mod http;
mod store;
mod tunnel;
mod users;
mod workers;

use std::process::ExitCode;
use std::sync::Arc;

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("hangar-server: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), String> {
    let cfg = config::Config::from_env()?;
    let store = store::Store::open(&cfg.storage)
        .await
        .map_err(|e| e.to_string())?;
    if let Some((name, password)) = &cfg.initial_admin
        && users::ensure_admin(&store, name, password)
            .await
            .map_err(|e| e.to_string())?
    {
        eprintln!("hangar-server: created the initial administrator {name}");
    }
    let listener = tokio::net::TcpListener::bind(&cfg.listen)
        .await
        .map_err(|e| format!("listening on {}: {e}", cfg.listen))?;
    eprintln!("hangar-server: serving HTTP on {}", cfg.listen);
    let app = http::router(Arc::new(http::App::new(store, cfg)));
    axum::serve(listener, app)
        .await
        .map_err(|e| format!("serving: {e}"))
}
