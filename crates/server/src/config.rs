//! The server's configuration: its flags and its environment. A setting
//! it does not read is passed over, not refused.

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
    /// Where the registry's blobs are kept, when it runs.
    pub blobs: Option<crate::blob::Config>,
    /// What faces the internet directly, each part only when configured.
    pub edge: Edge,
    /// For development only: every visitor to the web UI without a session
    /// is signed in as this person (`HANGAR_DEV_AUTO_SIGN_IN`).
    pub auto_sign_in: Option<String>,
    /// The release workers that change their own run
    /// (`HANGAR_WORKER_RELEASE`, the server's own build unless set; empty
    /// for none), from the image repository `HANGAR_RELEASE_ISO_IMAGE`
    /// (Hangar's published one unless set) tagged with it.
    pub worker_release: Option<hangar_api::Release>,
}

/// Where Hangar's release ISOs are published, an image per release tagged
/// with its version.
pub const RELEASE_ISO_IMAGE: &str = "ghcr.io/csnewman/hangar/release-iso";

/// The release workers run unless HANGAR_WORKER_RELEASE says: this build's,
/// when it was built as a release is (with HANGAR_VERSION).
const OWN_RELEASE: Option<&str> = option_env!("HANGAR_VERSION");

/// What faces the internet directly: the zone's DNS server
/// (`HANGAR_DNS_LISTEN`), HTTPS (`HANGAR_TLS_LISTEN`), and a redirect from
/// HTTP to it (`HANGAR_REDIRECT_LISTEN`). The zone is the public URL's host.
#[derive(Debug, Clone, Default)]
pub struct Edge {
    pub dns: Option<Dns>,
    pub tls: Option<Tls>,
    pub redirect: Option<String>,
}

/// The zone's DNS server.
#[derive(Debug, Clone)]
pub struct Dns {
    pub listen: String,
    /// Where every name in the zone points (`HANGAR_DNS_ADDRESSES`).
    pub addresses: Vec<std::net::IpAddr>,
    /// The zone's nameservers as the parent's delegation names them
    /// (`HANGAR_DNS_NAMESERVERS`; ns1.<zone> unless set).
    pub nameservers: Vec<String>,
    /// The SOA's responsible mailbox, as an address
    /// (`HANGAR_DNS_HOSTMASTER`; hostmaster@<zone> unless set).
    pub hostmaster: String,
}

/// HTTPS, and the certificates it serves.
#[derive(Debug, Clone)]
pub struct Tls {
    pub listen: String,
    pub certs: Certs,
}

#[derive(Debug, Clone)]
pub enum Certs {
    /// A certificate and key someone else issues and renews
    /// (`HANGAR_TLS_CERT_FILE`, `HANGAR_TLS_KEY_FILE`).
    Files { cert: PathBuf, key: PathBuf },
    /// Certificates an ACME CA issues for the zone and every name under
    /// it, against DNS-01 challenges the zone answers.
    Acme {
        /// The account's contact (`HANGAR_ACME_EMAIL`).
        email: Option<String>,
        /// The directory (`HANGAR_ACME_CA`; Let's Encrypt unless set).
        ca: String,
        /// PEM roots to trust for the CA itself (`HANGAR_ACME_CA_ROOTS`).
        roots: Option<PathBuf>,
    },
}

/// Let's Encrypt's production directory.
const LETS_ENCRYPT: &str = "https://acme-v02.api.letsencrypt.org/directory";

/// An address to listen on, where `:port` is every interface's.
fn listen_addr(l: String) -> String {
    match l.strip_prefix(':') {
        Some(port) => format!("{}:{port}", crate::listen::ANY),
        None => l,
    }
}

/// A list of values separated by commas or spaces.
fn split_list(s: &str) -> Vec<String> {
    s.split([',', ' '])
        .filter(|v| !v.is_empty())
        .map(String::from)
        .collect()
}

