//! The worker's runtime: environments as machines.
//!
//! Each environment has a controller task of its own. The worker hands it
//! the environment's spec whenever the desired set arrives; the controller
//! compares what is asked for with what is running and does whatever closes
//! the gap -- boots the machine, powers it off, or deletes its disks -- one
//! step at a time, so two requests for the same environment never overlap.
//!
//! An environment is running only once its agent has dialled back and the
//! template's provisioning has finished. Until then it is starting, with a
//! reason saying which step it is on, so a slow clone and a wedged boot
//! look different from the outside.
//!
//! Everything an environment keeps between runs is in its own directory
//! under the state directory: its writable layer and Docker store, its
//! console log, a mark recording that provisioning is done, and which copy
//! of its image it was made over. Stopping keeps the directory, so the next
//! start boots the same disks without cloning again; deleting removes it.

mod upgrade;

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use hangar_api::{
    DesiredState, Display, EnvironmentSpec, EnvironmentStats, Gpu, GpuDevice, GpuInfo, GpuRenderer,
    ImageCopy, ImageUpdate, LocalImage, ObservedEnvironment, Phase, Progress, Resources, Step,
};
use hangar_images::{Auth, HangarRegistry, Store};
use tokio::sync::{Notify, watch};

use crate::ch;
use crate::err;
use crate::guest::GuestConn;
use crate::host::{self, Monitor};
use crate::instance::{ImageDevice, Instance, InstanceConfig};
use crate::krun;
use crate::log::Log;
use crate::provision::{self, Reporter};
use crate::stats::{self, Counters, HostProcs};
use crate::suspend::{self, SUSPENDED_MARK};
use upgrade::Rollback;

/// The guest's services, by vsock port.
pub const TERMINAL_PORT: u32 = 101;
pub const DESKTOP_PORT: u32 = 103;
pub const CODE_PORT: u32 = 104;
pub const PROFILE_PORT: u32 = 105;
pub const PROCESSES_PORT: u32 = 106;
pub const SSH_PORT: u32 = 107;
pub const GUEST_PORTS_PORT: u32 = 108;

/// The logs an environment's machine writes, by name: its console, its
/// monitor's, and its GPU backend's.
pub const LOGS: [&str; 3] = ["console", "monitor", "gpu"];

/// How often a running machine is measured.
const SAMPLE_EVERY: Duration = Duration::from_secs(5);

/// How long a guest has to give back memory it was asked to. One that has
/// not by then is using it, and asking on would have it kill what it runs
/// to find more, so it keeps what it has.
const SHRINK_PATIENCE: Duration = Duration::from_secs(120);

/// How often a step's measurement is announced: each one is a report to the
/// server.
const MEASURE_EVERY: Duration = Duration::from_secs(1);

/// How often the references environments here are pinned to are looked
/// up, so a reference that has moved on -- a tag pushed again, a local
/// source rebuilt -- is offered to them as an update.
const LOOK_UP_IMAGES_EVERY: Duration = Duration::from_secs(120);

/// How often the host's huge page counts are read, and the least time
/// between two warnings about them.
const HUGE_PAGE_CHECK_EVERY: Duration = Duration::from_secs(60);
const HUGE_PAGE_WARN_EVERY: Duration = Duration::from_secs(15 * 60);

/// An environment's pinned copy, in its directory.
const IMAGE_PIN_FILE: &str = "image.json";

/// What the runtime needs from the worker's configuration.
#[derive(Clone, Default)]
pub struct Config {
    /// The monitor the worker's configuration names, which must be this
    /// host's; empty is the host's own.
    pub monitor: String,
    /// Holds one directory per environment.
    pub state_dir: PathBuf,
    /// The worker's local store of the images its environments boot from.
    pub images_dir: PathBuf,
    /// The guest kernel every environment boots.
    pub kernel: PathBuf,
    /// The hangar-agent every environment runs, built for the guest's
    /// architecture: each boot's initramfs is the agent, as init, so it
    /// always matches the worker that talks to it.
    pub agent: PathBuf,
    /// A local source for an image reference, as a template names it, which
    /// the worker copies into its store. Any other reference is pulled from
    /// its registry.
    pub images: HashMap<String, PathBuf>,
    /// The credentials pulls sign in to registries with, by host.
    pub registries: HashMap<String, Auth>,
    /// Each environment's room for its writable layer and its Docker store,
    /// which share one sparse disk of their sum.
    pub upper_gib: u64,
    pub docker_gib: u64,
    /// Each environment's swap, as a multiple of its memory; 0 gives none.
    pub swap: f64,
    pub image_device: ImageDevice,
    /// Offers an environment with a virtual GPU Vulkan as well as OpenGL,
    /// and carries its Vulkan state across a suspend.
    pub gpu_venus: bool,
    pub gpu_venus_restore: bool,
    /// Sizes the window such an environment maps GPU resources into.
    pub gpu_window_mib: u32,
    /// What renders for such environments (ch::gpu_env).
    pub gpu_renderer: String,
    pub gpu_device: String,
    /// How long an agent has to dial back after the monitor starts.
    pub boot_timeout: Duration,
    /// The monitor's program, mkfs.ext4, and gvproxy (a libkrun guest's
    /// network on macOS); none finds each.
    pub monitor_bin: Option<PathBuf>,
    pub mkfs: Option<PathBuf>,
    pub gvproxy: Option<PathBuf>,
    pub log: Log,
}

impl Config {
    fn mkfs(&self) -> io::Result<PathBuf> {
        match &self.mkfs {
            Some(m) => Ok(m.clone()),
            None => provision::find_mkfs(),
        }
    }
}

impl Config {
    fn defaults(&mut self) {
        if self.upper_gib == 0 {
            self.upper_gib = 16;
        }
        if self.docker_gib == 0 {
            self.docker_gib = 24;
        }
        if self.gpu_window_mib == 0 {
            self.gpu_window_mib = 512;
        }
        if self.boot_timeout.is_zero() {
            self.boot_timeout = Duration::from_secs(180);
        }
    }
}

/// What one environment may reach of the shared files, kept current as
/// long as the environment runs.
pub type FilesView = Arc<dyn Fn(&str) -> Arc<dyn hangar_nfs::View> + Send + Sync>;

#[derive(Default)]
struct RtState {
    envs: BTreeMap<String, Arc<Machine>>,
    /// Set by the first spec the server hands over. The worker applies a
    /// desired set whole, so from then on an environment with no spec is
    /// one the server has no record of.
    applied: bool,
    /// What a machine's memory can grow to while it runs (use_capacity);
    /// zero boots each with its own, for as long as it runs.
    memory_max_mib: u32,
    files: Option<(PathBuf, FilesView)>,
}

