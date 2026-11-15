//! The certificates HTTPS serves: a certificate and key in PEM files that
//! someone else issues and renews, or ones an ACME CA issues for the zone
//! and every name under it, which environments' hosts need.
//!
//! A wildcard certificate is issued only against the DNS-01 challenge, a
//! TXT record the CA looks up in the zone. The server is its zone's
//! authoritative server (zone.rs), so it answers the challenge itself:
//! nothing is configured at a DNS provider beyond delegating the zone
//! once. The account, certificates and keys are in the store
//! (`tls_storage`), and so is the lock held while obtaining them
//! (`tls_locks`), so every server sharing the store serves the same
//! certificates and only one obtains each.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Utc};
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, NewAccount,
    NewOrder, OrderStatus, RetryPolicy,
};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use serde::{Deserialize, Serialize};

use crate::store::Store;

/// The least time between two looks at whether certificate files have
/// changed.
const FILES_CHECK_EVERY: Duration = Duration::from_secs(30);

/// How often the stored certificates are read again (another server may
/// have renewed them) and checked for renewal.
const CHECK_EVERY: Duration = Duration::from_secs(600);

/// How long a lock is held without being renewed; one whose holder died
/// is free again after this.
const LOCK_LEASE: Duration = Duration::from_secs(120);

/// The lock held while obtaining certificates.
const LOCK: &str = "hangar/issue";

/// The TTL of a challenge's TXT record.
const CHALLENGE_TTL: i32 = 60;

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// The TLS configuration a listener serves a resolver's certificates with.
pub fn server_config(resolver: Arc<dyn ResolvesServerCert>) -> rustls::ServerConfig {
    let mut c = rustls::ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .expect("ring supports the default protocol versions")
        .with_no_client_auth()
        .with_cert_resolver(resolver);
    c.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    c
}

/// What a leaf certificate says about itself.
struct Leaf {
    names: Vec<String>,
    not_before: DateTime<Utc>,
    not_after: DateTime<Utc>,
}

fn leaf(der: &CertificateDer<'_>) -> Result<Leaf, String> {
    let (_, cert) = x509_parser::parse_x509_certificate(der)
        .map_err(|e| format!("parsing the certificate: {e}"))?;
    let at = |t: x509_parser::time::ASN1Time| {
        DateTime::from_timestamp(t.timestamp(), 0).unwrap_or_default()
    };
    let names = match cert.subject_alternative_name() {
        Ok(Some(san)) => san
            .value
            .general_names
            .iter()
            .filter_map(|n| match n {
                x509_parser::extensions::GeneralName::DNSName(d) => Some(d.to_lowercase()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    Ok(Leaf {
        names,
        not_before: at(cert.validity().not_before),
        not_after: at(cert.validity().not_after),
    })
}

/// Whether a certificate naming names covers host.
fn covers(names: &[String], host: &str) -> bool {
    let host = host.to_lowercase();
    names.iter().any(|n| match n.strip_prefix("*.") {
        Some(parent) => host
            .split_once('.')
            .is_some_and(|(label, rest)| !label.is_empty() && rest == parent),
        None => *n == host,
    })
}

fn certified(chain: &[u8], key: &[u8]) -> Result<CertifiedKey, String> {
    let certs = CertificateDer::pem_slice_iter(chain)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("reading the certificate chain: {e}"))?;
    if certs.is_empty() {
        return Err("there is no certificate".into());
    }
    let key =
        PrivateKeyDer::from_pem_slice(key).map_err(|e| format!("reading the private key: {e}"))?;
    CertifiedKey::from_der(certs, key, &provider()).map_err(|e| e.to_string())
}

/// A certificate and key someone else issues and renews, kept in PEM files:
/// a wildcard certificate for the domain Hangar's hosts are named in, say.
/// Replacing the files is picked up without a restart; a replacement that
/// does not load leaves the last good one served.
#[derive(Debug)]
pub struct Files {
    cert: PathBuf,
    key: PathBuf,
    state: Mutex<FilesState>,
}

#[derive(Debug)]
struct FilesState {
    served: Arc<CertifiedKey>,
    modified: [SystemTime; 2],
    checked: Instant,
}

impl Files {
    /// Reads the certificate chain in cert, leaf first, and its private key
    /// in key.
    pub fn load(cert: &Path, key: &Path) -> Result<Files, String> {
        let modified = modified(cert, key)?;
        let served = Arc::new(load_files(cert, key)?);
        Ok(Files {
            cert: cert.into(),
            key: key.into(),
            state: Mutex::new(FilesState {
                served,
                modified,
                checked: Instant::now(),
            }),
        })
    }

    /// The names the certificate served covers.
    pub fn names(&self) -> Vec<String> {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.served
            .end_entity_cert()
            .ok()
            .and_then(|c| leaf(c).ok())
            .map(|l| l.names)
            .unwrap_or_default()
    }

    /// Warns about each of the hosts Hangar serves that the certificate
    /// does not cover: a browser would refuse them.
    pub fn warn_uncovered(&self, hosts: &[String]) {
        let names = self.names();
        for h in hosts {
            let h = crate::publichost::hostname(h);
            if !covers(&names, h) {
                eprintln!(
                    "hangar-server: the certificate in {} does not cover {h}, a host Hangar serves; browsers will refuse it (it covers {})",
                    self.cert.display(),
                    names.join(", ")
                );
            }
        }
    }

    /// The certificate to serve, read again first if the files have
    /// changed since it was read.
    fn current(&self) -> Arc<CertifiedKey> {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if s.checked.elapsed() < FILES_CHECK_EVERY {
            return s.served.clone();
        }
        s.checked = Instant::now();
        let Ok(m) = modified(&self.cert, &self.key) else {
            return s.served.clone();
        };
        if m == s.modified {
            return s.served.clone();
        }
        // Tried again once they change again.
        s.modified = m;
        match load_files(&self.cert, &self.key) {
            Ok(c) => {
                eprintln!(
                    "hangar-server: loaded the replaced certificate in {}",
                    self.cert.display()
                );
                s.served = Arc::new(c);
            }
            Err(e) => eprintln!(
                "hangar-server: the certificate files changed but did not load ({e}); serving the last good certificate"
            ),
        }
        s.served.clone()
    }
}

impl ResolvesServerCert for Files {
    fn resolve(&self, _: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.current())
    }
}

fn modified(cert: &Path, key: &Path) -> Result<[SystemTime; 2], String> {
    let m = |p: &Path| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .map_err(|e| format!("{}: {e}", p.display()))
    };
    Ok([m(cert)?, m(key)?])
}

fn load_files(cert: &Path, key: &Path) -> Result<CertifiedKey, String> {
    let read = |p: &Path| std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()));
    certified(&read(cert)?, &read(key)?).map_err(|e| {
        format!(
            "loading the certificate in {} and key in {}: {e}",
            cert.display(),
            key.display()
        )
    })
}