fn edge(var: &dyn Fn(&str) -> Option<String>, public: Option<&Public>) -> Result<Edge, String> {
    let dns = var("HANGAR_DNS_LISTEN");
    let tls = var("HANGAR_TLS_LISTEN");
    let redirect = var("HANGAR_REDIRECT_LISTEN");
    let (cert, key) = (var("HANGAR_TLS_CERT_FILE"), var("HANGAR_TLS_KEY_FILE"));
    if (cert.is_some() || key.is_some()) && tls.is_none() {
        return Err("HANGAR_TLS_CERT_FILE and HANGAR_TLS_KEY_FILE are served by HANGAR_TLS_LISTEN, which is not set".into());
    }
    if cert.is_some() != key.is_some() {
        return Err("HANGAR_TLS_CERT_FILE and HANGAR_TLS_KEY_FILE are set together: the certificate and its key".into());
    }
    if dns.is_none() && tls.is_none() && redirect.is_none() {
        return Ok(Edge::default());
    }
    let Some(public) = public else {
        return Err("HANGAR_DNS_LISTEN, HANGAR_TLS_LISTEN and HANGAR_REDIRECT_LISTEN need HANGAR_PUBLIC_URL, whose host is the zone".into());
    };
    let zone = crate::zone::canonical(public.hostname());
    let prefix = public.style == Style::Prefix;
    let dns = match dns {
        Some(_) if prefix => {
            return Err("HANGAR_DNS_LISTEN serves a zone under the public host, and prefix host names (HANGAR_HOST_STYLE) are outside it, in a domain whose own DNS already answers for them; leave it unset".into());
        }
        Some(listen) => {
            if zone.parse::<std::net::IpAddr>().is_ok() {
                return Err(format!("the DNS zone must be a name, not {zone:?}"));
            }
            let addresses = split_list(&var("HANGAR_DNS_ADDRESSES").unwrap_or_default())
                .iter()
                .map(|a| {
                    a.parse()
                        .map_err(|_| format!("HANGAR_DNS_ADDRESSES: {a:?} is not an IP address"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            if addresses.is_empty() {
                return Err("the DNS zone needs at least one address for its names to point at (HANGAR_DNS_ADDRESSES)".into());
            }
            let mut nameservers: Vec<String> =
                split_list(&var("HANGAR_DNS_NAMESERVERS").unwrap_or_default())
                    .iter()
                    .map(|n| crate::zone::canonical(n))
                    .collect();
            if nameservers.is_empty() {
                nameservers.push(format!("ns1.{zone}"));
            }
            Some(Dns {
                listen: listen_addr(listen),
                addresses,
                nameservers,
                hostmaster: var("HANGAR_DNS_HOSTMASTER")
                    .unwrap_or_else(|| format!("hostmaster@{zone}")),
            })
        }
        None => None,
    };
    let tls = match tls {
        Some(listen) => {
            if !public.secure() {
                return Err("HANGAR_TLS_LISTEN needs an https HANGAR_PUBLIC_URL".into());
            }
            let certs = match (cert, key) {
                (Some(cert), Some(key)) => Certs::Files {
                    cert: cert.into(),
                    key: key.into(),
                },
                _ if prefix => {
                    return Err("HTTPS for prefix host names (HANGAR_HOST_STYLE) needs a certificate that covers them, such as one for *.<domain>, in HANGAR_TLS_CERT_FILE and HANGAR_TLS_KEY_FILE: ACME certificates are only issued for the zone Hangar serves".into());
                }
                _ => Certs::Acme {
                    email: var("HANGAR_ACME_EMAIL"),
                    ca: var("HANGAR_ACME_CA").unwrap_or_else(|| LETS_ENCRYPT.into()),
                    roots: var("HANGAR_ACME_CA_ROOTS").map(PathBuf::from),
                },
            };
            Some(Tls {
                listen: listen_addr(listen),
                certs,
            })
        }
        None => None,
    };
    Ok(Edge {
        dns,
        tls,
        redirect: redirect.map(listen_addr),
    })
}

/// The blob store: HANGAR_BLOB_URL, its keys there or in
/// HANGAR_BLOB_ACCESS_KEY and HANGAR_BLOB_SECRET_KEY(_FILE), and without a
/// secret key the one derived from the server's own: the SHA-256, in hex,
/// of "hangar-blob-access\n" and the key file's contents, trimmed, as the
/// compose file derives it for RustFS. A server keeping its state in a
/// file keeps its blobs in a directory beside it unless told otherwise.
fn blob_config(
    var: &dyn Fn(&str) -> Option<String>,
    storage: &Storage,
) -> Result<crate::blob::Config, String> {
    let url = match (var("HANGAR_BLOB_URL"), storage) {
        (Some(u), _) => u,
        (None, Storage::Json(state)) => {
            format!("file://{}", state.with_extension("blobs").display())
        }
        (None, Storage::Postgres(_)) => {
            return Err("HANGAR_BLOB_URL is not set: the server keeps files in an S3-compatible object store".into());
        }
    };
    let mut secret = var("HANGAR_BLOB_SECRET_KEY");
    if let Some(f) = var("HANGAR_BLOB_SECRET_KEY_FILE") {
        secret = Some(
            std::fs::read_to_string(&f)
                .map_err(|e| format!("HANGAR_BLOB_SECRET_KEY_FILE: {e}"))?
                .trim()
                .to_string(),
        );
    }
    let in_url = url::Url::parse(&url)
        .ok()
        .and_then(|u| u.password().map(str::to_string))
        .is_some_and(|p| !p.is_empty());
    if secret.is_none() && !in_url && !url.starts_with("file://") {
        let f = var("HANGAR_SECRET_KEY_FILE").ok_or(
            "no secret key for the object store: HANGAR_BLOB_SECRET_KEY, or HANGAR_SECRET_KEY_FILE to derive it from",
        )?;
        let key =
            std::fs::read_to_string(&f).map_err(|e| format!("HANGAR_SECRET_KEY_FILE: {e}"))?;
        use sha2::Digest;
        let sum = sha2::Sha256::digest(format!("hangar-blob-access\n{}", key.trim()).as_bytes());
        secret = Some(sum.iter().map(|b| format!("{b:02x}")).collect());
    }
    Ok(crate::blob::Config {
        url,
        access: var("HANGAR_BLOB_ACCESS_KEY"),
        secret,
        region: var("HANGAR_BLOB_REGION").unwrap_or_default(),
        encrypt: var("HANGAR_BLOB_ENCRYPT").as_deref() == Some("true"),
    })
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

/// The server's flags, as `-name value`, `-name=value` or `--name`, each
/// in place of the setting it names.
const FLAGS: &[(&str, &str, &str)] = &[
    ("listen", "HANGAR_LISTEN", "address to serve on"),
    ("database", "HANGAR_DATABASE_URL", "Postgres connection URL"),
    (
        "bootstrap-token-file",
        "HANGAR_BOOTSTRAP_TOKEN_FILE",
        "file holding the token workers present to register",
    ),
];

/// What the command line asks for besides serving.
pub enum Command {
    Serve(Config),
    /// Bring the schema up to date, then exit without serving
    /// (`-migrate-only`).
    Migrate(Config),
    /// Print the flags (`-h`).
    Help(String),
}

fn usage() -> String {
    let mut out = String::from("Usage of hangar-server:\n");
    for (name, env, what) in FLAGS {
        out.push_str(&format!("  -{name} string\n    \t{what} (or {env})\n"));
    }
    out.push_str("  -migrate-only\n    \tbring the schema up to date, then exit without serving\n");
    out
}

impl Config {
    /// Reads the configuration from the command line's flags and the
    /// environment.
    pub fn from_args(args: &[String]) -> Result<Command, String> {
        let mut given = std::collections::HashMap::new();
        let mut migrate = false;
        let mut i = 0;
        while i < args.len() {
            let a = args[i].trim_start_matches('-');
            if a.len() == args[i].len() {
                return Err(format!("unexpected argument {:?}\n{}", args[i], usage()));
            }
            let (name, value) = match a.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (a, None),
            };
            match name {
                "h" | "help" => return Ok(Command::Help(usage())),
                "migrate-only" => migrate = value.is_none_or(|v| v == "true"),
                _ => {
                    let Some((_, env, _)) = FLAGS.iter().find(|(n, _, _)| *n == name) else {
                        return Err(format!(
                            "flag provided but not defined: -{name}\n{}",
                            usage()
                        ));
                    };
                    let v = match value {
                        Some(v) => v,
                        None => {
                            i += 1;
                            args.get(i).cloned().ok_or_else(|| {
                                format!("flag needs an argument: -{name}\n{}", usage())
                            })?
                        }
                    };
                    given.insert(*env, v);
                }
            }
            i += 1;
        }
        let cfg = Config::read(&|k: &str| {
            given
                .get(k)
                .cloned()
                .or_else(|| std::env::var(k).ok())
                .filter(|v| !v.is_empty())
        })?;
        Ok(if migrate {
            Command::Migrate(cfg)
        } else {
            Command::Serve(cfg)
        })
    }

    fn read(var: &dyn Fn(&str) -> Option<String>) -> Result<Config, String> {
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
        let listen = listen_addr(var("HANGAR_LISTEN").unwrap_or_else(|| ":8081".into()));
        let registry =
            public.is_some() && var("HANGAR_REGISTRY_ENABLED").as_deref() != Some("false");
        let sealer = var("HANGAR_SECRET_KEY_FILE")
            .map(|f| crate::seal::Sealer::load(&f).map(std::sync::Arc::new))
            .transpose()?;
        let ssh = match var("HANGAR_SSH_LISTEN") {
            Some(l) => {
                let (host, port) = ssh_address(var("HANGAR_SSH_ADDRESS"), &l, public.as_ref())?;
                Some(Ssh {
                    listen: listen_addr(l),
                    host,
                    port,
                })
            }
            None => None,
        };
        let blobs = if registry {
            Some(blob_config(var, &storage)?)
        } else {
            None
        };
        let edge = edge(var, public.as_ref())?;
        let worker_release = match var("HANGAR_WORKER_RELEASE").or(OWN_RELEASE.map(String::from)) {
            Some(v) if !v.trim().is_empty() => Some(hangar_api::Release {
                iso_image: format!(
                    "{}:{}",
                    var("HANGAR_RELEASE_ISO_IMAGE")
                        .as_deref()
                        .unwrap_or(RELEASE_ISO_IMAGE)
                        .trim_end_matches('/'),
                    v.trim()
                ),
                version: v.trim().to_string(),
            }),
            _ => None,
        };
        Ok(Config {
            edge,
            worker_release,
            auto_sign_in: var("HANGAR_DEV_AUTO_SIGN_IN"),
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
            blobs,
        })
    }
}
