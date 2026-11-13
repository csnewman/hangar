//! hangar-worker: it holds the environments the control plane places on
//! this machine.
//!
//! It is level-triggered. The server sends the complete set of environments
//! this worker should hold, and the worker applies all of it every time,
//! whether or not anything changed; it reports the complete set it actually
//! holds, on every change and on a timer. A lost message on either side is
//! repaired by the next one, so nothing needs acknowledging or replaying.
//!
//! Losing the server is not a reason to change anything. A worker that
//! cannot reach it keeps running what it is running, and an environment the
//! server has no record of is reported, never stopped.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use hangar_api::{
    DesiredState, EnvironmentSpec, GpuInfo, ImageCopy, LocalImage, ObservedEnvironment,
    REGISTRY_PATH, Resources, Spec, WorkerStatus,
};
use hangar_images::HangarRegistry;
use hangar_vm::log::Log;
use hangar_vm::runtime::FilesView;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt};

use crate::client::{self, Client};
use crate::config::Config;
use crate::files;
use crate::logs::{self, Combiner, Reply, Request, Rings};
use crate::sys::{self, SysStats};
use crate::tunnel::{self, Header};

/// How often the worker reports even when nothing changed. The report
/// carries usage figures, which change all the time, so this is also how
/// fresh they are; it is well inside the server's online window, so a few
/// lost reports do not make the worker look offline.
pub const REPORT_EVERY: Duration = Duration::from_secs(5);

/// How long an environment has to answer a stream the control plane opens.
const DIAL_TIMEOUT: Duration = Duration::from_secs(30);

/// The label a worker gives itself for what its virtual GPUs render with,
/// software or hardware, so a template can ask for one by placement. A
/// label of that name in the configuration wins.
pub const RENDERER_LABEL: &str = "gpu-renderer";

/// What actually runs environments on a machine.
pub trait Runtime: Clone + Send + Sync + 'static {
    /// A connection to one of an environment's services.
    type Conn: AsyncRead + AsyncWrite + Unpin + Send + 'static;

    /// Moves one environment towards spec.desired. It returns promptly and
    /// does the work in the background, and it is called again with the
    /// same spec on every desired set, so it must be idempotent.
    fn apply(&self, spec: EnvironmentSpec);
    /// Every environment the runtime holds, whether or not the server asked
    /// for it. An environment that has been deleted and fully removed is
    /// absent.
    fn observe(&self) -> Vec<ObservedEnvironment>;
    /// Waits for Observe's answer to have changed, maybe.
    fn changed(&self) -> impl Future<Output = ()> + Send;
    /// Connects to a port of a running environment's guest (ports).
    fn dial(&self, id: &str, port: u32) -> impl Future<Output = io::Result<Self::Conn>> + Send;

    /// Where an environment's log of that name is, if it is one the
    /// environment has.
    fn log_file(&self, _id: &str, _name: &str) -> Option<PathBuf> {
        None
    }
    /// Every image in the runtime's own store, if it keeps one.
    fn images(&self) -> Vec<LocalImage> {
        Vec::new()
    }
    /// Deletes a copy of an image from the store, unless an environment
    /// uses it. It is asked again on every desired set until the copy is
    /// gone, so it must be idempotent.
    fn remove_image(&self, _copy: &ImageCopy) {}
    /// Says where Hangar's own registry is, if it has one.
    fn use_registry(&self, _hangar: Option<HangarRegistry>) {}
    /// What its virtual GPUs render with.
    fn gpu(&self) -> Option<GpuInfo> {
        None
    }
    /// What runs its environments, and whether it can suspend them.
    fn monitor(&self) -> (String, bool) {
        (String::new(), false)
    }
    /// Sizes what it runs by what the worker offers: a machine whose memory
    /// can be resized while it runs can grow to all of it.
    fn use_capacity(&self, _offered: Resources) {}
    /// Serves environments the files they share with others: from root,
    /// each what view says it may reach.
    fn use_shared_files(&self, _root: PathBuf, _view: FilesView) {}
}

pub struct Worker<R: Runtime> {
    cfg: Config,
    rt: R,
    client: Client,
    capacity: Resources,
    log: Log,
    sys: SysStats,
    /// The worker's own log, kept in memory for the control plane, and each
    /// environment's logs as one.
    rings: Option<Arc<Rings>>,
    combined: Option<Arc<Combiner>>,
}

