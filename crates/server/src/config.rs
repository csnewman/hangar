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
    /// What seals secrets at rest (`HANGAR_SECRET_KEY_FILE`); without it,
    /// people cannot keep SSH keys.
    pub sealer: Option<std::sync::Arc<crate::seal::Sealer>>,
    /// The SSH gateway: where it listens (`HANGAR_SSH_LISTEN`), and where
    /// people reach it, host and port.
    pub ssh: Option<Ssh>,
    /// Where copies of packs keep their files (`HANGAR_FILES_DIR`), which
    /// workers serve environments.
    pub files: PathBuf,
}

/// The SSH gateway's addresses.
#[derive(Debug, Clone)]
pub struct Ssh {
    pub listen: String,
    pub host: String,
    pub port: u16,
}

/// Where people reach the SSH gateway: address as given
/// (`HANGAR_SSH_ADDRESS`), or the public URL's host at the port it listens
/// on.
fn ssh_address(
    address: Option<String>,
    listen: &str,
    public: Option<&Public>,
) -> Result<(String, u16), String> {
    let address = match address {
        Some(a) => a,
        None => {
            let (_, port) = listen
                .rsplit_once(':')
                .ok_or_else(|| format!("the SSH listen address {listen:?} has no port"))?;
            let host = public.map_or("localhost", |p| p.hostname());
            format!("{host}:{port}")
        }
    };
    let (host, port) = address
        .rsplit_once(':')
        .ok_or_else(|| format!("the SSH address {address:?} has no port"))?;
    let port: u16 = port
        .parse()
        .ok()
        .filter(|p| *p > 0)
        .ok_or_else(|| format!("the SSH address {address:?} has no valid port"))?;
    Ok((
        host.trim_start_matches('[')
            .trim_end_matches(']')
            .to_string(),
        port,
    ))
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
        let sealer = var("HANGAR_SECRET_KEY_FILE")
            .map(|f| crate::seal::Sealer::load(&f).map(std::sync::Arc::new))
            .transpose()?;
        let ssh = match var("HANGAR_SSH_LISTEN") {
            Some(l) => {
                let (host, port) = ssh_address(var("HANGAR_SSH_ADDRESS"), &l, public.as_ref())?;
                let listen = match l.strip_prefix(':') {
                    Some(port) => format!("0.0.0.0:{port}"),
                    None => l,
                };
                Some(Ssh { listen, host, port })
            }
            None => None,
        };
        Ok(Config {
            listen,
            storage,
            bootstrap_token,
            initial_admin,
            public,
            registry,
            sealer,
            ssh,
            files: var("HANGAR_FILES_DIR")
                .unwrap_or_else(|| "/var/lib/hangar/files".into())
                .into(),
        })
    }
}
