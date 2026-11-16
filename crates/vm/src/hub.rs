//! The worker's half of the hub (hangar-hub): where agents with no vsock to
//! their host dial in over the network.
//!
//! Each machine that uses it is registered before it boots, with a secret
//! of its own that its initramfs carries to the agent. When the agent dials
//! in and says that secret, its link stands in for the machine's vsock: a
//! stream it opens to the control channel or the shared files is carried to
//! the socket the worker already listens on for that port (`<base>_<port>`,
//! as libkrun lays vsock out), and the worker reaches the guest's own
//! services by opening streams to their ports. Nothing above this module
//! knows which carried a connection.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hangar_hub::{Acceptor, Link, Settings};
use tokio::net::{TcpListener, UnixStream};
use tokio::sync::watch;

use crate::err;
use crate::guest::GuestConn;
use crate::log::Log;
use crate::vsock::hybrid_path;

/// The ports a guest may open streams to: the control channel, and the
/// shared files.
const GUEST_DIALS: [u32; 2] = [crate::agent::PORT, crate::files::SHARED_FILES_PORT];

/// The agent's settings in the initramfs, beside it at its root.
pub const SETTINGS_FILE: &str = "hub.json";

/// How long a connection has to say hello.
const HELLO_WITHIN: Duration = Duration::from_secs(10);

/// How long a dial waits for the guest's agent to be connected, as it is
/// again shortly after a resume.
const LINK_WITHIN: Duration = Duration::from_secs(20);

/// Where agents dial in, and the machines it is expecting.
pub struct Hub {
    acceptor: Acceptor,
    address: String,
    fingerprint: String,
    envs: Mutex<HashMap<String, Entry>>,
    /// Numbers registrations, so one ending removes itself alone and not
    /// a newer one of the same environment.
    next: std::sync::atomic::AtomicU64,
    log: Log,
}

struct Entry {
    registration: u64,
    secret: String,
    /// The machine's vsock base, where the worker listens for what the
    /// guest dials; set once its sockets are bound.
    base: Option<PathBuf>,
    link: watch::Sender<Option<Link>>,
}