/// A certificate as the store keeps it.
#[derive(Serialize, Deserialize)]
struct Stored {
    /// PEM, leaf first.
    chain: String,
    /// PEM.
    key: String,
}

/// Certificates an ACME CA issues for the zone and for `*.zone`, obtained
/// and renewed in the background.
pub struct Acme {
    zone: String,
    email: Option<String>,
    ca: String,
    roots: Option<PathBuf>,
    store: Store,
    apex: RwLock<Option<Arc<CertifiedKey>>>,
    wildcard: RwLock<Option<Arc<CertifiedKey>>>,
}

impl std::fmt::Debug for Acme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Acme").field("zone", &self.zone).finish()
    }
}

impl Acme {
    pub fn new(
        zone: &str,
        email: Option<String>,
        ca: String,
        roots: Option<PathBuf>,
        store: Store,
    ) -> Acme {
        Acme {
            zone: crate::zone::canonical(zone),
            email,
            ca,
            roots,
            store,
            apex: RwLock::new(None),
            wildcard: RwLock::new(None),
        }
    }

    fn names(&self) -> [String; 2] {
        [self.zone.clone(), format!("*.{}", self.zone)]
    }

    fn slot(&self, name: &str) -> &RwLock<Option<Arc<CertifiedKey>>> {
        if name.starts_with("*.") {
            &self.wildcard
        } else {
            &self.apex
        }
    }

    fn key(&self, name: &str) -> String {
        format!("hangar/certificates/{name}")
    }

    /// Obtains whichever certificate there is none of yet and keeps them
    /// renewed. Until a name has one, TLS handshakes for it fail.
    pub async fn run(self: Arc<Self>) {
        eprintln!(
            "hangar-server: managing TLS certificates for {} from {}",
            self.names().join(", "),
            self.ca
        );
        let mut retry = Duration::from_secs(60);
        loop {
            let wait = match self.round().await {
                Ok(()) => {
                    retry = Duration::from_secs(60);
                    CHECK_EVERY
                }
                Err(e) => {
                    eprintln!(
                        "hangar-server: obtaining TLS certificates: {e}; trying again in {}s",
                        retry.as_secs()
                    );
                    let w = retry;
                    retry = (retry * 2).min(Duration::from_secs(3600));
                    w
                }
            };
            tokio::time::sleep(wait).await;
        }
    }

