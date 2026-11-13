//! The worker API. Every request is one the worker opens; the server never
//! needs a route to the worker.

use std::fmt;
use std::io::Read;
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use hangar_api::{DesiredSet, EnvironmentFiles, RegisterWorker, WorkerCredential, WorkerStatus};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::config::Config;

/// Longer than the server holds a request for the desired set, so a held
/// request is never cut off from this end.
const TIMEOUT: Duration = Duration::from_secs(60);

/// Why a request failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The server refused a credential: the bootstrap token is wrong, or
    /// this worker's own was revoked or removed. Retrying cannot fix it.
    Rejected,
    Failed(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Rejected => f.write_str("the server rejected the credential"),
            Error::Failed(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for Error {}

fn failed(e: impl fmt::Display) -> Error {
    Error::Failed(e.to_string())
}

/// Speaks the worker API to one server.
#[derive(Clone)]
pub struct Client {
    base: String,
    credential: Arc<RwLock<String>>,
    agent: ureq::Agent,
}

impl Client {
    pub fn new(base: &str) -> Client {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(TIMEOUT))
            .user_agent(concat!("hangar-worker/", env!("CARGO_PKG_VERSION")))
            .build()
            .new_agent();
        Client {
            base: base.trim_end_matches('/').to_string(),
            credential: Arc::default(),
            agent,
        }
    }

    /// The server this client speaks to.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// The worker's own credential, once it has one.
    pub fn credential(&self) -> String {
        self.credential.read().unwrap().clone()
    }

    /// Reads the worker's credential, or exchanges the bootstrap token for
    /// one and saves it if there is none yet.
    pub async fn load_or_register(&self, cfg: &Config) -> Result<(), Error> {
        let path = Path::new(&cfg.server.credential_file);
        match std::fs::read_to_string(path) {
            Ok(c) => {
                *self.credential.write().unwrap() = c.trim().to_string();
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(failed(e)),
        }
        if cfg.server.token_file.is_empty() {
            return Err(Error::Failed(format!(
                "no credential at {} and no server.token_file to register with",
                path.display()
            )));
        }
        let token = std::fs::read_to_string(&cfg.server.token_file)
            .map_err(|e| Error::Failed(format!("reading the bootstrap token: {e}")))?;
        let req = RegisterWorker {
            name: cfg.node.name.clone(),
            labels: cfg.node.labels.clone(),
        };
        let cred: WorkerCredential = self
            .call("POST", "/api/worker/v1/register", token.trim(), Some(&req))
            .await
            .map_err(|e| match e {
                Error::Rejected => Error::Rejected,
                Error::Failed(why) => {
                    Error::Failed(format!("registering as {}: {why}", cfg.node.name))
                }
            })?;

        // Written beside its final name and renamed, so a crash leaves
        // either no credential or a whole one.
        if let Some(dir) = path.parent() {
            let mut b = std::fs::DirBuilder::new();
            b.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                b.mode(0o700);
            }
            b.create(dir).map_err(failed)?;
        }
        let tmp = std::path::PathBuf::from(format!("{}.tmp", cfg.server.credential_file));
        write_private(&tmp, format!("{}\n", cred.credential).as_bytes()).map_err(failed)?;
        std::fs::rename(&tmp, path).map_err(failed)?;
        *self.credential.write().unwrap() = cred.credential;
        Ok(())
    }

    /// Waits for a desired set newer than after. The server answers at once
    /// if there is one, and otherwise holds the request until there is or
    /// until its own timeout, when it answers with the current set
    /// regardless.
    pub async fn desired(&self, after: i64) -> Result<DesiredSet, Error> {
        self.call(
            "GET",
            &format!("/api/worker/v1/desired?after={after}"),
            &self.credential(),
            None::<&()>,
        )
        .await
    }

    /// What of the shared files an environment may reach.
    pub async fn environment_files(&self, id: &str) -> Result<EnvironmentFiles, Error> {
        self.call(
            "GET",
            &format!("/api/worker/v1/environments/{}/files", path_escape(id)),
            &self.credential(),
            None::<&()>,
        )
        .await
    }

    pub async fn report(&self, st: &WorkerStatus) -> Result<(), Error> {
        let _: Option<serde_json::Value> = self
            .call("PUT", "/api/worker/v1/status", &self.credential(), Some(st))
            .await?;
        Ok(())
    }

    async fn call<B: Serialize, T: DeserializeOwned + Send + 'static>(
        &self,
        method: &'static str,
        path: &str,
        token: &str,
        body: Option<&B>,
    ) -> Result<T, Error> {
        let body = body.map(serde_json::to_vec).transpose().map_err(failed)?;
        let agent = self.agent.clone();
        let url = format!("{}{path}", self.base);
        let path = path.to_string();
        let token = token.to_string();
        tokio::task::spawn_blocking(move || {
            let bytes = request(&agent, method, &url, &token, body).map_err(|e| match e {
                Error::Failed(why) => Error::Failed(format!("{method} {path}: {why}")),
                e => e,
            })?;
            if bytes.is_empty() {
                return serde_json::from_slice(b"null").map_err(failed);
            }
            serde_json::from_slice(&bytes)
                .map_err(|e| Error::Failed(format!("{method} {path}: {e}")))
        })
        .await
        .map_err(failed)?
    }
}

fn request(
    agent: &ureq::Agent,
    method: &str,
    url: &str,
    token: &str,
    body: Option<Vec<u8>>,
) -> Result<Vec<u8>, Error> {
    let mut req = ureq::http::Request::builder()
        .method(method)
        .uri(url)
        .header("Authorization", format!("Bearer {token}"));
    if body.is_some() {
        req = req.header("Content-Type", "application/json");
    }
    let resp = match body {
        Some(b) => agent.run(req.body(b).map_err(failed)?),
        None => agent.run(req.body(()).map_err(failed)?),
    }
    .map_err(failed)?;
    let status = resp.status();
    if status == ureq::http::StatusCode::UNAUTHORIZED {
        return Err(Error::Rejected);
    }
    let mut bytes = Vec::new();
    let limit = if status.as_u16() >= 300 {
        64 << 10
    } else {
        256 << 20
    };
    resp.into_body()
        .into_reader()
        .take(limit)
        .read_to_end(&mut bytes)
        .map_err(failed)?;
    if status.as_u16() >= 300 {
        if let Ok(e) = serde_json::from_slice::<hangar_api::Error>(&bytes)
            && !e.error.is_empty()
        {
            return Err(Error::Failed(e.error));
        }
        return Err(Error::Failed(format!(
            "{} {}",
            status.as_u16(),
            status.canonical_reason().unwrap_or("")
        )));
    }
    Ok(bytes)
}

/// Escapes a path segment, as Go's url.PathEscape does.
fn path_escape(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(path)?;
    f.write_all(data)?;
    f.sync_all()
}
