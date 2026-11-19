//! A Hangar of one's own on a Proxmox: one VM holding Hangar's server and
//! worker, booted from the release ISO with a data disk for its state, its
//! environments VMs beside it. Made with an API token alone: the release ISO
//! uploaded under the Hangar's prefix, the VM made with Proxmox's firewall
//! letting in what it serves, and its settings given through the guest agent
//! channel, so nothing secret is in the VM's config. Whoever makes it keeps
//! the Hangar's first access token (the Hangar is given only its hash) and
//! pins its certificate: one made for its address here, or with a host name
//! (`Web`), the certificate given for it, which then runs a registry too.
//!
//! Several Hangars may share a Proxmox: each names everything it makes after
//! its prefix (`check_prefix`), and touches nothing else.

use std::collections::BTreeSet;
use std::io;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::client::{Client, enc, host_port};
use crate::err;

/// Where Hangar's release ISOs are published: an image per release, tagged
/// with its version and latest, holding RELEASE_ISO_FILE.
pub const RELEASE_ISO_IMAGE: &str = "ghcr.io/csnewman/hangar/release-iso";
pub const RELEASE_ISO_FILE: &str = "hangar-release.iso";

/// The label a release image says its version with.
const VERSION_LABEL: &str = "org.opencontainers.image.version";

/// The tag every VM of Hangar's carries, beside its Hangar's prefix; the
/// Hangar's own VM carries WORKER_TAG too.
const TAG: &str = "hangar";
const WORKER_TAG: &str = "hangar-worker";
/// The tag an environment's VM carries beside them.
const ENV_TAG: &str = "hangar-env";

/// The kinds of ISO a Hangar keeps, which their names say after its prefix.
const ISO_KINDS: [&str; 2] = ["release", "image"];

/// What a Hangar's VM serves: the hub its environments dial, HTTPS and the
/// SSH gateway.
const PORTS: [(u16, &str); 3] = [
    (7443, "Hangar: its environments dial in"),
    (443, "Hangar: HTTPS"),
    (22, "Hangar: the SSH gateway"),
];

/// Whether prefix can name a Hangar's things on Proxmox: 1 to 20 lowercase
/// letters, digits and hyphens, starting with a letter and not ending with
/// a hyphen, which VM names, tags and storage file names all take. Words of
/// its own between hyphens may not be the kinds of ISO (release, image): a
/// prefix of P-image's ISOs would be named as P's image ISOs are, and P's
/// worker would take them for its own.
pub fn check_prefix(prefix: &str) -> Result<(), String> {
    let ok = (1..=20).contains(&prefix.len())
        && prefix.starts_with(|c: char| c.is_ascii_lowercase())
        && !prefix.ends_with('-')
        && prefix
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !ok {
        return Err(format!(
            "proxmox prefix {prefix:?}: must be 1-20 lowercase letters, digits or hyphens, \
             start with a letter and not end with a hyphen"
        ));
    }
    if prefix.split('-').any(|w| ISO_KINDS.contains(&w)) {
        return Err(format!(
            "proxmox prefix {prefix:?}: must not contain the word {}",
            ISO_KINDS.join(" or ")
        ));
    }
    Ok(())
}

/// Whether name is one of the ISOs a Hangar with prefix keeps.
pub fn is_own_iso(prefix: &str, name: &str) -> bool {
    ISO_KINDS
        .iter()
        .any(|k| name.starts_with(&format!("{prefix}-{k}-")))
        && name.ends_with(".iso")
}

/// A prefix made of a name: lower case, its runs of anything else a
/// hyphen, cut to fit, and a letter first.
pub fn prefix_of(name: &str) -> String {
    let mut p = String::new();
    for c in name.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            p.push(c);
        } else if !p.is_empty() && !p.ends_with('-') {
            p.push('-');
        }
    }
    let mut p: String = p
        .split('-')
        .filter(|w| !w.is_empty() && !ISO_KINDS.contains(w))
        .collect::<Vec<_>>()
        .join("-");
    if !p.starts_with(|c: char| c.is_ascii_lowercase()) {
        p.insert(0, 'h');
    }
    p.truncate(20);
    p.trim_end_matches('-').to_string()
}

/// What a release's ISO is named by: a version that is a commit's hash, as
/// published releases' are, by its first 12 digits; another made of what a
/// file name takes, as it is; anything else by its hash.
pub fn release_key(version: &str) -> String {
    let v = version.to_ascii_lowercase();
    if v.len() >= 12 && v.bytes().all(|b| b.is_ascii_hexdigit()) {
        return v[..12].to_string();
    }
    if (1..=32).contains(&v.len())
        && v.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
    {
        return v;
    }
    short_hash(version)
}

/// The first 12 hex digits of s's SHA-256.
pub fn short_hash(s: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, s.as_bytes())
        .as_ref()
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A certificate's SHA-256 as Proxmox shows one: upper-case hex in pairs
/// with colons. Taken in any form `hangar_hub::tls::pinned` takes.
pub fn show_fingerprint(fingerprint: &str) -> String {
    let hex: String = fingerprint
        .chars()
        .filter(char::is_ascii_hexdigit)
        .collect::<String>()
        .to_ascii_uppercase();
    hex.as_bytes()
        .chunks(2)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join(":")
}

/// The certificate the server at url (https://host:port) presents, by its
/// SHA-256 (lower-case hex), taken without checking: for a person to match
/// with what Proxmox shows of its own before it is pinned.
pub async fn certificate(url: &str) -> io::Result<String> {
    Ok(hangar_hub::tls::fingerprint(&presented(url).await?))
}

/// The certificate the server at url presents, DER, taken without checking.
pub async fn presented(url: &str) -> io::Result<rustls::pki_types::CertificateDer<'static>> {
    let (host, port) = host_port(url)?;
    let seen = Arc::new(std::sync::Mutex::new(None));
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(io::Error::other)?
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(Recorder(seen.clone())))
    .with_no_client_auth();
    let tcp = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::net::TcpStream::connect((host.as_str(), port)),
    )
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!("{host}:{port} did not answer"),
        )
    })??;
    let name = rustls::pki_types::ServerName::try_from(host.clone()).map_err(io::Error::other)?;
    // The handshake may fail after the certificate is seen (the recorder
    // takes any): what was seen is the answer.
    let _ = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_rustls::TlsConnector::from(Arc::new(config)).connect(name, tcp),
    )
    .await;
    let fp = seen.lock().unwrap().clone();
    fp.ok_or_else(|| err(format!("{host}:{port} presented no certificate")))
}