    /// Reads the stored certificates, and obtains any missing or due for
    /// renewal, holding the lock while it does.
    async fn round(&self) -> Result<(), String> {
        if self.due().await?.is_empty() {
            return Ok(());
        }
        let Some(lock) = Lock::take(&self.store).await? else {
            // Another server is obtaining them; its result is read next
            // round.
            return Ok(());
        };
        let result = async {
            // Another server may have obtained them since they were read.
            for name in self.due().await? {
                self.obtain(&name).await?;
            }
            Ok(())
        }
        .await;
        lock.release().await;
        result
    }

    /// Loads the stored certificates, giving the names that have none or
    /// whose certificate is in the last third of its life.
    async fn due(&self) -> Result<Vec<String>, String> {
        let mut due = Vec::new();
        for name in self.names() {
            let mut tx = self.store.begin().await.map_err(|e| e.to_string())?;
            let stored = tx
                .tls_load(&self.key(&name))
                .await
                .map_err(|e| e.to_string())?;
            tx.commit().await.map_err(|e| e.to_string())?;
            let loaded = stored.and_then(|b| {
                let s: Stored = serde_json::from_slice(&b).ok()?;
                let c = certified(s.chain.as_bytes(), s.key.as_bytes()).ok()?;
                let l = leaf(c.end_entity_cert().ok()?).ok()?;
                Some((c, l))
            });
            match loaded {
                Some((c, l)) => {
                    let renew_at = l.not_after - (l.not_after - l.not_before) / 3;
                    *self.slot(&name).write().unwrap_or_else(|e| e.into_inner()) =
                        Some(Arc::new(c));
                    if Utc::now() >= renew_at {
                        due.push(name);
                    }
                }
                None => due.push(name),
            }
        }
        Ok(due)
    }

    async fn account(&self) -> Result<Account, String> {
        let builder = || match &self.roots {
            Some(r) => Account::builder_with_root(r),
            None => Account::builder(),
        };
        let key = format!("hangar/acme/{}/account", account_key(&self.ca));
        let mut tx = self.store.begin().await.map_err(|e| e.to_string())?;
        let stored = tx.tls_load(&key).await.map_err(|e| e.to_string())?;
        tx.commit().await.map_err(|e| e.to_string())?;
        if let Some(b) = stored
            && let Ok(creds) = serde_json::from_slice::<AccountCredentials>(&b)
        {
            return builder()
                .map_err(|e| e.to_string())?
                .from_credentials(creds)
                .await
                .map_err(|e| format!("the ACME account: {e}"));
        }
        let contact: Vec<String> = self.email.iter().map(|e| format!("mailto:{e}")).collect();
        let contact: Vec<&str> = contact.iter().map(String::as_str).collect();
        let (account, creds) = builder()
            .map_err(|e| e.to_string())?
            .create(
                &NewAccount {
                    contact: &contact,
                    terms_of_service_agreed: true,
                    only_return_existing: false,
                },
                self.ca.clone(),
                None,
            )
            .await
            .map_err(|e| format!("creating the ACME account: {e}"))?;
        let b = serde_json::to_vec(&creds).map_err(|e| e.to_string())?;
        let mut tx = self.store.begin().await.map_err(|e| e.to_string())?;
        tx.tls_store(&key, &b, Utc::now())
            .await
            .map_err(|e| e.to_string())?;
        tx.commit().await.map_err(|e| e.to_string())?;
        Ok(account)
    }