impl Hub {
    /// Listens on listen for agents, which dial it at address. The
    /// certificate they pin is kept in dir, made there the first time.
    pub async fn start(listen: &str, address: &str, dir: &Path, log: Log) -> io::Result<Arc<Hub>> {
        let (cert, key) = certificate(dir)?;
        let fingerprint = hangar_hub::tls::fingerprint(&cert);
        let acceptor = Acceptor::new(cert, key)?;
        let ln = TcpListener::bind(listen)
            .await
            .map_err(|e| err(format!("listening for agents on {listen}: {e}")))?;
        let hub = Arc::new(Hub {
            acceptor,
            address: address.to_string(),
            fingerprint,
            envs: Mutex::new(HashMap::new()),
            next: std::sync::atomic::AtomicU64::new(1),
            log,
        });
        hub.log.info(
            "listening for agents",
            &[("listen", &listen), ("address", &address)],
        );
        let me = Arc::downgrade(&hub);
        tokio::spawn(async move {
            loop {
                let tcp = match ln.accept().await {
                    Ok((tcp, _)) => tcp,
                    Err(_) => {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let Some(hub) = me.upgrade() else { return };
                tokio::spawn(hub.serve(tcp));
            }
        });
        Ok(hub)
    }

    /// Expects environment's agent, with secret if it is resuming with the
    /// one it booted with, else a new one.
    pub fn register(self: &Arc<Self>, environment: &str, secret: Option<String>) -> Registration {
        let secret = secret.unwrap_or_else(new_secret);
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (link, _) = watch::channel(None);
        let replaced = self.envs.lock().unwrap().insert(
            environment.to_string(),
            Entry {
                registration: id,
                secret: secret.clone(),
                base: None,
                link,
            },
        );
        if let Some(old) = replaced
            && let Some(l) = old.link.borrow().as_ref()
        {
            l.close();
        }
        Registration {
            hub: self.clone(),
            environment: environment.to_string(),
            secret,
            id,
            ended: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// One connection: TLS, the agent's hello, and if it is a machine this
    /// hub expects, its link, until it closes.
    async fn serve(self: Arc<Self>, tcp: tokio::net::TcpStream) {
        let peer = tcp.peer_addr().map(|a| a.to_string()).unwrap_or_default();
        let g = match self.acceptor.hello(tcp, HELLO_WITHIN).await {
            Ok(g) => g,
            Err(e) => {
                self.log.debug(
                    "an agent connection ended before its hello",
                    &[("peer", &peer), ("err", &e)],
                );
                return;
            }
        };
        let (env, secret) = (g.hello.environment.clone(), g.hello.secret.clone());
        let expected = self
            .envs
            .lock()
            .unwrap()
            .get(&env)
            .is_some_and(|e| same(&e.secret, &secret));
        if !expected {
            self.log.warn(
                "refused an agent: no such environment here, or the wrong secret",
                &[("environment", &env), ("peer", &peer)],
            );
            g.refuse("no such environment here, or the wrong secret")
                .await;
            return;
        }
        let (link, mut incoming) = match g.welcome().await {
            Ok(l) => l,
            Err(_) => return,
        };
        self.log.info(
            "agent connected to the hub",
            &[("environment", &env), ("peer", &peer)],
        );
        // A registration that has ended since the hello takes the link
        // with it.
        match self.envs.lock().unwrap().get(&env) {
            Some(e) if same(&e.secret, &secret) => {
                e.link.send_replace(Some(link.clone()));
            }
            _ => link.close(),
        }
        while let Some(a) = incoming.next().await {
            let (hub, env, secret) = (self.clone(), env.clone(), secret.clone());
            let link = link.clone();
            tokio::spawn(async move {
                let Ok((port, stream)) = a.open().await else {
                    return;
                };
                // Any other port is the agent trying the link, answered
                // by closing the stream.
                if !GUEST_DIALS.contains(&port) {
                    return;
                }
                let Some(base) = hub.base(&env, &secret).await else {
                    hub.log.warn(
                        "a guest dialled a machine the hub is not expecting",
                        &[("environment", &env), ("port", &port)],
                    );
                    return;
                };
                let to = match UnixStream::connect(hybrid_path(&base, port)).await {
                    Ok(t) => t,
                    Err(e) => {
                        hub.log.warn(
                            "a guest dialled a port nothing listens on",
                            &[("environment", &env), ("port", &port), ("err", &e)],
                        );
                        return;
                    }
                };
                link.relay(stream, to).await;
            });
        }
        // The link is over: forget it, unless a newer one has replaced it.
        if let Some(e) = self.envs.lock().unwrap().get(&env) {
            e.link.send_if_modified(|l| match l {
                Some(cur) if cur.is_closed() => {
                    *l = None;
                    true
                }
                _ => false,
            });
        }
    }

    /// Where the worker listens for what an environment's guest dials,
    /// once a registration with this secret says, waiting for a while: a
    /// guest resuming dials before the worker has bound its sockets again.
    async fn base(&self, environment: &str, secret: &str) -> Option<PathBuf> {
        let deadline = tokio::time::Instant::now() + LINK_WITHIN;
        loop {
            let base = self
                .envs
                .lock()
                .unwrap()
                .get(environment)
                .filter(|e| same(&e.secret, secret))
                .and_then(|e| e.base.clone());
            if base.is_some() {
                return base;
            }
            if tokio::time::Instant::now() > deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Opens a connection to port in environment's guest.
    pub async fn dial(&self, environment: &str, port: u32) -> io::Result<GuestConn> {
        let rx = self
            .envs
            .lock()
            .unwrap()
            .get(environment)
            .map(|e| e.link.subscribe());
        let Some(mut rx) = rx else {
            return Err(err(format!("{environment} is not on the hub")));
        };
        let link = tokio::time::timeout(
            LINK_WITHIN,
            rx.wait_for(|l| l.as_ref().is_some_and(|l| !l.is_closed())),
        )
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{environment}'s agent is not connected to the hub"),
            )
        })?
        .map_err(|_| err("the hub has stopped"))?
        .clone()
        .expect("a link");
        let stream = link.open(port).await?;
        let (here, there) = UnixStream::pair()?;
        tokio::spawn(link.relay(stream, there));
        Ok(GuestConn::from_unix(here))
    }
}

/// A machine the hub expects, until this is dropped.
pub struct Registration {
    hub: Arc<Hub>,
    environment: String,
    secret: String,
    id: u64,
    ended: std::sync::atomic::AtomicBool,
}

impl Registration {
    /// What the agent needs to dial in.
    pub fn settings(&self) -> Settings {
        Settings {
            address: self.hub.address.clone(),
            fingerprint: self.hub.fingerprint.clone(),
            environment: self.environment.clone(),
            secret: self.secret.clone(),
        }
    }

    pub fn secret(&self) -> &str {
        &self.secret
    }

    /// Says where the machine's vsock sockets are, once they are bound.
    pub fn set_base(&self, base: &Path) {
        if let Some(e) = self.hub.envs.lock().unwrap().get_mut(&self.environment) {
            e.base = Some(base.to_path_buf());
        }
    }

    /// Opens a connection to port in the guest.
    pub async fn dial(&self, port: u32) -> io::Result<GuestConn> {
        self.hub.dial(&self.environment, port).await
    }

    /// Ends the registration, and closes the machine's link: a guest
    /// suspended finds it closed when it resumes, and dials again, as a
    /// resumed guest's vsock connections are reset under it.
    pub fn end(&self) {
        if self.ended.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let mut envs = self.hub.envs.lock().unwrap();
        if envs
            .get(&self.environment)
            .is_some_and(|e| e.registration == self.id)
            && let Some(e) = envs.remove(&self.environment)
            && let Some(l) = e.link.borrow().as_ref()
        {
            l.close();
        }
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.end();
    }
}

/// The secret a machine booted with, kept in its directory for a resume.
pub fn saved_secret(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join("hub-secret"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Keeps the secret a machine boots with, for a resume.
pub fn save_secret(dir: &Path, secret: &str) -> io::Result<()> {
    write_private(&dir.join("hub-secret"), secret.as_bytes())
}

/// The hub's certificate and key, both DER, from dir, made the first time.
fn certificate(dir: &Path) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let (cert_path, key_path) = (dir.join("hub-cert.der"), dir.join("hub-key.der"));
    if let (Ok(c), Ok(k)) = (std::fs::read(&cert_path), std::fs::read(&key_path)) {
        return Ok((c, k));
    }
    let (cert, key) = hangar_hub::tls::generate()?;
    std::fs::create_dir_all(dir)?;
    write_private(&key_path, &key)?;
    write_private(&cert_path, &cert)?;
    Ok((cert, key))
}

fn write_private(path: &Path, b: &[u8]) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".new");
    let tmp = PathBuf::from(tmp);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(b)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

/// A secret for one boot: 32 random bytes, in hex.
fn new_secret() -> String {
    let mut b = [0u8; 32];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut b)
        .expect("the system's random numbers");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Whether two secrets are the same, in time that does not depend on where
/// they differ.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}