/// Takes any certificate, keeping it.
#[derive(Debug)]
struct Recorder(Arc<std::sync::Mutex<Option<rustls::pki_types::CertificateDer<'static>>>>);

impl rustls::client::danger::ServerCertVerifier for Recorder {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        *self.0.lock().unwrap() = Some(end_entity.clone().into_owned());
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// A release, as fetched to upload: its ISO and its version.
#[derive(Debug, Clone)]
pub struct Release {
    pub file: PathBuf,
    pub version: String,
}

/// The version of the release the image names, as its label says, else
/// its digest's, without fetching the ISO. It blocks.
pub fn release_version(image: &str) -> io::Result<String> {
    let r = hangar_images::Reference::parse(image)
        .map_err(|e| err(format!("release image {image}: {e}")))?;
    hangar_images::set_guest_arch("amd64");
    let images = hangar_images::Client::new(Default::default(), None);
    let digest = images.resolve(&r)?;
    let config = images.config(&r, &digest)?;
    Ok(version_of(&config, &digest))
}

fn version_of(config: &Value, digest: &str) -> String {
    config["config"]["Labels"][VERSION_LABEL]
        .as_str()
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| digest.trim_start_matches("sha256:")[..12].to_string())
}

/// Fetches the release image names (RELEASE_ISO_IMAGE:latest, say) into
/// dir: its ISO, as RELEASE_ISO_FILE there, and its version, from its label,
/// else its digest's. It blocks.
pub fn fetch_release(image: &str, dir: &Path) -> io::Result<Release> {
    let r = hangar_images::Reference::parse(image)
        .map_err(|e| err(format!("release image {image}: {e}")))?;
    hangar_images::set_guest_arch("amd64");
    let images = hangar_images::Client::new(Default::default(), None);
    let digest = images.resolve(&r)?;
    let config = images.config(&r, &digest)?;
    let version = version_of(&config, &digest);
    std::fs::create_dir_all(dir)?;
    let file = dir.join(RELEASE_ISO_FILE);
    let scratch = dir.join("blobs");
    let fetched = images.fetch_file(&r, &digest, RELEASE_ISO_FILE, &scratch, &file);
    let _ = std::fs::remove_dir_all(&scratch);
    fetched?;
    Ok(Release { file, version })
}

/// A Proxmox, as a Hangar is made on it: its API and the token.
#[derive(Clone)]
pub struct Proxmox {
    client: Client,
    url: String,
    token: String,
    fingerprint: String,
}

/// What there is on a Proxmox to make a Hangar with, as far as the token
/// may use it, and what the token lacks.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Choices {
    pub version: String,
    pub nodes: Vec<Node>,
    /// Pools the token may make and run VMs in, with how many VMs each has.
    pub pools: Vec<(String, usize)>,
    /// What a Hangar needs of the token, each said as it stands.
    pub needs: Vec<Need>,
    /// Prefixes other Hangars here use, as far as the token sees.
    pub prefixes: BTreeSet<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Node {
    pub name: String,
    /// Logical CPUs (threads), what a VM's vCPUs are scheduled on.
    pub cpus: u32,
    /// Physical cores, when the token may see them (Sys.Audit).
    #[serde(default)]
    pub cores: u32,
    pub memory_mib: u64,
    pub free_mib: u64,
    /// Storages for disks (images) and for ISOs the token may use there.
    pub disks: Vec<Storage>,
    pub isos: Vec<Storage>,
    /// Bridges the token may join VMs to.
    pub networks: Vec<Network>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Storage {
    pub id: String,
    /// Its type: dir, zfspool, lvmthin, rbd...
    pub kind: String,
    pub free_bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Network {
    pub name: String,
    /// bridge, OVSBridge, or vnet.
    pub kind: String,
    /// Its address, where the node has one on it.
    pub cidr: String,
}

/// Something a Hangar needs of the token, and whether it has it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Need {
    pub ok: bool,
    /// As it stands: what it may do, or what it lacks and where.
    pub says: String,
}

/// Where a Hangar goes, and what it is.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Plan {
    pub node: String,
    pub disks: String,
    pub isos: String,
    pub network: String,
    pub pool: String,
    pub prefix: String,
    /// Its first administrator's name.
    pub admin: String,
    pub cores: u32,
    pub memory_mib: u32,
    pub data_gib: u32,
    /// The host name it is reached at, and the certificate it serves there;
    /// none, its address and a certificate made for it.
    pub web: Option<Web>,
}

/// A host name a Hangar is reached at, which its network's DNS answers
/// for, how its other hosts are named after it, and the certificate it
/// serves for them all, which its maker was given: a chain and its key,
/// PEM. In the prefix style the others are siblings of host
/// (registry-<host>), so a certificate for *.<host's domain> covers them;
/// in the subdomain style they are under it (registry.<host>).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Web {
    pub host: String,
    /// prefix or subdomain.
    pub style: String,
    pub cert: String,
    /// Kept apart from the rest, as a secret.
    #[serde(skip_serializing, default)]
    pub key: String,
}

impl Web {
    /// The registry's name, as host and the style make it.
    pub fn registry(&self) -> String {
        if self.style == "prefix" {
            format!("registry-{}", self.host)
        } else {
            format!("registry.{}", self.host)
        }
    }

    /// Whether it can be served: host a DNS name, the style one of the two,
    /// the key the certificate's, and the certificate valid for host and
    /// the registry's name. Its certificate's SHA-256, to pin.
    pub fn check(&self) -> Result<String, String> {
        use rustls::pki_types::pem::PemObject;
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
        if !matches!(self.style.as_str(), "prefix" | "subdomain") {
            return Err(format!("{:?} is not a host style", self.style));
        }
        match ServerName::try_from(self.host.as_str()) {
            Ok(ServerName::DnsName(_)) if self.host.contains('.') => {}
            _ => return Err(format!("{:?} is not a host name", self.host)),
        }
        let chain: Vec<CertificateDer<'static>> =
            CertificateDer::pem_slice_iter(self.cert.as_bytes())
                .collect::<Result<_, _>>()
                .map_err(|e| format!("The certificate is not PEM: {e:?}"))?;
        let Some(leaf) = chain.first().cloned() else {
            return Err("The certificate file holds no certificate.".into());
        };
        let key = PrivateKeyDer::from_pem_slice(self.key.as_bytes())
            .map_err(|e| format!("The key is not a PEM private key: {e:?}"))?;
        let signer = rustls::crypto::ring::sign::any_supported_type(&key)
            .map_err(|e| format!("The key is not one HTTPS can use: {e}"))?;
        rustls::sign::CertifiedKey::new(chain, signer)
            .keys_match()
            .map_err(|_| "The key is not the certificate's.".to_string())?;
        let ee = webpki::EndEntityCert::try_from(&leaf)
            .map_err(|e| format!("The certificate cannot be read: {e}"))?;
        for name in [self.host.clone(), self.registry()] {
            let n = ServerName::try_from(name.clone()).map_err(|e| e.to_string())?;
            ee.verify_is_valid_for_subject_name(&n)
                .map_err(|_| format!("The certificate does not cover {name}."))?;
        }
        Ok(hangar_hub::tls::fingerprint(&leaf))
    }
}