impl<R: Runtime> Worker<R> {
    /// A worker offering this machine's capacity, less what the
    /// configuration reserves for the host, times its overcommit, which
    /// answers the control plane's requests for its log from rings.
    pub fn new(cfg: Config, rt: R, log: Log, rings: Option<Arc<Rings>>) -> io::Result<Arc<Self>> {
        let total = sys::machine_capacity()
            .map_err(|e| io::Error::other(format!("measuring this machine: {e}")))?;
        let offered = offer(&cfg, total, sys::swap_mib(), &log);
        let client = Client::new(&cfg.server.url);
        rt.use_capacity(offered);
        if !cfg.storage.files.is_empty() {
            // Made if it is not there: a worker's first start has no files.
            std::fs::create_dir_all(&cfg.storage.files)
                .map_err(|e| io::Error::other(format!("the shared files' root: {e}")))?;
            let (c, l) = (client.clone(), log.clone());
            rt.use_shared_files(
                PathBuf::from(&cfg.storage.files),
                Arc::new(move |env: &str| files::view(&c, &l, env) as Arc<dyn hangar_nfs::View>),
            );
        }
        let combined = rings.as_ref().map(|r| {
            let rt = rt.clone();
            let files: logs::LogFiles =
                Arc::new(move |env: &str, name: &str| rt.log_file(env, name));
            Arc::new(Combiner::new(r.clone(), files))
        });
        let sys = SysStats::new(&cfg.storage.environments);
        Ok(Arc::new(Worker {
            cfg,
            rt,
            client,
            capacity: offered,
            log,
            sys,
            rings,
            combined,
        }))
    }

    /// What this worker offers the server: the machine, less what is
    /// reserved for the host, times its overcommit.
    pub fn capacity(&self) -> Resources {
        self.capacity
    }

    pub fn runtime(&self) -> &R {
        &self.rt
    }

    /// Registers if need be, then holds the desired set until the server
    /// rejects this worker's credential. Dropping the future stops it.
    pub async fn run(self: Arc<Self>) -> Result<(), client::Error> {
        self.retry("registering", || self.client.load_or_register(&self.cfg))
            .await?;
        self.log.info(
            "worker ready",
            &[
                ("name", &self.cfg.node.name),
                ("server", &self.cfg.server.url),
                ("cpus", &self.capacity.cpus),
                ("memory_mib", &self.capacity.memory_mib),
            ],
        );
        let combining = self.combined.clone().map(|c| tokio::spawn(c.run()));
        let handler = {
            let w = self.clone();
            move |h: Header, s: tunnel::Stream| {
                let w = w.clone();
                async move { w.stream(h, s).await }
            }
        };
        let credential = self.client.credential();
        let out = tokio::select! {
            r = self.report_loop() => r,
            r = self.desired_loop() => r,
            _ = tunnel::serve(&self.cfg.server.url, &credential, self.log.clone(), handler) => {
                Err(client::Error::Rejected)
            }
        };
        if let Some(c) = combining {
            c.abort();
        }
        out
    }

    async fn desired_loop(&self) -> Result<(), client::Error> {
        let mut version = 0;
        loop {
            let set = self
                .retry("fetching the desired set", || self.client.desired(version))
                .await?;
            if set.version != version {
                self.log.debug(
                    "desired set",
                    &[
                        ("version", &set.version),
                        ("environments", &set.environments.len()),
                    ],
                );
            }
            version = set.version;
            for spec in set.environments {
                self.rt.apply(spec);
            }
            // What the server has no record of, and an administrator asked
            // to be rid of, goes as a deleted environment would.
            for id in set.remove_environments {
                self.rt.apply(EnvironmentSpec {
                    id,
                    name: String::new(),
                    desired: DesiredState::Deleted,
                    spec: Spec::default(),
                    pin_digest: None,
                    drop_rollback: false,
                });
            }
            self.rt
                .use_registry((!set.registry.is_empty()).then(|| HangarRegistry {
                    host: set.registry.clone(),
                    url: format!("{}{REGISTRY_PATH}", self.cfg.server.url),
                    credential: self.client.credential(),
                }));
            for c in &set.remove_images {
                self.rt.remove_image(c);
            }
        }
    }

