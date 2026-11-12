//! Hangar's registry: Docker's credential helper for it, which asks the
//! server for a credential each time it is used: one good for an hour, as
//! the environment's owner, which nothing stores.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::net::UnixListener;

use super::files::Files;
use super::{Guest, Message, TYPE_GET_CREDENTIAL, warn};

/// The name Docker runs the credential helper by,
/// `docker-credential-<name>`, and the one the Docker configuration names
/// for Hangar's registry.
pub const HELPER: &str = "hangar";

/// Where the credential helper asks the agent for credentials.
pub const SOCKET: &str = "/run/hangar/registry.sock";

/// How long the agent waits for a session to ask through.
const CREDENTIAL_WAIT: Duration = Duration::from_secs(30);

/// A credential as a Docker credential helper gives it, or why there is
/// none.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct HelperCredential {
    #[serde(
        rename = "ServerURL",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub server_url: String,
    #[serde(rename = "Username", default, skip_serializing_if = "String::is_empty")]
    pub username: String,
    #[serde(rename = "Secret", default, skip_serializing_if = "String::is_empty")]
    pub secret: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
}

impl<F: Files> Guest<F> {
    /// Has Docker use the credential helper for Hangar's registry, for the
    /// user and for root, so docker push and sudo docker push both reach it
    /// as the environment's owner.
    pub(super) fn set_registry(&self, host: &str) {
        for (home, uid, gid) in &self.docker_homes {
            let path = home.join(".docker").join("config.json");
            if let Err(e) = use_helper(&path, host, *uid, *gid) {
                warn(&format!(
                    "configuring docker in {} for Hangar's registry: {e}",
                    home.display()
                ));
            }
        }
    }

    /// Serves, on `socket`, credentials for Hangar's registry to the
    /// credential helper: it writes the registry's host, and is answered
    /// with the Docker helper's JSON for it, or an error.
    pub async fn serve_credentials(self: &Arc<Self>, socket: &Path) -> io::Result<()> {
        if let Some(dir) = socket.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let _ = std::fs::remove_file(socket);
        let ln = UnixListener::bind(socket)?;
        std::os::unix::fs::lchown(socket, Some(self.user.uid), Some(self.user.gid))?;
        std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
        loop {
            let (conn, _) = ln.accept().await?;
            let g = self.clone();
            tokio::spawn(tokio::time::timeout(
                CREDENTIAL_WAIT + Duration::from_secs(35),
                async move {
                    let (rd, mut wr) = conn.into_split();
                    let mut host = String::new();
                    if tokio::io::BufReader::new(rd)
                        .read_line(&mut host)
                        .await
                        .is_err()
                    {
                        return;
                    }
                    let c = g.credential(host.trim()).await;
                    let mut b = serde_json::to_vec(&c).unwrap_or_default();
                    b.push(b'\n');
                    let _ = wr.write_all(&b).await;
                },
            ));
        }
    }

    async fn credential(&self, host: &str) -> HelperCredential {
        let failed = |error: String| HelperCredential {
            error,
            ..HelperCredential::default()
        };
        let Some(s) = self.connected(CREDENTIAL_WAIT).await else {
            return failed("not connected to Hangar".into());
        };
        let ask = Message {
            kind: TYPE_GET_CREDENTIAL.into(),
            ..Message::default()
        };
        match s.ask(ask).await {
            Err(e) => failed(e),
            Ok(r) if !r.host.eq_ignore_ascii_case(host) => {
                failed(format!("Hangar's registry is {}, not {host}", r.host))
            }
            Ok(r) => HelperCredential {
                server_url: host.to_string(),
                username: r.username,
                secret: r.secret,
                error: String::new(),
            },
        }
    }
}

/// Names the credential helper for `host` in a Docker configuration,
/// keeping the rest of it as it is.
fn use_helper(path: &Path, host: &str, uid: u32, gid: u32) -> io::Result<()> {
    let mut cfg: Map<String, Value> = match std::fs::read(path) {
        Ok(b) => serde_json::from_slice(&b)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Map::new(),
        Err(e) => return Err(e),
    };
    let helpers = cfg
        .entry("credHelpers")
        .or_insert_with(|| Value::Object(Map::new()));
    if !helpers.is_object() {
        *helpers = Value::Object(Map::new());
    }
    let helpers = helpers.as_object_mut().unwrap();
    if helpers.get(host).and_then(Value::as_str) == Some(HELPER) {
        return Ok(());
    }
    helpers.insert(host.to_string(), HELPER.into());
    let mut out = vec![];
    let mut ser = serde_json::Serializer::with_formatter(
        &mut out,
        serde_json::ser::PrettyFormatter::with_indent(b"\t"),
    );
    cfg.serialize(&mut ser)?;
    out.push(b'\n');
    let dir = path.parent().unwrap_or(Path::new("/"));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    std::os::unix::fs::lchown(dir, Some(uid), Some(gid))?;
    let mut tmp = PathBuf::from(path);
    tmp.as_mut_os_string().push(".hangar");
    let written = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(&out)?;
        drop(f);
        std::os::unix::fs::lchown(&tmp, Some(uid), Some(gid))?;
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// What docker takes as having no credentials, and pulls anonymously.
const NOT_FOUND: &str = "credentials not found in native keychain";

/// Docker's credential helper protocol for Hangar's registry, run by docker
/// as `docker-credential-hangar`: get asks the agent on `socket` for a
/// credential for the host on standard input; store and erase have nothing
/// to do, since Hangar gives the credentials; list names no stored ones.
pub fn credential_helper(args: &[String], socket: &Path) -> ExitCode {
    let [action] = args else {
        eprintln!("usage: docker-credential-hangar get|store|erase|list");
        return ExitCode::from(2);
    };
    let mut input = String::new();
    let _ = io::stdin().read_to_string(&mut input);
    match action.as_str() {
        "get" => {
            let host = input.trim();
            let host = host
                .strip_prefix("https://")
                .or_else(|| host.strip_prefix("http://"))
                .unwrap_or(host);
            match ask_credential(socket, host) {
                Ok(c) if c.error.is_empty() => {
                    println!("{}", serde_json::to_string(&c).unwrap_or_default());
                    ExitCode::SUCCESS
                }
                Ok(c) => {
                    eprintln!("hangar: {}", c.error);
                    println!("{NOT_FOUND}");
                    ExitCode::FAILURE
                }
                Err(_) => {
                    println!("{NOT_FOUND}");
                    ExitCode::FAILURE
                }
            }
        }
        "store" => {
            println!("Hangar signs this environment in to its registry itself: no login is needed");
            ExitCode::FAILURE
        }
        "erase" => ExitCode::SUCCESS,
        "list" => {
            println!("{{}}");
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("unknown action {other}");
            ExitCode::from(2)
        }
    }
}

fn ask_credential(socket: &Path, host: &str) -> io::Result<HelperCredential> {
    let mut conn = std::os::unix::net::UnixStream::connect(socket)?;
    writeln!(conn, "{host}")?;
    let mut line = vec![];
    BufReader::new(conn).read_until(b'\n', &mut line)?;
    Ok(serde_json::from_slice(&line)?)
}