/// A Hangar made on a Proxmox, as whoever made it keeps it: where it is,
/// and the secrets it was made with.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Made {
    pub node: String,
    pub vmid: u32,
    pub isos: String,
    pub pool: String,
    pub prefix: String,
    pub address: Ipv4Addr,
    /// Its address for people and the app: https://<host> with a host
    /// name, else https://<address>.
    pub url: String,
    /// Its certificate's SHA-256, to pin.
    pub fingerprint: String,
    /// Its first administrator's access token, kept apart from the rest.
    #[serde(skip_serializing, default)]
    pub token: String,
}

/// A Hangar of its own found on a Proxmox (`Proxmox::hangars`): where its
/// VM is, what it was made with, and the release it boots.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Found {
    pub node: String,
    pub vmid: u32,
    pub prefix: String,
    pub pool: String,
    pub disks: String,
    pub isos: String,
    pub network: String,
    pub cores: u32,
    pub memory_mib: u32,
    pub data_gib: u32,
    /// The release its VM boots, by its ISO's key (`release_key`); empty
    /// for an ISO not named as its prefix's releases are.
    pub release: String,
    pub running: bool,
    /// Its address, while it runs and its agent says.
    pub address: Option<Ipv4Addr>,
}

impl Found {
    /// The plan it was made with, for admin.
    pub fn plan(&self, admin: &str) -> Plan {
        Plan {
            node: self.node.clone(),
            disks: self.disks.clone(),
            isos: self.isos.clone(),
            network: self.network.clone(),
            pool: self.pool.clone(),
            prefix: self.prefix.clone(),
            admin: admin.to_string(),
            cores: self.cores,
            memory_mib: self.memory_mib,
            data_gib: self.data_gib,
            web: None,
        }
    }
}

/// How far making a Hangar has got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Uploading {
        name: String,
    },
    Making {
        vmid: u32,
    },
    Starting,
    Settings,
    Address,
    Answering {
        url: String,
    },
    /// Upgrading: the VM's CD-ROM is pointed at the release's ISO.
    Attaching {
        name: String,
    },
    /// Upgrading: the VM restarts into it.
    Restarting,
}

/// What the token needs on a pool, to make and run a Hangar's VMs there.
const POOL_PRIVS: [&str; 13] = [
    "VM.Allocate",
    "VM.Audit",
    "VM.Config.CDROM",
    "VM.Config.CPU",
    "VM.Config.Disk",
    "VM.Config.HWType",
    "VM.Config.Memory",
    "VM.Config.Network",
    "VM.Config.Options",
    "VM.Console",
    "VM.PowerMgmt",
    "Pool.Allocate",
    "Pool.Audit",
];

/// Any of these lets it write files in a VM through the guest agent.
const AGENT_PRIVS: [&str; 3] = [
    "VM.GuestAgent.FileWrite",
    "VM.GuestAgent.Unrestricted",
    "VM.Monitor",
];

impl Proxmox {
    /// The API at url (https://host:8006), with token (USER@REALM!ID=SECRET),
    /// taking only the certificate with fingerprint (`certificate`).
    pub fn new(url: &str, token: &str, fingerprint: &str) -> io::Result<Proxmox> {
        Ok(Proxmox {
            client: Client::new(url, token, Some(fingerprint))?,
            url: url.trim().trim_end_matches('/').to_string(),
            token: token.trim().to_string(),
            fingerprint: fingerprint.to_string(),
        })
    }

    fn vm(&self, node: &str, id: u32, rest: &str) -> String {
        format!("/nodes/{}/qemu/{id}/{rest}", enc(node))
    }

    /// The token's privileges on path, as Proxmox works them out.
    async fn privileges(&self, path: &str) -> io::Result<BTreeSet<String>> {
        let v = self
            .client
            .get(&format!("/access/permissions?path={}", enc(path)))
            .await?;
        Ok(v[path]
            .as_object()
            .map(|m| {
                m.iter()
                    .filter(|(_, on)| on.as_i64().unwrap_or(0) != 0 || on.as_bool() == Some(true))
                    .map(|(k, _)| k.clone())
                    .collect()
            })
            .unwrap_or_default())
    }

