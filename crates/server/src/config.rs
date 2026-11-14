//! The server's configuration: its environment, as the Go server's
//! (cmd/hangar-server) is, so either reads the same deployment. A setting
//! of the Go server's that this one does not read is passed over, not
//! refused.

use std::path::PathBuf;

use crate::publichost::{Public, Style};

/// Where the server keeps its state.
#[derive(Debug, Clone)]
pub enum Storage {
    /// A Postgres database, by URL: a deployment, any number of servers.
    Postgres(String),
    /// A JSON file, for one server on one machine.
    Json(PathBuf),
}

#[derive(Debug, Clone)]
pub struct Config {
    /// The address HTTP is served on (`HANGAR_LISTEN`, :8081 unless set).
    pub listen: String,
    pub storage: Storage,
    /// The token a worker presents to register (`HANGAR_BOOTSTRAP_TOKEN`,
    /// or the file `HANGAR_BOOTSTRAP_TOKEN_FILE` names).
    pub bootstrap_token: Option<String>,
    /// The administrator made at start when there is none by that name.
    pub initial_admin: Option<(String, String)>,
    /// Where Hangar is reached (`HANGAR_PUBLIC_URL`), and how its other
    /// hosts are named after it (`HANGAR_HOST_STYLE`).
    pub public: Option<Public>,
    /// Whether the server is its own container registry: it is whenever it
    /// has a public URL, unless `HANGAR_REGISTRY_ENABLED` is false.
    pub registry: bool,
}

impl Config {
    /// Reads the configuration from the environment.
    pub fn from_env() -> Result<Config, String> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let storage = match (var("HANGAR_DATABASE_URL"), var("HANGAR_STATE_FILE")) {
            (Some(url), None) => Storage::Postgres(url),
            (None, Some(path)) => Storage::Json(path.into()),
            (Some(_), Some(_)) => {
                return Err(
                    "HANGAR_DATABASE_URL and HANGAR_STATE_FILE are both set: a server keeps its state in one".into(),
                );
            }
            (None, None) => {
                return Err(
                    "no storage: set HANGAR_DATABASE_URL (Postgres) or HANGAR_STATE_FILE (a JSON file)".into(),
                );
            }
        };
        let bootstrap_token = match (
            var("HANGAR_BOOTSTRAP_TOKEN"),
            var("HANGAR_BOOTSTRAP_TOKEN_FILE"),
        ) {
            (Some(t), _) => Some(t),
            (None, Some(f)) => Some(
                std::fs::read_to_string(&f)
                    .map_err(|e| format!("HANGAR_BOOTSTRAP_TOKEN_FILE: {e}"))?
                    .trim()
                    .to_string(),
            ),
            (None, None) => None,
        };
        let initial_admin = var("HANGAR_INITIAL_ADMIN_PASSWORD").map(|pw| {
            (
                var("HANGAR_INITIAL_ADMIN_USERNAME").unwrap_or_else(|| "admin".into()),
                pw,
            )
        });
        let style = Style::parse(&var("HANGAR_HOST_STYLE").unwrap_or_default())
            .map_err(|e| format!("HANGAR_HOST_STYLE: {e}"))?;
        let public = var("HANGAR_PUBLIC_URL")
            .map(|url| Public::parse(&url, style).map_err(|e| format!("HANGAR_PUBLIC_URL: {e}")))
            .transpose()?;
        let listen = var("HANGAR_LISTEN").unwrap_or_else(|| ":8081".into());
        let listen = match listen.strip_prefix(':') {
            Some(port) => format!("0.0.0.0:{port}"),
            None => listen,
        };
        let registry =
            public.is_some() && var("HANGAR_REGISTRY_ENABLED").as_deref() != Some("false");
        Ok(Config {
            listen,
            storage,
            bootstrap_token,
            initial_admin,
            public,
            registry,
        })
    }
}