/// Runs environments as machines.
pub struct Runtime {
    cfg: Config,
    monitor: Monitor,
    store: Arc<Store>,
    state: Mutex<RtState>,
    changed: Notify,
    /// What virtual GPUs render with here, none when nothing here renders
    /// for a guest; gpu_env the environment they render under; gpu_zink
    /// draws a guest's OpenGL with zink on its GPU, under libkrun on macOS.
    gpu: Option<GpuInfo>,
    gpu_env: Vec<(String, String)>,
    gpu_zink: bool,
    stop: watch::Sender<bool>,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl Runtime {
    /// Checks that this host can run environments and starts a runtime.
    ///
    /// Environments with a directory left in the state directory by an
    /// earlier run are reported as stopped, or suspended: their machines
    /// went with the process that ran them, and their disks are still there
    /// for the next start.
    pub async fn new(mut cfg: Config) -> io::Result<Arc<Runtime>> {
        cfg.defaults();
        preflight(&cfg)?;
        std::fs::create_dir_all(&cfg.state_dir)?;
        let store = {
            let (dir, sources, auth) = (
                cfg.images_dir.clone(),
                cfg.images.clone(),
                cfg.registries.clone(),
            );
            tokio::task::spawn_blocking(move || Store::open(&dir, sources, auth))
                .await
                .map_err(io::Error::other)??
        };
        let gpu_env = ch::gpu_env(&cfg.gpu_renderer, &cfg.gpu_device)
            .map_err(|e| err(format!("vm.gpu: {e}")))?;
        let caps = host::detect(&cfg.monitor)?;
        let (mut gpu, mut gpu_zink) = (None, false);
        match caps.monitor {
            // libkrun on Linux renders on the host's own OpenGL and Vulkan,
            // which hangar-krun probes under the environment its machines
            // get.
            Monitor::Krun if caps.os == "linux" => {
                match krun::read_features(cfg.monitor_bin.as_deref()).await {
                    Err(e) => cfg
                        .log
                        .warn("environments are offered no virtual GPU", &[("err", &e)]),
                    Ok(f) if f.gpu => {
                        gpu = Some(gpu_info(
                            &cfg,
                            krun::probe_gpu(cfg.monitor_bin.as_deref(), &gpu_env).await,
                        ))
                    }
                    Ok(_) => {}
                }
            }
            Monitor::Krun => (gpu, gpu_zink) = krun_gpu(&cfg).await,
            Monitor::CloudHypervisor => {
                if ch::find_gpu_backend().is_ok() {
                    gpu = Some(gpu_info(&cfg, ch::probe_gpu(&gpu_env).await));
                }
            }
        }
        let (stop, _) = watch::channel(false);
        let rt = Arc::new(Runtime {
            cfg,
            monitor: caps.monitor,
            store: Arc::new(store),
            state: Mutex::default(),
            changed: Notify::new(),
            gpu,
            gpu_env,
            gpu_zink,
            stop,
            tasks: Mutex::default(),
        });
        if caps.monitor == Monitor::CloudHypervisor || caps.os == "linux" {
            rt.spawn(watch_huge_pages(
                Arc::downgrade(&rt),
                rt.stop.subscribe(),
                rt.cfg.log.clone(),
            ));
        }
        for e in std::fs::read_dir(&rt.cfg.state_dir)?.flatten() {
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                let id = e.file_name().to_string_lossy().into_owned();
                let phase = if e.path().join(SUSPENDED_MARK).exists() {
                    Phase::Suspended
                } else {
                    Phase::Stopped
                };
                let mut st = rt.state.lock().unwrap();
                rt.new_machine(&mut st, &id, phase);
            }
        }
        rt.spawn(watch_images(Arc::downgrade(&rt), rt.stop.subscribe()));
        Ok(rt)
    }

    fn spawn(&self, f: impl Future<Output = ()> + Send + 'static) {
        self.tasks.lock().unwrap().push(tokio::spawn(f));
    }

    /// What virtual GPUs render with here.
    pub fn gpu(&self) -> Option<&GpuInfo> {
        self.gpu.as_ref()
    }

