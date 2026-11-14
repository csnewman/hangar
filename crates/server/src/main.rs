//! hangar-server: Hangar's control plane. Configured by its environment as
//! the Go server is (cmd/hangar-server), so either serves a deployment.

mod audit;
mod config;
mod environments;
mod fail;
mod http;
mod keys;
mod placement;
mod ports;
mod publichost;
mod seal;
mod sshgw;
mod store;
mod teams;
mod templates;
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
    tokio::spawn(placement::run(store.clone()));
    let host_key = match &cfg.ssh {
        Some(_) => Some(
            keys::host_key(&store, cfg.sealer.as_deref())
                .await
                .map_err(|e| format!("the SSH gateway's host key: {e}"))?,
        ),
        None => None,
    };
    let app = Arc::new(http::App::new(store, cfg, host_key.as_ref()));
    if let (Some(ssh), Some(key)) = (&app.cfg.ssh, host_key) {
        tokio::spawn(sshgw::serve(app.clone(), ssh.listen.clone(), key));
    }
    let app = http::router(app);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .map_err(|e| format!("serving: {e}"))
}