    /// What there is to make a Hangar with, and what the token lacks.
    pub async fn choices(&self) -> io::Result<Choices> {
        let version = self.client.get("/version").await?;
        let mut c = Choices {
            version: version["version"].as_str().unwrap_or_default().to_string(),
            ..Default::default()
        };
        let resources = self.client.get("/cluster/resources").await?;
        let resources: Vec<Value> = resources.as_array().cloned().unwrap_or_default();
        // Pools, with what the token may do in each.
        let mut lacking = String::new();
        for p in self
            .client
            .get("/pools")
            .await?
            .as_array()
            .into_iter()
            .flatten()
        {
            let Some(id) = p["poolid"].as_str() else {
                continue;
            };
            let has = self.privileges(&format!("/pool/{id}")).await?;
            let missing: Vec<&str> = POOL_PRIVS
                .iter()
                .copied()
                .filter(|p| !has.contains(*p))
                .chain((!AGENT_PRIVS.iter().any(|p| has.contains(*p))).then_some(AGENT_PRIVS[0]))
                .collect();
            if missing.is_empty() {
                let vms = resources
                    .iter()
                    .filter(|r| r["pool"] == id && r["type"] == "qemu")
                    .count();
                c.pools.push((id.to_string(), vms));
            } else if lacking.is_empty() {
                lacking = format!("{} on /pool/{id}", missing.join(", "));
            }
        }
        // Nodes, with their storages and networks the token may use.
        for n in resources.iter().filter(|r| r["type"] == "node") {
            let Some(name) = n["node"].as_str() else {
                continue;
            };
            if n["status"] != "online" {
                continue;
            }
            let audit = self
                .privileges(&format!("/nodes/{name}"))
                .await?
                .contains("Sys.Audit");
            let mut node = Node {
                name: name.to_string(),
                cpus: n["maxcpu"].as_u64().unwrap_or(0) as u32,
                memory_mib: n["maxmem"].as_u64().unwrap_or(0) >> 20,
                free_mib: n["maxmem"]
                    .as_u64()
                    .unwrap_or(0)
                    .saturating_sub(n["mem"].as_u64().unwrap_or(0))
                    >> 20,
                ..Default::default()
            };
            if audit
                && let Ok(st) = self
                    .client
                    .get(&format!("/nodes/{}/status", enc(name)))
                    .await
            {
                let cpu = &st["cpuinfo"];
                node.cores = (cpu["cores"].as_u64().unwrap_or(0)
                    * cpu["sockets"].as_u64().unwrap_or(1)) as u32;
            }
            let storages = self
                .client
                .get(&format!("/nodes/{}/storage?enabled=1", enc(name)))
                .await?;
            for s in storages.as_array().into_iter().flatten() {
                let Some(id) = s["storage"].as_str() else {
                    continue;
                };
                let content = s["content"].as_str().unwrap_or_default();
                let has = self.privileges(&format!("/storage/{id}")).await?;
                let st = Storage {
                    id: id.to_string(),
                    kind: s["type"].as_str().unwrap_or_default().to_string(),
                    free_bytes: s["avail"].as_u64().unwrap_or(0),
                };
                if content.split(',').any(|k| k == "images")
                    && has.contains("Datastore.AllocateSpace")
                {
                    node.disks.push(st.clone());
                }
                if content.split(',').any(|k| k == "iso")
                    && has.contains("Datastore.AllocateTemplate")
                    && has.contains("Datastore.Audit")
                {
                    node.isos.push(st);
                }
            }
            let nets = self
                .client
                .get(&format!("/nodes/{}/network?type=any_bridge", enc(name)))
                .await
                .unwrap_or_default();
            for b in nets.as_array().into_iter().flatten() {
                let Some(iface) = b["iface"].as_str() else {
                    continue;
                };
                let has = self
                    .privileges(&format!("/sdn/zones/localnetwork/{iface}"))
                    .await?;
                if has.contains("SDN.Use") {
                    node.networks.push(Network {
                        name: iface.to_string(),
                        kind: b["type"].as_str().unwrap_or("bridge").to_string(),
                        cidr: b["cidr"].as_str().unwrap_or_default().to_string(),
                    });
                }
            }
            node.disks.sort_by_key(|s| std::cmp::Reverse(s.free_bytes));
            node.isos.sort_by_key(|s| std::cmp::Reverse(s.free_bytes));
            if !audit {
                c.needs.push(Need {
                    ok: false,
                    says: format!(
                        "Node {name}: memory not visible (requires Sys.Audit on /nodes/{name})"
                    ),
                });
            }
            c.nodes.push(node);
        }
        c.nodes.sort_by_key(|n| std::cmp::Reverse(n.free_mib));
        c.needs.splice(0..0, self.needs(&c, &lacking));
        // Prefixes in use: other Hangars' VMs' tags, and their ISOs' names.
        for r in &resources {
            let tags: Vec<&str> = r["tags"].as_str().unwrap_or_default().split(';').collect();
            if tags.contains(&TAG) {
                c.prefixes.extend(
                    tags.iter()
                        .filter(|t| ![TAG, WORKER_TAG, ENV_TAG, ""].contains(*t))
                        .map(|t| t.to_string()),
                );
            }
        }
        let iso_stores: BTreeSet<String> = c
            .nodes
            .iter()
            .flat_map(|n| n.isos.iter().map(move |s| (n.name.clone(), s.id.clone())))
            .map(|(n, s)| format!("{n}\0{s}"))
            .collect();
        for ns in iso_stores {
            let (node, store) = ns.split_once('\0').unwrap_or_default();
            let content = self
                .client
                .get(&format!(
                    "/nodes/{}/storage/{}/content?content=iso",
                    enc(node),
                    enc(store)
                ))
                .await
                .unwrap_or_default();
            for v in content.as_array().into_iter().flatten() {
                let name = v["volid"]
                    .as_str()
                    .unwrap_or_default()
                    .rsplit('/')
                    .next()
                    .unwrap_or_default();
                for k in ISO_KINDS {
                    if let Some((p, _)) = name.split_once(&format!("-{k}-")) {
                        c.prefixes.insert(p.to_string());
                    }
                }
            }
        }
        Ok(c)
    }

    /// The needs a Hangar has of the token, said as the choices stand.
    fn needs(&self, c: &Choices, lacking: &str) -> Vec<Need> {
        let first = |f: &dyn Fn(&Node) -> Option<String>| c.nodes.iter().find_map(f);
        let pool = c.pools.first().map(|(p, _)| p.clone());
        let disks = first(&|n| n.disks.first().map(|s| s.id.clone()));
        let isos = first(&|n| n.isos.first().map(|s| s.id.clone()));
        let net = first(&|n| n.networks.first().map(|s| s.name.clone()));
        vec![
            match pool {
                Some(p) => Need { ok: true, says: format!("VMs: pool {p}") },
                None if lacking.is_empty() => Need {
                    ok: false,
                    says: "VMs: no pool available (requires PVEVMAdmin, Pool.Allocate and Pool.Audit on /pool/<name>)".into(),
                },
                None => Need { ok: false, says: format!("VMs: missing {lacking}") },
            },
            match disks {
                Some(d) => Need { ok: true, says: format!("Disk storage: {d}") },
                None => Need {
                    ok: false,
                    says: "Disk storage: none available (requires Datastore.AllocateSpace on /storage/<id>)".into(),
                },
            },
            match isos {
                Some(d) => Need { ok: true, says: format!("ISO storage: {d}") },
                None => Need {
                    ok: false,
                    says: "ISO storage: none available (requires Datastore.AllocateTemplate and Datastore.Audit on /storage/<id>)".into(),
                },
            },
            match net {
                Some(n) => Need { ok: true, says: format!("Network: {n}") },
                None => Need {
                    ok: false,
                    says: "Network: none available (requires SDN.Use on /sdn/zones/localnetwork/<bridge>)".into(),
                },
            },
        ]
    }

    /// Makes a Hangar as plan says, from release: its ISO uploaded under
    /// the prefix unless it is there, the VM made and started, its settings
    /// given, and Hangar waited for. A VM made and not finished is removed
    /// again.
    pub async fn make(
        &self,
        plan: &Plan,
        release: &Release,
        progress: &(dyn Fn(Step) + Send + Sync),
    ) -> io::Result<Made> {
        check_prefix(&plan.prefix).map_err(err)?;
        let iso = self
            .upload_release(&plan.node, &plan.isos, &plan.prefix, release, progress)
            .await?;
        if let Some(w) = &plan.web {
            w.check().map_err(err)?;
        }
        let (token, token_sha256) = owner_token();
        let id = self.next_id().await?;
        progress(Step::Making { vmid: id });
        let made = self.make_vm(plan, id, &iso, &token_sha256, progress).await;
        let (address, fingerprint) = match made {
            Ok(a) => a,
            Err(e) => {
                let _ = self.destroy(&plan.node, id).await;
                return Err(e);
            }
        };
        let url = url_of(plan.web.as_ref(), address);
        progress(Step::Answering { url: url.clone() });
        healthy(
            &format!("https://{address}"),
            &fingerprint,
            Duration::from_secs(180),
        )
        .await?;
        Ok(Made {
            node: plan.node.clone(),
            vmid: id,
            isos: plan.isos.clone(),
            pool: plan.pool.clone(),
            prefix: plan.prefix.clone(),
            address,
            url,
            fingerprint,
            token,
        })
    }