    /// What runs the environments.
    pub fn monitor(&self) -> &'static str {
        self.monitor.name()
    }

    /// Whether the environments can be suspended, which they can under
    /// either monitor.
    pub fn suspends(&self) -> bool {
        true
    }

    /// Waits for something to have changed that the worker reports.
    pub async fn changed(&self) {
        self.changed.notified().await;
    }

    fn notify(&self) {
        self.changed.notify_one();
    }

    /// Has machines booted after it able to grow to the worker's whole
    /// offer of memory while they run. The room is address space, not
    /// memory: the guest takes none of it until it is resized into it.
    pub fn use_capacity(&self, offered: Resources) {
        self.state.lock().unwrap().memory_max_mib = offered.memory_mib.max(0) as u32;
    }

    /// Has environments booted from then on served the files they share
    /// with others, over NFS: from root, each what view says it may reach.
    pub fn use_shared_files(&self, root: PathBuf, view: FilesView) {
        self.state.lock().unwrap().files = Some((root, view));
    }

    /// Says where Hangar's own registry is.
    pub fn use_registry(&self, hangar: Option<HangarRegistry>) {
        self.store.use_registry(hangar);
    }

    /// Every environment here, as it is.
    pub fn observe(self: &Arc<Self>) -> Vec<ObservedEnvironment> {
        let machines: Vec<Arc<Machine>> =
            self.state.lock().unwrap().envs.values().cloned().collect();
        machines
            .into_iter()
            .map(|m| {
                let st = m.st.lock().unwrap();
                let mut o = ObservedEnvironment {
                    id: m.id.clone(),
                    phase: st.phase,
                    reason: st.reason.clone(),
                    progress: None,
                    stats: None,
                    image_digest: st
                        .image
                        .as_ref()
                        .map(|i| i.digest.clone())
                        .unwrap_or_default(),
                    image_pinned: false,
                    image_update: None,
                    image_rollback: None,
                };
                if st.phase == Phase::Starting {
                    o.progress = st.progress.clone();
                }
                if st.phase == Phase::Running {
                    o.stats = st.stats.clone();
                }
                drop(st);
                m.image_state(&mut o);
                o
            })
            .collect()
    }

    /// Hands an environment's spec to its controller, starting one if this
    /// is the first the runtime has heard of it.
    pub fn apply(self: &Arc<Self>, spec: EnvironmentSpec) {
        let m = {
            let mut st = self.state.lock().unwrap();
            st.applied = true;
            match st.envs.get(&spec.id) {
                Some(m) => m.clone(),
                // Never held here, so there is nothing to delete.
                None if spec.desired == DesiredState::Deleted => return,
                None => {
                    let id = spec.id.clone();
                    self.new_machine(&mut st, &id, Phase::Stopped)
                }
            }
        };
        m.want(spec);
    }

    fn new_machine(self: &Arc<Self>, st: &mut RtState, id: &str, phase: Phase) -> Arc<Machine> {
        let dir = self.cfg.state_dir.join(id);
        let m = Arc::new(Machine {
            rt: Arc::downgrade(self),
            id: id.to_string(),
            log: self.cfg.log.with("environment", id),
            st: Mutex::new(MState {
                phase,
                image: read_image_pin(&dir),
                rollback: upgrade::read_rollback(&dir),
                ..Default::default()
            }),
            dir,
            disk: tokio::sync::Mutex::new(()),
            nudge: Notify::new(),
        });
        st.envs.insert(id.to_string(), m.clone());
        let (mm, stop) = (m.clone(), self.stop.subscribe());
        self.spawn(async move { mm.control(stop).await });
        m
    }

    fn forget(&self, id: &str) {
        self.state.lock().unwrap().envs.remove(id);
        self.prune();
        self.notify();
    }

    /// Powers every machine off and waits for them, or until timeout.
    ///
    /// A worker that is stopping takes its machines with it: nothing would
    /// be left to supervise them, answer their agents, or notice them exit.
    /// Their disks are kept, so they start again where they left off.
    pub async fn shutdown(&self, timeout: Duration) -> io::Result<()> {
        let _ = self.stop.send(true);
        let tasks: Vec<_> = std::mem::take(&mut *self.tasks.lock().unwrap());
        let all = async {
            for t in tasks {
                let _ = t.await;
            }
        };
        tokio::time::timeout(timeout, all)
            .await
            .map_err(|_| err("the machines did not all power off in time"))
    }

    /// The local store, with the environments using each copy.
    pub fn images(&self) -> Vec<LocalImage> {
        self.store
            .list()
            .into_iter()
            .map(|i| {
                let c = ImageCopy {
                    r#ref: i.reference.clone(),
                    digest: i.digest.clone(),
                };
                LocalImage {
                    environments: self.image_users(&c),
                    r#ref: i.reference,
                    digest: i.digest,
                    size_bytes: i.size_bytes as i64,
                    state: i.state.to_string(),
                    current: i.current,
                }
            })
            .collect()
    }

    /// Deletes a copy of an image, unless an environment on this worker
    /// still uses it -- stopped or not, since starting it again boots from
    /// that copy.
    pub fn remove_image(&self, c: &ImageCopy) {
        if !self.image_users(c).is_empty() {
            return;
        }
        if let Err(e) = self.store.remove(&store_copy(c)) {
            self.cfg.log.warn(
                "removing an image",
                &[("image", &c.r#ref), ("digest", &c.digest), ("err", &e)],
            );
            return;
        }
        self.notify();
    }

    /// Deletes the copies of images their references have moved on from
    /// that no environment here uses.
    fn prune(&self) {
        if self.unaccounted() {
            return;
        }
        let (removed, errs) = self.store.prune(|c| {
            !self
                .image_users(&ImageCopy {
                    r#ref: c.reference.clone(),
                    digest: c.digest.clone(),
                })
                .is_empty()
        });
        for c in removed {
            self.cfg.log.info(
                "removed a superseded copy of an image",
                &[("image", &c.reference), ("digest", &c.digest)],
            );
        }
        for e in errs {
            self.cfg
                .log
                .warn("removing superseded copies of images", &[("err", &e)]);
        }
    }

    /// Whether an environment here has disks but no pin, and no spec yet to
    /// say what its image is: it could be using any copy. Once the server
    /// has handed over its specs, one still without is unknown to it and
    /// cannot be started, so it holds on to nothing.
    fn unaccounted(&self) -> bool {
        let st = self.state.lock().unwrap();
        if st.applied {
            return false;
        }
        st.envs.values().any(|m| {
            let s = m.st.lock().unwrap();
            s.spec.is_none() && s.image.is_none() && m.has_disks()
        })
    }

    /// The environments here that use a copy of an image: the ones pinned
    /// to it or keeping a rollback to it, and any with disks but no pin
    /// whose image is the same reference, which could have been made over
    /// any copy of it.
    fn image_users(&self, c: &ImageCopy) -> Vec<String> {
        let st = self.state.lock().unwrap();
        st.envs
            .iter()
            .filter(|(_, m)| {
                let s = m.st.lock().unwrap();
                if s.spec
                    .as_ref()
                    .is_some_and(|sp| sp.desired == DesiredState::Deleted)
                {
                    return false;
                }
                s.image.as_ref() == Some(c)
                    || s.rollback.as_ref().is_some_and(|r| r.copy() == *c)
                    || (s.image.is_none()
                        && s.spec.as_ref().is_some_and(|sp| sp.spec.image == c.r#ref)
                        && m.has_disks())
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// The agent's session with a running environment's guest, to ask it
    /// things.
    pub fn session(&self, id: &str) -> Option<Arc<crate::agent::Session>> {
        let m = self.state.lock().unwrap().envs.get(id).cloned()?;
        let st = m.st.lock().unwrap();
        st.running.as_ref().map(|i| i.session().clone())
    }

    /// Where an environment's machine writes the log of that name: only
    /// environments this runtime holds, and only those logs, so a request
    /// names no other file.
    pub fn log_file(&self, id: &str, name: &str) -> Option<PathBuf> {
        if !LOGS.contains(&name) {
            return None;
        }
        let st = self.state.lock().unwrap();
        st.envs.get(id).map(|m| m.dir.join(format!("{name}.log")))
    }

    /// Connects to a port of a running environment's guest: its terminals,
    /// desktop, Code service, profile session, processes, SSH or ports.
    pub async fn dial(&self, id: &str, port: u32) -> io::Result<GuestConn> {
        let m = self
            .state
            .lock()
            .unwrap()
            .envs
            .get(id)
            .cloned()
            .ok_or_else(|| err(format!("environment {id} is not on this worker")))?;
        let inst = {
            let s = m.st.lock().unwrap();
            s.running.clone().or_else(|| s.starting.clone())
        };
        let Some(inst) = inst else {
            return Err(err(format!("environment {id} is not running")));
        };
        inst.dial_guest(port).await
    }
}

/// Waits for a flag to be set.
async fn set(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|s| *s).await;
}

/// The store's name for a copy.
fn store_copy(c: &ImageCopy) -> hangar_images::ImageCopy {
    hangar_images::ImageCopy {
        reference: c.r#ref.clone(),
        digest: c.digest.clone(),
    }
}

/// Refuses to start on a host that cannot run environments at all, rather
/// than failing each one as it is asked for.
fn preflight(cfg: &Config) -> io::Result<()> {
    let caps = host::detect(&cfg.monitor)?;
    let on_path = |name: &str| {
        std::env::var_os("PATH")
            .iter()
            .flat_map(std::env::split_paths)
            .any(|d| d.join(name).is_file())
    };
    match caps.monitor {
        Monitor::Krun => {
            if cfg.monitor_bin.is_none() {
                krun::find()?;
            }
            if caps.os == "linux" && !on_path("passt") {
                return Err(err("passt not found (apt install passt)"));
            }
            cfg.mkfs()?;
            if cfg.image_device == ImageDevice::Pmem {
                return Err(err(
                    "libkrun has no persistent memory: set vm.image_device to disk",
                ));
            }
            // libkrun boots an x86_64 kernel as ELF alone, so a bzImage
            // would fail every boot.
            if caps.arch == "x86_64" {
                let mut magic = [0u8; 4];
                std::fs::File::open(&cfg.kernel)
                    .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut magic))
                    .map_err(|e| err(format!("guest kernel: {e}")))?;
                if &magic != b"\x7fELF" {
                    return Err(err(format!(
                        "guest kernel {} is not ELF, which libkrun needs on x86_64: name the vmlinux beside it",
                        cfg.kernel.display()
                    )));
                }
            }
        }
        Monitor::CloudHypervisor => {
            crate::hugepages::check()?;
            if cfg.monitor_bin.is_none() {
                ch::find()?;
            }
            if !on_path("passt") {
                return Err(err("passt not found (apt install passt)"));
            }
            if cfg.mkfs.is_none() && !on_path("mkfs.ext4") {
                return Err(err("mkfs.ext4 not found (apt install e2fsprogs)"));
            }
        }
    }
    std::fs::metadata(&cfg.kernel).map_err(|e| err(format!("guest kernel: {e}")))?;
    if cfg.agent.as_os_str().is_empty() {
        return Err(err(
            "no agent: every environment boots with the worker's (vm.agent)",
        ));
    }
    std::fs::metadata(&cfg.agent).map_err(|e| err(format!("agent: {e}")))?;
    for (r, base) in &cfg.images {
        let m = std::fs::metadata(base).map_err(|e| err(format!("image {r}: base: {e}")))?;
        if !m.is_dir() {
            return Err(err(format!(
                "image {r}: base {} is not a directory: an image is its root filesystem",
                base.display()
            )));
        }
    }
    Ok(())
}

/// Decides from a probe, or its failure, whether a virtual GPU is on offer:
/// not when the configuration asks for hardware and there is only the CPU,
/// nor when the host cannot render at all.
fn gpu_info(cfg: &Config, p: io::Result<ch::GpuProbe>) -> GpuInfo {
    let mut info = GpuInfo {
        renderer: if cfg.gpu_renderer.is_empty() {
            ch::RENDERER_AUTO.into()
        } else {
            cfg.gpu_renderer.clone()
        },
        device: cfg.gpu_device.clone(),
        ..Default::default()
    };
    let p = match p {
        Ok(p) => p,
        Err(e) => {
            info.unavailable = e.to_string();
            cfg.log.warn(
                "probing the GPU: environments are offered no virtual GPU",
                &[("err", &e)],
            );
            return info;
        }
    };
    info.gl = p.gl.map(|g| GpuRenderer {
        vendor: g.vendor,
        renderer: g.renderer,
        version: g.version,
        software: g.software,
    });
    info.gl_error = p.gl_error;
    info.vulkan_error = p.vulkan_error;
    info.vulkan = p
        .vulkan
        .unwrap_or_default()
        .into_iter()
        .map(|d| GpuDevice {
            name: d.name,
            software: d.software,
        })
        .collect();
    match &info.gl {
        None => {
            info.unavailable = format!("the host has no OpenGL to render with: {}", info.gl_error)
        }
        Some(gl) if info.renderer == ch::RENDERER_HARDWARE && gl.software => {
            info.unavailable = format!(
                "the configuration asks for a GPU to render on, and this host renders on its CPU ({})",
                gl.renderer
            )
        }
        _ => {}
    }
    match &info.gl {
        Some(gl) if info.unavailable.is_empty() => cfg.log.info(
            "virtual GPUs render with",
            &[("gl", &gl.renderer), ("software", &gl.software)],
        ),
        _ => cfg.log.warn(
            "environments are offered no virtual GPU",
            &[("why", &info.unavailable)],
        ),
    }
    info
}

/// What a machine under libkrun on macOS renders with: Vulkan through Venus
/// on the Mac's own GPU, by the driver hangar-krun chose, when its libkrun
/// was built with a GPU, and nothing otherwise. The GPU has no OpenGL of
/// its own: zink draws it in the guest where the driver can share images
/// between the guest's programs, and the guest's CPU otherwise.
async fn krun_gpu(cfg: &Config) -> (Option<GpuInfo>, bool) {
    let f = match krun::read_features(cfg.monitor_bin.as_deref()).await {
        Ok(f) => f,
        Err(e) => {
            cfg.log
                .warn("environments are offered no virtual GPU", &[("err", &e)]);
            return (None, false);
        }
    };
    if !f.gpu {
        cfg.log.info(
            "hangar-krun's libkrun was built without a GPU: environments are offered none",
            &[],
        );
        return (None, false);
    }
    let driver = if f.vulkan == krun::VULKAN_KOSMICKRISP {
        "KosmicKrisp"
    } else {
        "MoltenVK"
    };
    let device = format!("{} ({driver})", mac_gpu_name().await);
    let gl = if f.zink {
        GpuRenderer {
            vendor: "Mesa".into(),
            renderer: format!("zink on {device}"),
            ..Default::default()
        }
    } else {
        cfg.log.info(
            &format!(
                "OpenGL in environments is drawn on their CPU: {driver} cannot share images between a guest's programs (dev/krun-build.sh builds a KosmicKrisp that can)"
            ),
            &[],
        );
        GpuRenderer {
            vendor: "Mesa".into(),
            renderer: "llvmpipe (the guest's CPU)".into(),
            software: true,
            ..Default::default()
        }
    };
    (
        Some(GpuInfo {
            renderer: "hardware".into(),
            vulkan: vec![GpuDevice {
                name: device,
                software: false,
            }],
            gl: Some(gl),
            ..Default::default()
        }),
        f.zink,
    )
}

/// The Mac's GPU, by its name: its chip's, which is its GPU's.
async fn mac_gpu_name() -> String {
    let out = tokio::process::Command::new("sysctl")
        .args(["-n", "machdep.cpu.brand_string"])
        .output()
        .await;
    match out {
        Ok(o) if o.status.success() => {
            let name = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if name.is_empty() {
                "the Mac's GPU".into()
            } else {
                name
            }
        }
        _ => "the Mac's GPU".into(),
    }
}

fn read_image_pin(dir: &Path) -> Option<ImageCopy> {
    let b = std::fs::read(dir.join(IMAGE_PIN_FILE)).ok()?;
    let c: ImageCopy = serde_json::from_slice(&b).ok()?;
    (!c.r#ref.is_empty()).then_some(c)
}

/// Looks up, at the start and then every LOOK_UP_IMAGES_EVERY, what each
/// image reference an environment here is pinned to names, and tells the
/// server when one has moved on. A copy left superseded and unused is
/// removed.
async fn watch_images(rt: Weak<Runtime>, mut stop: watch::Receiver<bool>) {
    loop {
        let Some(r) = rt.upgrade() else { return };
        let refs: std::collections::BTreeSet<String> = r
            .state
            .lock()
            .unwrap()
            .envs
            .values()
            .filter_map(|m| m.st.lock().unwrap().image.as_ref().map(|i| i.r#ref.clone()))
            .collect();
        let store = r.store.clone();
        let log = r.cfg.log.clone();
        let changed = tokio::task::spawn_blocking(move || {
            let mut changed = false;
            for reference in refs {
                let before = store.latest(&reference);
                match store.current(&reference) {
                    Ok(d) => changed |= before.as_deref() != Some(d.as_str()),
                    Err(e) => {
                        log.debug("looking up an image", &[("image", &reference), ("err", &e)])
                    }
                }
            }
            changed
        })
        .await
        .unwrap_or(false);
        if changed {
            r.prune();
            r.notify();
        }
        drop(r);
        tokio::select! {
            _ = set(&mut stop) => return,
            _ = tokio::time::sleep(LOOK_UP_IMAGES_EVERY) => {}
        }
    }
}

/// Warns while the host backs shared memory, which guest memory is, with
/// 4 KiB pages for want of free 2 MB blocks. Nothing fails when it does,
/// and guests slow down with nothing else to show why.
async fn watch_huge_pages(rt: Weak<Runtime>, mut stop: watch::Receiver<bool>, log: Log) {
    let Ok(mut last) = crate::hugepages::read_counts() else {
        log.debug("huge page counts are unavailable", &[]);
        return;
    };
    let mut warned: Option<Instant> = None;
    let (mut fell_back, mut allocated) = (0u64, 0u64);
    loop {
        tokio::select! {
            _ = set(&mut stop) => return,
            _ = tokio::time::sleep(HUGE_PAGE_CHECK_EVERY) => {}
        }
        if rt.upgrade().is_none() {
            return;
        }
        let Ok(c) = crate::hugepages::read_counts() else {
            continue;
        };
        fell_back += c.fell_back.saturating_sub(last.fell_back);
        allocated += c.allocated.saturating_sub(last.allocated);
        last = c;
        if fell_back == 0 || warned.is_some_and(|w| w.elapsed() < HUGE_PAGE_WARN_EVERY) {
            continue;
        }
        log.warn(
            "the host had no free 2 MB blocks for some guest memory, which runs on 4 KiB pages and is slower; raise vm.min_free_kbytes or vm.compaction_proactiveness, or give the page cache less",
            &[("fell_back_mib", &(fell_back * 2)), ("huge_mib", &(allocated * 2))],
        );
        (warned, fell_back, allocated) = (Some(Instant::now()), 0, 0);
    }
}

struct MState {
    spec: Option<EnvironmentSpec>,
    phase: Phase,
    reason: String,
    /// The step a start is on, and how much of it is done.
    progress: Option<Progress>,
    /// When a measurement of progress was last announced, and how much was
    /// done then, for the rate.
    measured: Option<Instant>,
    measured_done: i64,
    /// The booted machine, while there is one; starting is it while it is
    /// still being set up, which it takes streams for too.
    running: Option<Arc<Instance>>,
    starting: Option<Arc<Instance>>,
    /// Interrupts a boot in progress when what is wanted changes.
    cancel_boot: Option<watch::Sender<bool>>,
    /// The latest measurement of the running machine.
    stats: Option<EnvironmentStats>,
    /// Set when suspending failed, so it is not tried again until something
    /// else is asked for.
    suspend_failed: bool,
    /// The memory the running machine was last asked to resize to, so a
    /// size it cannot take is tried once, not on every desired set.
    memory_asked: i64,
    /// Whether the running machine was last told to reclaim the memory its
    /// programs leave idle.
    reclaim_asked: bool,
    /// The copy of its image the environment was made over, once it has
    /// first started.
    image: Option<ImageCopy>,
    /// The writable disk kept from before the last upgrade.
    rollback: Option<Rollback>,
    /// What upgrading from the copy update_from names to a newer one would
    /// hide, once checked; checking says a check is under way.
    update: Option<ImageUpdate>,
    update_from: String,
    checking: bool,
}

impl Default for MState {
    fn default() -> MState {
        MState {
            spec: None,
            phase: Phase::Stopped,
            reason: String::new(),
            progress: None,
            measured: None,
            measured_done: 0,
            running: None,
            starting: None,
            cancel_boot: None,
            stats: None,
            suspend_failed: false,
            memory_asked: 0,
            reclaim_asked: false,
            image: None,
            rollback: None,
            update: None,
            update_from: String::new(),
            checking: false,
        }
    }
}

/// One environment and the task that looks after it.
pub(crate) struct Machine {
    rt: Weak<Runtime>,
    id: String,
    dir: PathBuf,
    log: Log,
    st: Mutex<MState>,
    /// Held while the writable disk is used other than by the machine:
    /// compared with a newer image, copied, or restored.
    disk: tokio::sync::Mutex<()>,
    nudge: Notify,
}

/// Why a start stopped short.
enum Stop {
    /// What is wanted changed.
    Cancelled,
    Failed(io::Error),
}

impl From<io::Error> for Stop {
    fn from(e: io::Error) -> Stop {
        Stop::Failed(e)
    }
}

impl Reporter for Machine {
    fn step(&self, step: Step, what: &str) {
        self.step_to(step, what);
    }

    fn measure(&self, done: i64, total: i64, unit: &str) {
        self.measure_to(done, total, unit);
    }
}

impl Machine {
    fn rt(&self) -> Option<Arc<Runtime>> {
        self.rt.upgrade()
    }

    fn notify(&self) {
        if let Some(rt) = self.rt() {
            rt.notify();
        }
    }

    fn set(&self, phase: Phase, reason: &str) {
        let changed = {
            let mut st = self.st.lock().unwrap();
            let changed = st.phase != phase || st.reason != reason;
            st.phase = phase;
            st.reason = reason.to_string();
            if phase != Phase::Starting {
                st.progress = None;
            }
            changed
        };
        if changed {
            let p = serde_json::to_value(phase)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default();
            self.log
                .info("environment", &[("phase", &p), ("reason", &reason)]);
            self.notify();
        }
    }

    /// Records the step a start is on, with the reason to show for it. A
    /// new step, or a new part of one -- a clone going from receiving
    /// objects to resolving deltas -- is measured afresh.
    fn step_to(&self, step: Step, reason: &str) {
        {
            let mut st = self.st.lock().unwrap();
            if st.progress.as_ref().is_none_or(|p| p.step != step) || st.reason != reason {
                st.progress = Some(Progress {
                    step,
                    done: 0,
                    total: 0,
                    unit: String::new(),
                    rate: 0.0,
                });
                st.measured = None;
                st.measured_done = 0;
            }
        }
        self.set(Phase::Starting, reason);
    }

    /// Records how much of the current step is done. It is announced at
    /// most every MEASURE_EVERY, and when the step completes.
    fn measure_to(&self, done: i64, total: i64, unit: &str) {
        let announce = {
            let mut st = self.st.lock().unwrap();
            if st.phase != Phase::Starting {
                return;
            }
            let (measured, measured_done) = (st.measured, st.measured_done);
            let Some(p) = st.progress.as_mut() else {
                return;
            };
            p.done = done;
            p.total = total;
            p.unit = unit.to_string();
            let now = Instant::now();
            let since = measured.map(|m| now - m);
            let announce = since.is_none_or(|s| s >= MEASURE_EVERY) || (total > 0 && done >= total);
            if announce {
                // The rate is smoothed over the last few announcements, so
                // it follows a download that speeds up or stalls without
                // jumping. The first measurement of a step is where its rate
                // is measured from.
                if let Some(s) = since
                    && !s.is_zero()
                    && done >= measured_done
                {
                    let latest = (done - measured_done) as f64 / s.as_secs_f64();
                    p.rate = if p.rate == 0.0 {
                        latest
                    } else {
                        0.7 * p.rate + 0.3 * latest
                    };
                }
                st.measured = Some(now);
                st.measured_done = done;
            }
            announce
        };
        if announce {
            self.notify();
        }
    }

    /// Records what the environment should be doing and wakes its
    /// controller. A boot in progress is interrupted if it stops being
    /// wanted.
    fn want(&self, spec: EnvironmentSpec) {
        {
            let mut st = self.st.lock().unwrap();
            if st.spec.as_ref().is_none_or(|s| s.desired != spec.desired) {
                st.suspend_failed = false;
            }
            if spec.desired != DesiredState::Running
                && let Some(c) = &st.cancel_boot
            {
                let _ = c.send(true);
            }
            st.spec = Some(spec);
        }
        self.nudge.notify_one();
    }

    fn has_disks(&self) -> bool {
        self.dir.join("upper.ext4").exists()
    }

    /// The environment's controller. It acts on the latest spec after
    /// every nudge, and on the machine exiting.
    async fn control(self: Arc<Self>, mut stop: watch::Receiver<bool>) {
        loop {
            let running = self.st.lock().unwrap().running.clone();
            tokio::select! {
                _ = set(&mut stop) => {
                    self.power_off().await;
                    return;
                }
                _ = self.nudge.notified() => {}
                _ = async { running.as_ref().unwrap().exited().await }, if running.is_some() => {
                    self.lost().await;
                    continue;
                }
            }
            let (spec, running, failed) = {
                let st = self.st.lock().unwrap();
                (
                    st.spec.clone(),
                    st.running.is_some(),
                    st.phase == Phase::Failed,
                )
            };
            let Some(spec) = spec else { continue };
            if spec.desired != DesiredState::Deleted {
                self.drop_rollback(&spec).await;
            }
            match spec.desired {
                DesiredState::Running => {
                    // A failed environment stays failed until it is stopped
                    // and started again: retrying on every desired set would
                    // hide the failure in a loop.
                    if !running && !failed {
                        self.boot(&spec, &stop).await;
                    }
                    self.fit_memory().await;
                    self.fit_reclaim().await;
                }
                DesiredState::Suspended => {
                    // Only a running machine has memory to keep. One that is
                    // not stays as it is: stopped, failed, or already
                    // suspended.
                    let tried = self.st.lock().unwrap().suspend_failed;
                    if running && !tried {
                        match self.suspend().await {
                            // Tried once per request: the guest runs on,
                            // saying why, until it is asked for something
                            // else.
                            Err(e) => {
                                self.log.warn("suspending", &[("err", &e)]);
                                self.st.lock().unwrap().suspend_failed = true;
                                self.set(Phase::Running, &format!("could not suspend: {e}"));
                            }
                            Ok(()) => self.set(Phase::Suspended, ""),
                        }
                    }
                }
                DesiredState::Stopped => {
                    if running {
                        self.set(Phase::Stopping, "");
                        self.power_off().await;
                    }
                    suspend::discard(&self.dir);
                    self.set(Phase::Stopped, "");
                }
                DesiredState::Deleted => {
                    self.set(Phase::Deleting, "");
                    self.power_off().await;
                    let _ = std::fs::remove_dir_all(
                        std::env::temp_dir().join(format!("hangar-{}", self.id)),
                    );
                    if let Err(e) = std::fs::remove_dir_all(&self.dir)
                        && e.kind() != io::ErrorKind::NotFound
                    {
                        self.set(Phase::Failed, &format!("removing its disks: {e}"));
                        continue;
                    }
                    if let Some(rt) = self.rt() {
                        rt.forget(&self.id);
                    }
                    return;
                }
                DesiredState::Other => {}
            }
        }
    }

    /// Starts the machine and provisions it, reporting each step.
    async fn boot(self: &Arc<Self>, spec: &EnvironmentSpec, stop: &watch::Receiver<bool>) {
        let (tx, cancel) = watch::channel(false);
        self.st.lock().unwrap().cancel_boot = Some(tx);
        let started = self.start(spec, cancel).await;
        self.st.lock().unwrap().cancel_boot = None;
        let inst = match started {
            Ok(i) => i,
            Err(e) => {
                match e {
                    // Interrupted because what is wanted changed; the
                    // controller deals with what is wanted instead.
                    Stop::Cancelled if !*stop.borrow() => self.set(Phase::Stopped, ""),
                    Stop::Cancelled => self.set(Phase::Failed, "the worker stopped"),
                    Stop::Failed(e) => self.set(Phase::Failed, &e.to_string()),
                }
                self.nudge.notify_one();
                return;
            }
        };
        {
            let mut st = self.st.lock().unwrap();
            st.running = Some(inst.clone());
            st.memory_asked = spec.spec.memory_mib;
        }
        self.set(Phase::Running, "");
        let m = self.clone();
        let cpus = spec.spec.cpus.max(0) as u32;
        tokio::spawn(async move { m.sample(inst, cpus).await });
    }

    /// Boots the environment and provisions it, or resumes it if it is
    /// suspended. On error, everything it started has been stopped again.
    async fn start(
        self: &Arc<Self>,
        spec: &EnvironmentSpec,
        cancel: watch::Receiver<bool>,
    ) -> Result<Arc<Instance>, Stop> {
        let rt = self.rt().ok_or(Stop::Cancelled)?;
        let s = &spec.spec;
        if s.gpu == Gpu::Passthrough {
            return Err(err("GPU passthrough is not supported by this worker").into());
        }
        if s.gpu == Gpu::Virtual {
            match &rt.gpu {
                None => {
                    return Err(err(
                        "this worker has no virtual GPU, and the template asks for one",
                    )
                    .into());
                }
                Some(g) if !g.unavailable.is_empty() => {
                    return Err(err(format!(
                        "this worker offers no virtual GPU: {}",
                        g.unavailable
                    ))
                    .into());
                }
                _ => {}
            }
        }
        let cancelled = |c: &watch::Receiver<bool>| *c.borrow();
        // A check of what an upgrade would hide stands down once the phase
        // is starting, and one under way is waited for: both use the
        // writable disk.
        {
            let _disk = self.disk.lock().await;
            self.set(Phase::Starting, "starting");
            self.change_image(spec).await?;
        }
        let img = self.resolve(spec, &cancel).await?;
        if s.display == Display::Desktop {
            // An image that says it has no desktop would boot to one that
            // never appears; one that does not say is taken at its word.
            let info = image_info(&img).map_err(|e| err(format!("image {}: {e}", s.image)))?;
            if info.is_some_and(|i| !i.desktop) {
                return Err(err(format!(
                    "image {} has no desktop, and the template asks for one: use an image with a desktop, or set the display to none",
                    s.image
                ))
                .into());
            }
        }
        self.step_to(Step::Disks, "preparing its disks");
        std::fs::create_dir_all(&self.dir)?;
        // One writable disk holds both the root's layer and Docker's store,
        // which the agent mounts at /var/lib/docker beside the root rather
        // than in it: overlay2 cannot stack on the overlay root.
        let upper = self.dir.join("upper.ext4");
        provision::ensure_disk(
            &upper,
            "hangar-upper",
            rt.cfg.upper_gib + rt.cfg.docker_gib,
            &rt.cfg.mkfs()?,
        )
        .await?;
        // Built once per copy of the image, and shared by every machine on
        // it.
        self.step_to(Step::Disks, "preparing the image");
        let base_file = {
            let (store, img) = (rt.store.clone(), img.clone());
            tokio::task::spawn_blocking(move || store.erofs(&img))
                .await
                .map_err(io::Error::other)?
                .map_err(|e| err(format!("image {}: {e}", s.image)))?
        };
        self.step_to(Step::Disks, "preparing its disks");
        self.make_room_for_huge_pages(s.memory_mib.max(0) as u64);
        if cancelled(&cancel) {
            return Err(Stop::Cancelled);
        }
        let device = rt.cfg.image_device;
        let swap = match s.swap_mib {
            Some(n) => n.max(0) as u64,
            None => (s.memory_mib.max(0) as f64 * rt.cfg.swap) as u64,
        };
        let (memory_max_mib, files) = {
            let st = rt.state.lock().unwrap();
            (st.memory_max_mib, st.files.clone())
        };
        let me = self.clone();
        let mut cfg = InstanceConfig {
            id: self.id.clone(),
            name: spec.name.clone(),
            monitor: rt.monitor.name().into(),
            monitor_bin: rt.cfg.monitor_bin.clone(),
            gvproxy: rt.cfg.gvproxy.clone(),
            dir: self.dir.clone(),
            kernel: rt.cfg.kernel.clone(),
            agent: rt.cfg.agent.clone(),
            base: img.base.clone().unwrap_or_default(),
            base_file,
            base_device: device,
            base_dax: device == ImageDevice::Pmem && s.dax,
            // The writable layer is the first disk: the agent, as init,
            // mounts /dev/vda.
            disks: vec![ch::Disk {
                path: upper,
                ..Default::default()
            }],
            memory_mib: s.memory_mib.max(0) as u32,
            memory_max_mib,
            swap_mib: swap,
            cpus: s.cpus.max(0) as u32,
            net: true,
            gpu: s.gpu == Gpu::Virtual,
            console_file: self.dir.join("console.log"),
            agent_wait: rt.cfg.boot_timeout,
            progress: Some(Arc::new(move |step: &str| me.step_to(Step::Boot, step))),
            log: self.log.clone(),
            shared_files: files.map(|(root, view)| (root, view(&self.id))),
            cancel: Some(cancel.clone()),
            ..Default::default()
        };
        if cfg.gpu {
            cfg.gpu_venus = rt.cfg.gpu_venus;
            cfg.gpu_venus_restore = rt.cfg.gpu_venus && rt.cfg.gpu_venus_restore;
            cfg.gpu_window_mib = rt.cfg.gpu_window_mib;
            cfg.gpu_env = rt.gpu_env.clone();
            cfg.gpu_zink = rt.gpu_zink;
        }
        // An image may have a desktop; the template decides whether it
        // runs. A headless environment boots to multi-user.target, which
        // leaves out everything graphical whatever the image holds, and
        // masks the desktop so nothing can pull it in.
        cfg.extra_cmdline = if s.display == Display::None {
            "systemd.unit=multi-user.target systemd.mask=hangar-desktop.service".into()
        } else {
            "systemd.unit=graphical.target".into()
        };
        drop(rt);
        let inst = match Instance::boot(cfg).await {
            Ok(i) => Arc::new(i),
            Err(_) if cancelled(&cancel) => return Err(Stop::Cancelled),
            Err(e) => return Err(e.into()),
        };
        // The machine takes streams while it is set up: a clone over SSH
        // signs with the owner's keys, which the server sends over the
        // profile session it opens as soon as the machine will have one.
        self.st.lock().unwrap().starting = Some(inst.clone());
        let set_up = self.set_up(&inst, spec, cancel).await;
        self.st.lock().unwrap().starting = None;
        if let Err(e) = set_up {
            inst.shutdown().await;
            return Err(e);
        }
        Ok(inst)
    }

    /// What a booted machine waits for before it is running: the Code tab's
    /// trust, the network, the profile, and the template applied.
    async fn set_up(
        &self,
        inst: &Instance,
        spec: &EnvironmentSpec,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<(), Stop> {
        let sess = inst.session();
        let work = async {
            if let Err(e) = provision::write_code_trust(sess, &spec.spec).await {
                self.log.warn(
                    "could not tell the Code tab which folders to trust",
                    &[("err", &e)],
                );
            }
            self.st.lock().unwrap().reclaim_asked = spec.spec.memory_reclaim;
            if let Err(e) = provision::write_reclaim(sess, spec.spec.memory_reclaim).await {
                self.log.warn(
                    "could not tell the guest whether to reclaim idle memory",
                    &[("err", &e)],
                );
            }
            provision::wait_network(sess, self).await?;
            provision::wait_profile(sess, self, &self.log).await;
            provision::provision(sess, &self.dir, spec, self, &self.log).await
        };
        tokio::select! {
            r = work => r.map_err(Stop::Failed),
            _ = set(&mut cancel) => Err(Stop::Cancelled),
        }
    }

    /// The copy of its image the environment boots from, fetched into the
    /// store first if need be.
    ///
    /// On its first start that is whatever the image's reference names
    /// then, and the environment is pinned to it: every later start boots
    /// the same copy, whatever the reference has moved on to, because the
    /// environment's writable layer was made over it.
    async fn resolve(
        self: &Arc<Self>,
        spec: &EnvironmentSpec,
        cancel: &watch::Receiver<bool>,
    ) -> Result<hangar_images::Image, Stop> {
        let rt = self.rt().ok_or(Stop::Cancelled)?;
        let reference = spec.spec.image.clone();
        let pinned = self.st.lock().unwrap().image.clone();
        let fresh = pinned.as_ref().is_none_or(|p| p.r#ref != reference);
        let pin = match pinned {
            Some(p) if !fresh => p,
            _ => self.choose(&rt, &reference).await?,
        };
        let img = self.fetch(&rt, &pin, cancel).await?;
        if fresh {
            std::fs::create_dir_all(&self.dir)?;
            std::fs::write(
                self.dir.join(IMAGE_PIN_FILE),
                serde_json::to_vec(&pin).map_err(io::Error::other)?,
            )?;
            self.st.lock().unwrap().image = Some(pin);
            rt.prune();
            rt.notify();
        }
        Ok(img)
    }

    /// Fetches a copy into the store, reporting it as the start's download
    /// and unpack steps. A start interrupted meanwhile goes at once; the
    /// fetch finishes on its own, into the store.
    async fn fetch(
        self: &Arc<Self>,
        rt: &Arc<Runtime>,
        c: &ImageCopy,
        cancel: &watch::Receiver<bool>,
    ) -> Result<hangar_images::Image, Stop> {
        let (store, sc, me) = (rt.store.clone(), store_copy(c), self.clone());
        let got = tokio::task::spawn_blocking(move || {
            store.get(&sc, &mut |p: hangar_images::Progress| me.fetch_progress(p))
        });
        let mut cancel = cancel.clone();
        tokio::select! {
            r = got => Ok(r.map_err(io::Error::other)??),
            _ = set(&mut cancel) => Err(Stop::Cancelled),
        }
    }

    /// Picks the copy of a reference an environment not yet pinned to one
    /// is made over: what it names at its first start, or, when that cannot
    /// be looked up, the newest copy held. An environment with disks but no
    /// pin was made over a copy nothing recorded, and is given the oldest
    /// held, the one taken first.
    async fn choose(&self, rt: &Arc<Runtime>, reference: &str) -> io::Result<ImageCopy> {
        let copy = |digest: String| ImageCopy {
            r#ref: reference.to_string(),
            digest,
        };
        if self.has_disks()
            && let Some(d) = rt.store.oldest(reference)
        {
            return Ok(copy(d));
        }
        let (store, r) = (rt.store.clone(), reference.to_string());
        let current = tokio::task::spawn_blocking(move || store.current(&r))
            .await
            .map_err(io::Error::other)?;
        match current {
            Ok(d) => Ok(copy(d)),
            Err(e) => match rt.store.newest(reference) {
                Some(held) => {
                    self.log.warn(
                        "looking up the image; using the newest copy held",
                        &[("image", &reference), ("err", &e)],
                    );
                    Ok(copy(held))
                }
                None => Err(e),
            },
        }
    }

    /// Reports fetching a copy of the image as the start's download and
    /// unpack steps.
    fn fetch_progress(&self, p: hangar_images::Progress) {
        use hangar_images::Stage;
        match p.stage {
            Stage::Copy => self.step_to(Step::Download, "copying the image"),
            Stage::Download => {
                self.step_to(Step::Download, "downloading the image");
                self.measure_to(p.done as i64, p.total as i64, "bytes");
            }
            Stage::Unpack => {
                let mut reason = "unpacking the image".to_string();
                if p.layers > 1 {
                    reason += &format!(": layer {} of {}", p.layer, p.layers);
                }
                self.step_to(Step::Unpack, &reason);
                self.measure_to(p.done as i64, p.total as i64, "bytes");
            }
        }
    }

    /// Compacts the host's memory when it has too few free huge-page-sized
    /// blocks for a guest of mib. A guest's memory that finds none runs on
    /// 4 KiB pages, which is slower on any host and, under nested
    /// virtualisation, hundreds of times slower to touch; a page cache that
    /// has grown through the host's free memory is the usual reason, and
    /// compaction moves it aside in about a second.
    fn make_room_for_huge_pages(&self, mib: u64) {
        let Ok(free) = crate::hugepages::free_huge_blocks_mib() else {
            return;
        };
        if free >= mib {
            return;
        }
        let start = Instant::now();
        if let Err(e) = crate::hugepages::compact_memory() {
            // A container that is not privileged has /proc/sys read-only,
            // which no later boot changes: said once, with what stands in.
            static READ_ONLY_SAID: AtomicBool = AtomicBool::new(false);
            if e.kind() != io::ErrorKind::ReadOnlyFilesystem {
                self.log.warn(
                    "compacting the host's memory for the guest's huge pages",
                    &[("err", &e)],
                );
            } else if !READ_ONLY_SAID.swap(true, Ordering::Relaxed) {
                self.log.warn(
                    "the host's memory cannot be compacted from here (/proc/sys is read-only), so its vm.compaction_proactiveness and vm.min_free_kbytes keep blocks free for the guests' huge pages",
                    &[],
                );
            }
            return;
        }
        let after = crate::hugepages::free_huge_blocks_mib().unwrap_or(0);
        self.log.info(
            "compacted the host's memory for the guest's huge pages",
            &[
                ("free_huge_mib_before", &free),
                ("free_huge_mib_after", &after),
                ("guest_mib", &mib),
                ("took", &format!("{:?}", start.elapsed())),
            ],
        );
    }

    /// Resizes the running machine's memory to what its spec says, if it
    /// has not been asked to already. One that booted with no room to
    /// resize takes the size when it next starts.
    async fn fit_memory(&self) {
        let (inst, want) = {
            let mut st = self.st.lock().unwrap();
            let (Some(inst), Some(spec)) = (st.running.clone(), st.spec.as_ref()) else {
                return;
            };
            let want = spec.spec.memory_mib;
            if want == st.memory_asked {
                return;
            }
            st.memory_asked = want;
            (inst, want)
        };
        match inst.resize_memory(want.max(0) as u32).await {
            Ok(asked) => self.log.info("resizing memory", &[("mib", &asked)]),
            Err(e) => self
                .log
                .warn("resizing memory", &[("mib", &want), ("err", &e)]),
        }
    }

    /// Tells the running machine's agent whether to reclaim the memory its
    /// programs leave idle, when its spec says other than it was last told.
    /// A guest takes the change at once.
    async fn fit_reclaim(&self) {
        let (inst, want) = {
            let mut st = self.st.lock().unwrap();
            let (Some(inst), Some(spec)) = (st.running.clone(), st.spec.as_ref()) else {
                return;
            };
            let want = spec.spec.memory_reclaim;
            if want == st.reclaim_asked {
                return;
            }
            st.reclaim_asked = want;
            (inst, want)
        };
        match provision::write_reclaim(inst.session(), want).await {
            Ok(()) => self.log.info("reclaiming idle memory", &[("on", &want)]),
            Err(e) => self.log.warn(
                "telling the guest whether to reclaim idle memory",
                &[("err", &e)],
            ),
        }
    }

    /// Handles a machine that exited without being asked to.
    async fn lost(&self) {
        let Some(inst) = self.st.lock().unwrap().running.take() else {
            return;
        };
        if inst.rebooted() {
            inst.close().await;
            if inst.panicked() {
                self.log.warn(
                    "the guest's kernel panicked; booting it again (its console log says why)",
                    &[],
                );
                self.set(Phase::Starting, "rebooting after a kernel panic");
            } else {
                self.log.info("the guest rebooted; booting it again", &[]);
                self.set(Phase::Starting, "rebooting");
            }
            self.nudge.notify_one();
            return;
        }
        let reason = inst
            .err()
            .unwrap_or_else(|| "the machine stopped on its own".into());
        inst.close().await;
        self.set(Phase::Failed, &reason);
    }

    /// Stops the machine if one is running, cleanly if it can.
    async fn power_off(&self) {
        let inst = self.st.lock().unwrap().running.take();
        if let Some(inst) = inst {
            inst.shutdown().await;
        }
    }

    /// Writes the running machine to disk and stops it. If it cannot, the
    /// guest carries on running as it was.
    async fn suspend(&self) -> io::Result<()> {
        // Taken from the controller before it goes, so its exit is not seen
        // as the machine stopping on its own.
        let Some(inst) = self.st.lock().unwrap().running.take() else {
            return Err(err("the machine is not running"));
        };
        self.set(Phase::Suspending, "");
        let start = Instant::now();
        match tokio::time::timeout(Duration::from_secs(600), inst.suspend()).await {
            Ok(Ok(())) => {
                self.log
                    .info("suspended", &[("seconds", &start.elapsed().as_secs_f64())]);
                Ok(())
            }
            Ok(Err(e)) => {
                self.st.lock().unwrap().running = Some(inst);
                Err(e)
            }
            Err(_) => {
                self.st.lock().unwrap().running = Some(inst);
                Err(err("suspending took more than ten minutes"))
            }
        }
    }

    /// Measures a running machine until it exits, keeping the latest
    /// figures for observe to report.
    async fn sample(self: Arc<Self>, inst: Arc<Instance>, cpus: u32) {
        let mut prev: Option<Counters> = None;
        // Since when the guest has had more memory than it was asked to.
        let mut shrinking: Option<Instant> = None;
        loop {
            let mut now = Counters::new(Instant::now());
            let mut st = EnvironmentStats {
                disk_used_bytes: stats::allocated(&self.dir.join("upper.ext4")) as i64,
                swap_disk_bytes: stats::allocated(&self.dir.join("swap.img")) as i64,
                ..Default::default()
            };
            let pid = inst.pid();
            if pid != 0 {
                now.cpu_ticks = stats::process_ticks(pid);
                st.host_memory_mib = (stats::guest_ram(pid) >> 20) as i64;
            }
            if let Ok(Ok(mem)) = tokio::time::timeout(Duration::from_secs(5), inst.memory()).await {
                st.machine_memory_mib = mem.actual as i64;
                if mem.resizable() {
                    st.machine_memory_min_mib = mem.boot as i64;
                    st.machine_memory_max_mib = mem.max as i64;
                }
                if mem.requested >= mem.actual {
                    shrinking = None;
                } else if let Some(since) = shrinking {
                    if now.at - since > SHRINK_PATIENCE {
                        self.log.warn(
                            "the guest could not give back the memory asked of it; it keeps what it has",
                            &[("asked_mib", &mem.requested), ("has_mib", &mem.actual)],
                        );
                        if let Err(e) = inst.resize_memory(mem.actual).await {
                            self.log
                                .warn("resizing memory", &[("mib", &mem.actual), ("err", &e)]);
                        }
                        shrinking = None;
                    }
                } else {
                    shrinking = Some(now.at);
                }
            }
            let (gpu, others) = inst.support_pids();
            let host = tokio::task::spawn_blocking(HostProcs::read)
                .await
                .unwrap_or_default();
            if let Some(g) = gpu {
                now.gpu_ticks = host.tree_ticks(g);
            }
            now.sup_ticks = now.gpu_ticks + others.iter().map(|p| host.tree_ticks(*p)).sum::<u64>();
            if pid != 0 {
                st.monitor_memory_mib = (host.tree_memory(pid) >> 20) as i64;
            }
            let support: u64 = others
                .iter()
                .chain(gpu.iter())
                .map(|p| host.tree_memory(*p))
                .sum();
            st.support_memory_mib = (support >> 20) as i64;
            let sess = inst.session();
            if let Ok(out) = sess
                .exec(
                    Duration::from_secs(10),
                    &[
                        "cat",
                        "/proc/meminfo",
                        "/proc/net/dev",
                        "/proc/diskstats",
                        "/proc/vmstat",
                    ],
                )
                .await
                && out.code == 0
            {
                stats::guest(&out.stdout, &mut st, &mut now);
            }
            let page = if sess.hello.page_size > 0 {
                sess.hello.page_size as f64
            } else {
                4096.0
            };
            if let Some(p) = &prev {
                stats::rates(p, &now, cpus, page, &mut st);
                self.st.lock().unwrap().stats = Some(st);
            }
            prev = Some(now);
            tokio::select! {
                _ = inst.exited() => {
                    self.st.lock().unwrap().stats = None;
                    return;
                }
                _ = tokio::time::sleep(SAMPLE_EVERY) => {}
            }
        }
    }
}

/// What an image says about itself, if it says: a local build by its root
/// filesystem, a pulled copy by its EROFS image.
fn image_info(img: &hangar_images::Image) -> io::Result<Option<hangar_images::ImageInfo>> {
    match &img.base {
        Some(base) => hangar_images::read_image_info(base),
        None => hangar_images::read_image_info_erofs(&img.copy.join(hangar_images::EROFS_FILE)),
    }
}