    /// The worker's report of everything it holds.
    pub fn status(&self) -> WorkerStatus {
        let environments = self.rt.observe();
        let mut stats = self.sys.measure();
        if let Some(st) = &mut stats {
            for e in &environments {
                if let Some(es) = &e.stats {
                    st.support_cpus += es.support_cpus;
                    st.gpu_cpus += es.gpu_cpus;
                    st.guest_memory_mib += es.host_memory_mib;
                    st.monitor_memory_mib += es.monitor_memory_mib;
                    st.support_memory_mib += es.support_memory_mib;
                    st.guest_swap_mib += es.swap_used_mib;
                    st.guest_swap_pool_mib += es.swap_pool_mib;
                    st.guest_swap_disk_bytes += es.swap_disk_bytes;
                }
            }
        }
        let gpu = self.rt.gpu();
        let (monitor, suspends) = self.rt.monitor();
        WorkerStatus {
            capacity: self.capacity,
            labels: with_renderer_label(&self.cfg.node.labels, gpu.as_ref()),
            environments,
            stats,
            images: self.rt.images(),
            gpu,
            monitor,
            suspends,
        }
    }

    async fn report_loop(&self) -> Result<(), client::Error> {
        let mut tick = tokio::time::interval(REPORT_EVERY);
        tick.tick().await;
        loop {
            self.retry("reporting status", || async {
                let st = self.status();
                self.forget_gone(&st.environments);
                self.client.report(&st).await
            })
            .await?;
            tokio::select! {
                _ = tick.tick() => {}
                _ = self.rt.changed() => {
                    // Changes tend to arrive in bursts, and one report can
                    // carry them all.
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }

    /// Lets go of the logs kept of environments that are gone.
    fn forget_gone(&self, here: &[ObservedEnvironment]) {
        let Some(rings) = &self.rings else { return };
        let here: BTreeSet<&str> = here.iter().map(|e| e.id.as_str()).collect();
        for env in rings.environments() {
            if !here.contains(env.as_str()) {
                rings.forget(&env);
            }
        }
    }

    /// Runs f until it succeeds, backing off between failures. A rejected
    /// credential ends it: no amount of retrying fixes a revoked worker.
    async fn retry<T, F, Fut>(&self, what: &str, mut f: F) -> Result<T, client::Error>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, client::Error>>,
    {
        let mut backoff = Duration::from_millis(500);
        loop {
            match f().await {
                Ok(v) => return Ok(v),
                Err(client::Error::Rejected) => return Err(client::Error::Rejected),
                Err(e) => {
                    self.log.warn(
                        &format!("{what} failed, retrying"),
                        &[("err", &e), ("after", &format!("{backoff:?}"))],
                    );
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(15));
                }
            }
        }
    }

    /// Serves one stream the control plane opens over the tunnel.
    async fn stream(&self, h: Header, mut s: tunnel::Stream) {
        if h.kind == tunnel::KIND_LOGS {
            self.serve_logs(&mut s).await;
            let _ = s.shutdown().await;
            return;
        }
        let Some(port) = port_of(&h.kind) else {
            self.log.warn(
                "unknown stream from the control plane",
                &[("kind", &h.kind)],
            );
            return;
        };
        let guest =
            match tokio::time::timeout(DIAL_TIMEOUT, self.rt.dial(&h.environment, port)).await {
                Ok(Ok(g)) => g,
                Ok(Err(e)) => {
                    self.log.warn(
                        "opening a stream to an environment",
                        &[
                            ("kind", &h.kind),
                            ("environment", &h.environment),
                            ("err", &e),
                        ],
                    );
                    let _ = s.shutdown().await;
                    return;
                }
                Err(_) => {
                    self.log.warn(
                        "opening a stream to an environment",
                        &[
                            ("kind", &h.kind),
                            ("environment", &h.environment),
                            ("err", &"timed out"),
                        ],
                    );
                    let _ = s.shutdown().await;
                    return;
                }
            };
        relay(s, guest).await;
    }

    /// Answers one request for part of a log: the worker's own, whole or an
    /// environment's lines of it, or a file an environment's machine
    /// writes.
    async fn serve_logs(&self, s: &mut tunnel::Stream) {
        let mut line = Vec::new();
        if s.read_until(b'\n', &mut line).await.is_err() || line.is_empty() {
            return;
        }
        let reply = match serde_json::from_slice::<Request>(&line) {
            Ok(req) => self.read_log(&req),
            Err(_) => Reply::error("a malformed request"),
        };
        let mut b = serde_json::to_vec(&reply).unwrap_or_default();
        b.push(b'\n');
        let _ = s.write_all(&b).await;
    }

    fn read_log(&self, req: &Request) -> Reply {
        match req.log.as_str() {
            logs::COMBINED => match &self.combined {
                Some(c) if !req.environment.is_empty() => c.read_combined(req),
                _ => Reply::missing(),
            },
            logs::WORKER => match &self.rings {
                Some(r) => r.read(req),
                None => Reply::missing(),
            },
            name => {
                if req.environment.is_empty() {
                    return Reply::missing();
                }
                match self.rt.log_file(&req.environment, name) {
                    Some(path) => {
                        logs::read_file(&path, req).unwrap_or_else(|e| Reply::error(e.to_string()))
                    }
                    None => Reply::missing(),
                }
            }
        }
    }
}

/// The guest port a kind of stream is served on.
fn port_of(kind: &str) -> Option<u32> {
    use hangar_vm::runtime as rt;
    Some(match kind {
        tunnel::KIND_TERMINAL => rt::TERMINAL_PORT,
        tunnel::KIND_DESKTOP => rt::DESKTOP_PORT,
        tunnel::KIND_CODE => rt::CODE_PORT,
        tunnel::KIND_PROFILE => rt::PROFILE_PORT,
        tunnel::KIND_PROCESSES => rt::PROCESSES_PORT,
        tunnel::KIND_SSH => rt::SSH_PORT,
        tunnel::KIND_PORTS => rt::GUEST_PORTS_PORT,
        _ => return None,
    })
}

/// Relays bytes both ways, as they are: the protocol is the guest's and the
/// browser's, and nothing here needs to read it. Once either side has
/// finished, both are closed.
async fn relay<A, B>(a: A, b: B)
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    let (mut ar, mut aw) = tokio::io::split(a);
    let (mut br, mut bw) = tokio::io::split(b);
    tokio::select! {
        _ = tokio::io::copy(&mut ar, &mut bw) => {}
        _ = tokio::io::copy(&mut br, &mut aw) => {}
    }
    let _ = tokio::join!(aw.shutdown(), bw.shutdown());
}

/// What the worker offers: the machine's CPUs and memory, less what is
/// reserved, times the overcommit ratios. Memory offered beyond the
/// machine's that its swap cannot cover is warned about.
pub fn offer(cfg: &Config, total: Resources, swap_mib: i64, log: &Log) -> Resources {
    let ratio = |r: f64| r.max(1.0);
    let cpus = (total.cpus - cfg.reserved.cpus).max(0);
    let mem = (total.memory_mib - cfg.reserved.memory.mib() as i64).max(0);
    let offered = Resources {
        cpus: (cpus as f64 * ratio(cfg.overcommit.cpus)) as i64,
        memory_mib: (mem as f64 * ratio(cfg.overcommit.memory)) as i64,
    };
    let beyond = offered.memory_mib - mem;
    if beyond > 0 && swap_mib < beyond {
        log.warn(
            "memory is overcommitted beyond this machine's memory and swap: guests that use all of theirs will be killed",
            &[
                ("offered_mib", &offered.memory_mib),
                ("memory_mib", &mem),
                ("swap_mib", &swap_mib),
            ],
        );
    }
    offered
}

/// labels with the renderer label added, for a worker whose GPU renders.
pub fn with_renderer_label(
    labels: &BTreeMap<String, String>,
    gpu: Option<&GpuInfo>,
) -> BTreeMap<String, String> {
    let mut out = labels.clone();
    let Some(gl) = gpu
        .filter(|g| g.unavailable.is_empty())
        .and_then(|g| g.gl.as_ref())
    else {
        return out;
    };
    out.entry(RENDERER_LABEL.to_string())
        .or_insert_with(|| if gl.software { "software" } else { "hardware" }.to_string());
    out
}