    /// Uploads release's ISO as the prefix's, unless it is there; its
    /// volume's ID.
    async fn upload_release(
        &self,
        node: &str,
        isos: &str,
        prefix: &str,
        release: &Release,
        progress: &(dyn Fn(Step) + Send + Sync),
    ) -> io::Result<String> {
        let name = format!("{prefix}-release-{}.iso", release_key(&release.version));
        let volid = format!("{isos}:iso/{name}");
        let have = self
            .client
            .get(&format!(
                "/nodes/{}/storage/{}/content?content=iso",
                enc(node),
                enc(isos)
            ))
            .await?;
        let there = have
            .as_array()
            .into_iter()
            .flatten()
            .any(|v| v["volid"] == volid.as_str());
        if !there {
            progress(Step::Uploading { name: name.clone() });
            let upid = self
                .client
                .upload(node, isos, "iso", &release.file, &name)
                .await?;
            self.client
                .wait(node, &upid, Duration::from_secs(600))
                .await?;
        }
        Ok(volid)
    }

    async fn next_id(&self) -> io::Result<u32> {
        let v = self.client.get("/cluster/nextid").await?;
        v.as_str()
            .and_then(|s| s.parse().ok())
            .or_else(|| v.as_u64().map(|n| n as u32))
            .ok_or_else(|| err(format!("proxmox gave no free VMID: {v}")))
    }

    /// Makes and starts the VM, and gives it its settings: its address
    /// and its certificate's fingerprint.
    async fn make_vm(
        &self,
        plan: &Plan,
        id: u32,
        iso: &str,
        token_sha256: &str,
        progress: &(dyn Fn(Step) + Send + Sync),
    ) -> io::Result<(Ipv4Addr, String)> {
        let node = &plan.node;
        let (vmid, cores, memory) = (
            id.to_string(),
            plan.cores.max(1).to_string(),
            plan.memory_mib.max(1024).to_string(),
        );
        let tags = format!("{TAG};{};{WORKER_TAG}", plan.prefix);
        let name = format!("{}-hangar", plan.prefix);
        let cdrom = format!("{iso},media=cdrom");
        let data = format!(
            "{}:{},serial=hangar-data,discard=on",
            plan.disks,
            plan.data_gib.max(8)
        );
        let net = format!("virtio,bridge={},firewall=1", plan.network);
        let mut form = vec![
            ("vmid", vmid.as_str()),
            ("name", name.as_str()),
            ("tags", tags.as_str()),
            ("ostype", "l26"),
            ("cpu", "host"),
            ("cores", cores.as_str()),
            ("memory", memory.as_str()),
            ("agent", "1"),
            ("serial0", "socket"),
            ("scsihw", "virtio-scsi-pci"),
            ("scsi1", cdrom.as_str()),
            ("virtio0", data.as_str()),
            ("net0", net.as_str()),
            ("boot", "order=scsi1"),
            (
                "description",
                "A Hangar of its own: its server and worker. Made by Hangar.",
            ),
        ];
        if !plan.pool.is_empty() {
            form.push(("pool", plan.pool.as_str()));
        }
        let upid = self
            .client
            .post(&format!("/nodes/{}/qemu", enc(node)), &form)
            .await?;
        self.wait(node, &upid).await?;
        self.client
            .put(
                &self.vm(node, id, "firewall/options"),
                &[
                    ("enable", "1"),
                    ("policy_in", "DROP"),
                    ("policy_out", "ACCEPT"),
                    ("dhcp", "1"),
                    ("ndp", "1"),
                    ("radv", "0"),
                    ("macfilter", "1"),
                    ("ipfilter", "0"),
                ],
            )
            .await?;
        for (port, comment) in PORTS {
            let port = port.to_string();
            self.client
                .post(
                    &self.vm(node, id, "firewall/rules"),
                    &[
                        ("type", "in"),
                        ("action", "ACCEPT"),
                        ("proto", "tcp"),
                        ("dport", port.as_str()),
                        ("enable", "1"),
                        ("comment", comment),
                    ],
                )
                .await?;
        }
        progress(Step::Starting);
        let upid = self
            .client
            .post(&self.vm(node, id, "status/start"), &[])
            .await?;
        self.wait(node, &upid).await?;
        self.agent_ready(node, id).await?;
        progress(Step::Address);
        let address = self.wait_address(node, id).await?;
        progress(Step::Settings);
        let fingerprint = self.give_settings(plan, id, address, token_sha256).await?;
        Ok((address, fingerprint))
    }