    /// Has the CA issue a certificate for name, answering its challenges
    /// from the zone, and keeps it.
    async fn obtain(&self, name: &str) -> Result<(), String> {
        eprintln!("hangar-server: obtaining a certificate for {name}");
        let account = self.account().await?;
        let ids = [Identifier::Dns(name.into())];
        let mut order = account
            .new_order(&NewOrder::new(&ids))
            .await
            .map_err(|e| format!("ordering a certificate for {name}: {e}"))?;
        let mut presented = Vec::new();
        let answered = async {
            let mut authzs = order.authorizations();
            while let Some(a) = authzs.next().await {
                let mut a = a.map_err(|e| e.to_string())?;
                match a.status {
                    AuthorizationStatus::Valid => continue,
                    AuthorizationStatus::Pending => {}
                    s => return Err(format!("an authorization is {s:?}")),
                }
                let mut ch = a
                    .challenge(ChallengeType::Dns01)
                    .ok_or("the CA offers no DNS-01 challenge")?;
                let Identifier::Dns(domain) = ch.identifier().identifier else {
                    return Err("the CA asks about something other than a name".into());
                };
                let record = format!("_acme-challenge.{domain}");
                let value = ch.key_authorization().dns_value();
                crate::zone::add_txt(&self.store, &record, &value, CHALLENGE_TTL)
                    .await
                    .map_err(|e| e.to_string())?;
                presented.push((record, value));
                ch.set_ready().await.map_err(|e| e.to_string())?;
            }
            let retry = RetryPolicy::new()
                .initial_delay(Duration::from_secs(1))
                .timeout(Duration::from_secs(120));
            order.poll_ready(&retry).await.map_err(|e| e.to_string())
        }
        .await;
        for (record, value) in &presented {
            if let Err(e) = crate::zone::remove_txt(&self.store, record, value).await {
                eprintln!("hangar-server: removing the challenge record {record}: {e}");
            }
        }
        match answered? {
            OrderStatus::Ready => {}
            s => return Err(format!("the order for {name} is {s:?}, not ready")),
        }
        let key = order
            .finalize()
            .await
            .map_err(|e| format!("finalizing the order for {name}: {e}"))?;
        let retry = RetryPolicy::new()
            .initial_delay(Duration::from_secs(1))
            .timeout(Duration::from_secs(120));
        let chain = order
            .poll_certificate(&retry)
            .await
            .map_err(|e| format!("fetching the certificate for {name}: {e}"))?;
        let c = certified(chain.as_bytes(), key.as_bytes())?;
        let b = serde_json::to_vec(&Stored { chain, key }).map_err(|e| e.to_string())?;
        let mut tx = self.store.begin().await.map_err(|e| e.to_string())?;
        tx.tls_store(&self.key(name), &b, Utc::now())
            .await
            .map_err(|e| e.to_string())?;
        tx.commit().await.map_err(|e| e.to_string())?;
        *self.slot(name).write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(c));
        eprintln!("hangar-server: obtained a certificate for {name}");
        Ok(())
    }
}

impl ResolvesServerCert for Acme {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let read = |s: &RwLock<Option<Arc<CertifiedKey>>>| {
            s.read().unwrap_or_else(|e| e.into_inner()).clone()
        };
        let under = hello.server_name().is_some_and(|n| {
            !n.eq_ignore_ascii_case(&self.zone)
                && n.to_lowercase().ends_with(&format!(".{}", self.zone))
        });
        if under {
            read(&self.wildcard)
        } else {
            read(&self.apex)
        }
    }
}

/// A CA's directory as part of a storage key.
fn account_key(ca: &str) -> String {
    ca.trim_start_matches("https://")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// The lock on obtaining certificates, its lease renewed while held.
struct Lock {
    store: Store,
    renewing: tokio::task::JoinHandle<()>,
}

impl Lock {
    async fn take(store: &Store) -> Result<Option<Lock>, String> {
        let now = Utc::now();
        let mut tx = store.begin().await.map_err(|e| e.to_string())?;
        let took = tx
            .tls_lock(LOCK, now + LOCK_LEASE, now)
            .await
            .map_err(|e| e.to_string())?;
        tx.commit().await.map_err(|e| e.to_string())?;
        if !took {
            return Ok(None);
        }
        let s = store.clone();
        let renewing = tokio::spawn(async move {
            loop {
                tokio::time::sleep(LOCK_LEASE / 4).await;
                let renewed = async {
                    let mut tx = s.begin().await?;
                    let held = tx.tls_renew_lock(LOCK, Utc::now() + LOCK_LEASE).await?;
                    tx.commit().await?;
                    Ok::<_, crate::store::Error>(held)
                };
                match renewed.await {
                    Ok(true) => {}
                    Ok(false) => {
                        eprintln!("hangar-server: the lock on obtaining certificates was lost");
                        return;
                    }
                    Err(e) => eprintln!("hangar-server: renewing the certificates' lock: {e}"),
                }
            }
        });
        Ok(Some(Lock {
            store: store.clone(),
            renewing,
        }))
    }

    async fn release(self) {
        self.renewing.abort();
        let released = async {
            let mut tx = self.store.begin().await?;
            tx.tls_unlock(LOCK).await?;
            tx.commit().await
        };
        if let Err(e) = released.await {
            eprintln!("hangar-server: releasing the certificates' lock: {e}");
        }
    }
}
