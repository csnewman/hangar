//! The server's configuration: its environment, as the Go server's
//! (cmd/hangar-server) is, so either reads the same deployment. A setting
//! of the Go server's that this one does not read is passed over, not
//! refused.

use std::path::PathBuf;

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
    /// The administrator made at start when there is none by that name.
    pub initial_admin: Option<(String, String)>,
    /// Where Hangar is reached (`HANGAR_PUBLIC_URL`): its scheme, and its
    /// host with any port.
    pub public: Option<Public>,
    /// Whether the server is its own container registry: it is whenever it
    /// has a public URL, unless `HANGAR_REGISTRY_ENABLED` is false.
    pub registry: bool,
}

/// The public URL, split.
#[derive(Debug, Clone)]
pub struct Public {
    pub scheme: String,
    /// The host, with its port when it has one.
    pub host: String,
}

impl Public {
    /// Whether people reach Hangar over HTTPS.
    pub fn secure(&self) -> bool {
        self.scheme == "https"
    }

    /// The name of the host for label, under Hangar's: `<label>.<host>`.
    pub fn name(&self, label: &str) -> String {
        format!("{}.{}", label.to_lowercase(), self.host)
    }
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
        let initial_admin = var("HANGAR_INITIAL_ADMIN_PASSWORD").map(|pw| {
            (
                var("HANGAR_INITIAL_ADMIN_USERNAME").unwrap_or_else(|| "admin".into()),
                pw,
            )
        });
        let public = match var("HANGAR_PUBLIC_URL") {
            Some(url) => {
                let (scheme, rest) = url
                    .split_once("://")
                    .ok_or_else(|| format!("HANGAR_PUBLIC_URL {url:?} is not a URL"))?;
                let host = rest.split('/').next().unwrap_or_default().to_lowercase();
                if host.is_empty() || !matches!(scheme, "http" | "https") {
                    return Err(format!("HANGAR_PUBLIC_URL {url:?} is not an http(s) URL"));
                }
                Some(Public {
                    scheme: scheme.into(),
                    host,
                })
            }
            None => None,
        };
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
            initial_admin,
            public,
            registry,
        })
    }
}