    /// Waits for the VM's address from its network's DHCP, as its agent
    /// says.
    async fn wait_address(&self, node: &str, id: u32) -> io::Result<Ipv4Addr> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
        loop {
            if let Some(a) = self.address(node, id).await? {
                return Ok(a);
            }
            if tokio::time::Instant::now() > deadline {
                return Err(err("the VM got no address from the network's DHCP"));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// Writes a Hangar's settings through its VM's guest agent channel, as
    /// plan says, with its host name and certificate (a certificate made
    /// for its address without one) and the hash of its first
    /// administrator's token; the certificate's fingerprint.
    async fn give_settings(
        &self,
        plan: &Plan,
        id: u32,
        address: Ipv4Addr,
        token_sha256: &str,
    ) -> io::Result<String> {
        let (own, fingerprint) = match &plan.web {
            Some(w) => (
                json!({
                    "admin": plan.admin,
                    "admin_token_sha256": token_sha256,
                    "tls_cert": w.cert,
                    "tls_key": w.key,
                    "host": w.host,
                    "host_style": w.style,
                }),
                w.check().map_err(err)?,
            ),
            None => {
                let (cert, key) = web_certificate(address)?;
                (
                    json!({
                        "admin": plan.admin,
                        "admin_token_sha256": token_sha256,
                        "tls_cert": pem("CERTIFICATE", &cert),
                        "tls_key": pem("PRIVATE KEY", &key),
                    }),
                    hangar_hub::tls::fingerprint(&cert),
                )
            }
        };
        let settings = json!({
            "node": plan.prefix,
            "proxmox": {
                "url": self.url,
                "token": self.token,
                "fingerprint": self.fingerprint,
                "node": plan.node,
                "storage": plan.disks,
                "iso_storage": plan.isos,
                "bridge": plan.network,
                "pool": plan.pool,
                "prefix": plan.prefix,
            },
            "own": own,
        });
        self.client
            .post(
                &self.vm(&plan.node, id, "agent/file-write"),
                &[
                    ("file", "/dev/hangar/worker.json"),
                    ("content", &settings.to_string()),
                ],
            )
            .await?;
        Ok(fingerprint)
    }

    /// The Hangars of their own on this Proxmox, as far as the token sees:
    /// VMs named `<prefix>-hangar` with Hangar's tags and their prefix.
    pub async fn hangars(&self) -> io::Result<Vec<Found>> {
        let resources = self.client.get("/cluster/resources?type=vm").await?;
        let mut out = Vec::new();
        for r in resources.as_array().into_iter().flatten() {
            let tags: Vec<&str> = r["tags"].as_str().unwrap_or_default().split(';').collect();
            if !tags.contains(&TAG) || !tags.contains(&WORKER_TAG) {
                continue;
            }
            let Some(prefix) = tags
                .iter()
                .find(|t| ![TAG, WORKER_TAG, ENV_TAG, ""].contains(*t))
            else {
                continue;
            };
            let (Some(node), Some(id)) = (r["node"].as_str(), r["vmid"].as_u64()) else {
                continue;
            };
            if r["name"].as_str() != Some(&format!("{prefix}-hangar")) {
                continue;
            }
            let id = id as u32;
            let config = self.client.get(&self.vm(node, id, "config")).await?;
            let field = |k: &str| config[k].as_str().unwrap_or_default().to_string();
            let (scsi1, virtio0, net0) = (field("scsi1"), field("virtio0"), field("net0"));
            let volume = scsi1.split(',').next().unwrap_or_default();
            let iso = volume.rsplit('/').next().unwrap_or_default();
            let release = iso
                .strip_prefix(&format!("{prefix}-release-"))
                .and_then(|k| k.strip_suffix(".iso"))
                .unwrap_or_default()
                .to_string();
            let option = |s: &str, k: &str| {
                s.split(',')
                    .find_map(|kv| kv.strip_prefix(&format!("{k}=")))
                    .unwrap_or_default()
                    .to_string()
            };
            let running = r["status"] == "running";
            out.push(Found {
                node: node.to_string(),
                vmid: id,
                prefix: prefix.to_string(),
                pool: r["pool"].as_str().unwrap_or_default().to_string(),
                disks: virtio0.split(':').next().unwrap_or_default().to_string(),
                isos: volume.split(':').next().unwrap_or_default().to_string(),
                network: option(&net0, "bridge"),
                cores: number(&config["cores"]).unwrap_or(1) as u32,
                memory_mib: number(&config["memory"]).unwrap_or(0) as u32,
                data_gib: gib(&option(&virtio0, "size")),
                release,
                running,
                address: match running {
                    true => self.address(node, id).await?,
                    false => None,
                },
            });
        }
        out.sort_by_key(|f| (f.prefix.clone(), f.vmid));
        Ok(out)
    }

    /// Takes back a Hangar found here (`hangars`), as its maker does once
    /// the token it kept is lost, with a new token for its first
    /// administrator and a new certificate. Its VM is started if it is
    /// stopped, moved to release first when one is given (as `upgrade`
    /// does), and given its settings afresh, as `make` gives them, with
    /// the new token's hash and this token for its worker. The Hangar
    /// replaces the token its owner had with the new one, and ends their
    /// sessions (`hangar-server`'s initial administrator).
    pub async fn reconnect(
        &self,
        at: &Found,
        release: Option<&Release>,
        admin: &str,
        web: Option<&Web>,
        progress: &(dyn Fn(Step) + Send + Sync),
    ) -> io::Result<Made> {
        if let Some(w) = web {
            w.check().map_err(err)?;
        }
        let (node, id) = (at.node.as_str(), at.vmid);
        if !at.running {
            progress(Step::Starting);
            let upid = self
                .client
                .post(&self.vm(node, id, "status/start"), &[])
                .await?;
            self.wait(node, &upid).await?;
        }
        if let Some(release) = release {
            let iso = self
                .upload_release(node, &at.isos, &at.prefix, release, progress)
                .await?;
            progress(Step::Attaching {
                name: iso.rsplit('/').next().unwrap_or(&iso).to_string(),
            });
            let cdrom = format!("{iso},media=cdrom");
            self.client
                .put(&self.vm(node, id, "config"), &[("scsi1", cdrom.as_str())])
                .await?;
            progress(Step::Restarting);
            let upid = self
                .client
                .post(&self.vm(node, id, "status/reboot"), &[])
                .await?;
            self.wait(node, &upid).await?;
        }
        self.agent_ready(node, id).await?;
        progress(Step::Address);
        let address = self.wait_address(node, id).await?;
        let reached = format!("https://{address}");
        if release.is_some() {
            // Its first start on the release records whom its token went
            // to, which the new settings' token is then given to.
            responds(&reached, Duration::from_secs(180)).await?;
        }
        let (token, token_sha256) = owner_token();
        progress(Step::Settings);
        let plan = Plan {
            web: web.cloned(),
            ..at.plan(admin)
        };
        let fingerprint = self
            .give_settings(&plan, id, address, &token_sha256)
            .await?;
        let url = url_of(web, address);
        progress(Step::Answering { url: url.clone() });
        healthy(&reached, &fingerprint, Duration::from_secs(180)).await?;
        Ok(Made {
            node: at.node.clone(),
            vmid: id,
            isos: at.isos.clone(),
            pool: at.pool.clone(),
            prefix: at.prefix.clone(),
            address,
            url,
            fingerprint,
            token,
        })
    }

    /// Gives a Hangar a host name and the certificate it serves there
    /// (web), or takes it back to its address and a certificate made for
    /// it (none), keeping its first administrator's token: its settings
    /// written afresh through its guest agent, which restarts its server.
    /// The Hangar as it is then.
    pub async fn set_web(
        &self,
        at: &Found,
        admin: &str,
        token: &str,
        web: Option<&Web>,
        progress: &(dyn Fn(Step) + Send + Sync),
    ) -> io::Result<Made> {
        if let Some(w) = web {
            w.check().map_err(err)?;
        }
        let (node, id) = (at.node.as_str(), at.vmid);
        self.agent_ready(node, id).await?;
        progress(Step::Address);
        let address = self.wait_address(node, id).await?;
        progress(Step::Settings);
        let plan = Plan {
            web: web.cloned(),
            ..at.plan(admin)
        };
        let fingerprint = self
            .give_settings(&plan, id, address, &token_sha256(token))
            .await?;
        let url = url_of(web, address);
        progress(Step::Answering { url: url.clone() });
        healthy(
            &format!("https://{address}"),
            &fingerprint,
            Duration::from_secs(180),
        )
        .await?;
        Ok(Made {
            node: at.node.clone(),
            vmid: id,
            isos: at.isos.clone(),
            pool: at.pool.clone(),
            prefix: at.prefix.clone(),
            address,
            url,
            fingerprint,
            token: token.to_string(),
        })
    }

    async fn wait(&self, node: &str, upid: &Value) -> io::Result<()> {
        match upid.as_str() {
            Some(u) if !u.is_empty() => self.client.wait(node, u, Duration::from_secs(300)).await,
            _ => Ok(()),
        }
    }

    /// Waits for the VM's agent to answer on its agent channel.
    async fn agent_ready(&self, node: &str, id: u32) -> io::Result<()> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
        loop {
            if self
                .client
                .post(&self.vm(node, id, "agent/ping"), &[])
                .await
                .is_ok()
            {
                return Ok(());
            }
            if tokio::time::Instant::now() > deadline {
                return Err(err("the VM's agent never answered on its agent channel"));
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    /// The VM's IPv4 address on its network, as its agent says, if it has
    /// one yet.
    pub async fn address(&self, node: &str, id: u32) -> io::Result<Option<Ipv4Addr>> {
        let Ok(v) = self
            .client
            .get(&self.vm(node, id, "agent/network-get-interfaces"))
            .await
        else {
            return Ok(None);
        };
        Ok(v["result"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|i| {
                let n = i["name"].as_str().unwrap_or_default();
                n != "lo" && !n.starts_with("dummy")
            })
            .flat_map(|i| i["ip-addresses"].as_array().cloned().unwrap_or_default())
            .filter(|a| a["ip-address-type"] == "ipv4")
            .filter_map(|a| a["ip-address"].as_str()?.parse::<Ipv4Addr>().ok())
            .find(|a| !a.is_loopback() && !a.is_link_local()))
    }

    /// Moves a Hangar to release: its ISO uploaded, put in its VM's boot
    /// CD-ROM, and the VM rebooted, which leaves its environments running
    /// for its worker to take back.
    pub async fn upgrade(
        &self,
        at: &Made,
        release: &Release,
        progress: &(dyn Fn(Step) + Send + Sync),
    ) -> io::Result<()> {
        let iso = self
            .upload_release(&at.node, &at.isos, &at.prefix, release, progress)
            .await?;
        progress(Step::Attaching {
            name: iso.rsplit('/').next().unwrap_or(&iso).to_string(),
        });
        let cdrom = format!("{iso},media=cdrom");
        self.client
            .put(
                &self.vm(&at.node, at.vmid, "config"),
                &[("scsi1", cdrom.as_str())],
            )
            .await?;
        // Asked at its address, which a host name's DNS need not answer
        // with from here.
        let reached = match self.address(&at.node, at.vmid).await {
            Ok(Some(a)) => format!("https://{a}"),
            _ => at.url.clone(),
        };
        progress(Step::Restarting);
        self.client
            .post(&self.vm(&at.node, at.vmid, "status/reboot"), &[])
            .await?;
        // The release it ran answers until the VM goes down.
        let down_by = tokio::time::Instant::now() + Duration::from_secs(60);
        while tokio::time::Instant::now() < down_by
            && health(&reached, &at.fingerprint).await.is_ok()
        {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        progress(Step::Answering {
            url: at.url.clone(),
        });
        healthy(&reached, &at.fingerprint, Duration::from_secs(180)).await
    }

    /// Removes a Hangar and everything it made: its own VM first, so its
    /// worker makes nothing more, then its environments' VMs, then its
    /// ISOs. Only what carries its prefix, in its pool.
    pub async fn remove(&self, at: &Made) -> io::Result<()> {
        self.destroy(&at.node, at.vmid).await?;
        let resources = self.client.get("/cluster/resources?type=vm").await?;
        for r in resources.as_array().into_iter().flatten() {
            let tags: Vec<&str> = r["tags"].as_str().unwrap_or_default().split(';').collect();
            if !tags.contains(&TAG) || !tags.contains(&at.prefix.as_str()) {
                continue;
            }
            if !at.pool.is_empty() && r["pool"] != at.pool.as_str() {
                continue;
            }
            let (Some(node), Some(id)) = (r["node"].as_str(), r["vmid"].as_u64()) else {
                continue;
            };
            self.destroy(node, id as u32).await?;
        }
        let content = self
            .client
            .get(&format!(
                "/nodes/{}/storage/{}/content?content=iso",
                enc(&at.node),
                enc(&at.isos)
            ))
            .await?;
        for v in content.as_array().into_iter().flatten() {
            let Some(volid) = v["volid"].as_str() else {
                continue;
            };
            let name = volid.rsplit('/').next().unwrap_or_default();
            if !is_own_iso(&at.prefix, name) {
                continue;
            }
            let upid = self
                .client
                .delete(
                    &format!(
                        "/nodes/{}/storage/{}/content/{}",
                        enc(&at.node),
                        enc(&at.isos),
                        enc(volid)
                    ),
                    &[],
                )
                .await?;
            self.wait(&at.node, &upid).await?;
        }
        Ok(())
    }

    /// Stops a VM if it runs and destroys it with its disks; one gone
    /// already is no error.
    async fn destroy(&self, node: &str, id: u32) -> io::Result<()> {
        let st = match self.client.get(&self.vm(node, id, "status/current")).await {
            Ok(st) => st,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        if st["status"] == "running" {
            let upid = self
                .client
                .post(&self.vm(node, id, "status/stop"), &[])
                .await?;
            self.wait(node, &upid).await?;
        }
        let upid = self
            .client
            .delete(
                &format!("/nodes/{}/qemu/{id}", enc(node)),
                &[("purge", "1"), ("destroy-unreferenced-disks", "1")],
            )
            .await?;
        self.wait(node, &upid).await
    }
}

/// A new access token for a Hangar's first administrator, as its server
/// makes them (hgr_ and 32 random bytes), and its SHA-256 in hex, which is
/// all the Hangar is given.
/// The SHA-256 of an access token, in hex, as the Hangar is given it.
fn token_sha256(token: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, token.as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A Hangar's address for people and the app: its host name's, else its
/// address's.
fn url_of(web: Option<&Web>, address: Ipv4Addr) -> String {
    match web {
        Some(w) => format!("https://{}", w.host),
        None => format!("https://{address}"),
    }
}

fn owner_token() -> (String, String) {
    let mut raw = [0u8; 32];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut raw)
        .expect("the system's random numbers");
    let token = format!(
        "hgr_{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
    );
    let hash = token_sha256(&token);
    (token, hash)
}

/// A number Proxmox gives as one or as a string.
fn number(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str()?.parse().ok())
}

/// A disk's size as Proxmox writes it (16G, 512M, 1T) in GiB.
fn gib(size: &str) -> u32 {
    let (n, unit) = size.split_at(
        size.find(|c: char| !c.is_ascii_digit())
            .unwrap_or(size.len()),
    );
    let n: u64 = n.parse().unwrap_or(0);
    (match unit {
        "T" => n * 1024,
        "M" => n / 1024,
        "K" => n / (1024 * 1024),
        _ => n,
    }) as u32
}

/// How long a Hangar's HTTPS certificate lasts: under the 825 days Apple's
/// platforms take for one a person trusts.
const WEB_CERTIFICATE_DAYS: i64 = 820;

/// A certificate and PKCS #8 key, both DER, for a Hangar's HTTPS at
/// address, which a browser takes once a person trusts it: the address its
/// name, for a TLS server alone, lasting WEB_CERTIFICATE_DAYS.
pub fn web_certificate(address: Ipv4Addr) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let mut p = rcgen::CertificateParams::default();
    p.subject_alt_names = vec![rcgen::SanType::IpAddress(address.into())];
    p.distinguished_name = rcgen::DistinguishedName::new();
    p.distinguished_name
        .push(rcgen::DnType::CommonName, format!("Hangar {address}"));
    p.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    p.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature];
    let mut serial = [0u8; 16];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut serial)
        .map_err(|_| err("no random numbers for a serial number"))?;
    serial[0] &= 0x7f;
    p.serial_number = Some(rcgen::SerialNumber::from_slice(&serial));
    let now = time::OffsetDateTime::now_utc();
    p.not_before = now - time::Duration::days(1);
    p.not_after = now + time::Duration::days(WEB_CERTIFICATE_DAYS);
    let key = rcgen::KeyPair::generate().map_err(io::Error::other)?;
    let cert = p.self_signed(&key).map_err(io::Error::other)?;
    Ok((cert.der().to_vec(), key.serialize_der()))
}

/// DER as PEM, under label.
fn pem(label: &str, der: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(der);
    let mut out = format!("-----BEGIN {label}-----\n");
    for line in b64.as_bytes().chunks(64) {
        out.push_str(&String::from_utf8_lossy(line));
        out.push('\n');
    }
    out.push_str(&format!("-----END {label}-----\n"));
    out
}

/// Waits for the Hangar at url to say it is healthy, over HTTPS to the
/// certificate with fingerprint alone.
pub async fn healthy(url: &str, fingerprint: &str, within: Duration) -> io::Result<()> {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let last = match health(url, fingerprint).await {
            Ok(()) => return Ok(()),
            Err(e) => e,
        };
        if tokio::time::Instant::now() > deadline {
            return Err(err(format!("Hangar at {url} did not answer: {last}")));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Waits for the Hangar at url to say it is healthy over HTTPS, taking any
/// certificate: whether it has started, and nothing more.
async fn responds(url: &str, within: Duration) -> io::Result<()> {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let last = match health_with(url, None).await {
            Ok(()) => return Ok(()),
            Err(e) => e,
        };
        if tokio::time::Instant::now() > deadline {
            return Err(err(format!("Hangar at {url} did not answer: {last}")));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn health(url: &str, fingerprint: &str) -> io::Result<()> {
    health_with(url, Some(fingerprint)).await
}

/// Asks the Hangar at url whether it is healthy, over HTTPS to the
/// certificate with fingerprint alone, or to any.
async fn health_with(url: &str, fingerprint: Option<&str>) -> io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let host = url.trim_start_matches("https://").trim_end_matches('/');
    let (name, port) = match host.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().map_err(io::Error::other)?),
        None => (host.to_string(), 443u16),
    };
    let config = match fingerprint {
        Some(fp) => {
            hangar_hub::tls::pinned(fp, &[&rustls::version::TLS13, &rustls::version::TLS12])?
        }
        None => rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(io::Error::other)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Recorder(Default::default())))
        .with_no_client_auth(),
    };
    let tcp = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::TcpStream::connect((name.as_str(), port)),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "connecting"))??;
    let server = rustls::pki_types::ServerName::try_from(name.clone()).map_err(io::Error::other)?;
    let mut tls = tokio::time::timeout(
        Duration::from_secs(5),
        tokio_rustls::TlsConnector::from(Arc::new(config)).connect(server, tcp),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TLS"))??;
    tls.write_all(
        format!("GET /api/healthz HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n")
            .as_bytes(),
    )
    .await?;
    let mut answer = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), tls.read_to_end(&mut answer))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "reading"))??;
    let head = String::from_utf8_lossy(&answer);
    if head.starts_with("HTTP/1.1 200") {
        Ok(())
    } else {
        Err(err(head.lines().next().unwrap_or("no answer").to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A certificate for names and its key, PEM.
    fn certificate(names: &[&str]) -> (String, String) {
        let key = rcgen::KeyPair::generate().unwrap();
        let p =
            rcgen::CertificateParams::new(names.iter().map(|n| n.to_string()).collect::<Vec<_>>())
                .unwrap();
        (p.self_signed(&key).unwrap().pem(), key.serialize_pem())
    }

    fn web(host: &str, style: &str, (cert, key): &(String, String)) -> Web {
        Web {
            host: host.into(),
            style: style.into(),
            cert: cert.clone(),
            key: key.clone(),
        }
    }

    #[test]
    fn a_wildcard_covers_a_host_and_its_prefix_names() {
        let c = certificate(&["*.example.test"]);
        let fp = web("hangar.example.test", "prefix", &c).check().unwrap();
        assert_eq!(fp.len(), 64);
        // Under the host, registry.hangar.example.test is two labels below
        // the wildcard's domain.
        let e = web("hangar.example.test", "subdomain", &c)
            .check()
            .unwrap_err();
        assert!(e.contains("registry.hangar.example.test"), "{e}");
        let both = certificate(&["hangar.example.test", "*.hangar.example.test"]);
        web("hangar.example.test", "subdomain", &both)
            .check()
            .unwrap();
    }

    #[test]
    fn a_certificate_is_refused_with_another_key_or_name() {
        let (cert, _) = certificate(&["*.example.test"]);
        let (_, other) = certificate(&["*.example.test"]);
        let e = web("hangar.example.test", "prefix", &(cert.clone(), other))
            .check()
            .unwrap_err();
        assert!(e.contains("not the certificate's"), "{e}");
        let c = certificate(&["*.example.test"]);
        let e = web("hangar.elsewhere.test", "prefix", &c)
            .check()
            .unwrap_err();
        assert!(e.contains("does not cover hangar.elsewhere.test"), "{e}");
        for host in ["192.168.1.9", "hangar", ""] {
            assert!(web(host, "prefix", &c).check().is_err(), "{host}");
        }
        assert!(web("hangar.example.test", "sideways", &c).check().is_err());
    }
}
