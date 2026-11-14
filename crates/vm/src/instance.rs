//! A running machine: the monitor, its backends, and the agent's session
//! with the guest.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use tokio::sync::{mpsc, watch};

use crate::agent;
use crate::ch::{self, Api, GpuBackend, Memory, Passt};
use crate::err;
use crate::files::{SHARED_FILES_PORT, SharedFiles};
use crate::guest::{self, GuestConn};
use crate::host::{self, Caps, Monitor};
use crate::krun;
use crate::log::Log;
use crate::suspend::{self, KrunMemory, Record, SNAPSHOT_DIR};
use crate::vsock::hybrid_path;

/// The context ID every guest is given. Each monitor carries its guest's
/// vsock on unix sockets of its own, so the number is local to them and
/// never has to be unique across the host.
pub const GUEST_CID: u32 = 3;

/// The vsock ports the worker reaches a guest's services on: terminals,
/// the desktop, the Code service, the profile session, processes, SSH and
/// the guest's own ports. libkrun carries each on a unix socket of its
/// own, which it listens on.
pub const GUEST_PORTS: [u32; 7] = [101, 103, 104, 105, 106, 107, 108];

/// The address of a libkrun guest's network device.
pub const GUEST_MAC: &str = "5a:94:ef:e4:0c:ee";

/// The descriptor hangar-krun has the network's socket as, beside its
/// replies' 3.
const NET_FD: i32 = 4;

/// How the base reaches the guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageDevice {
    /// An EROFS image of the base as a read-only virtio-blk disk. The guest
    /// answers lookups from its own cache and reads in large requests; each
    /// guest caches what it reads itself.
    #[default]
    Disk,
    /// The EROFS image as virtio-pmem. An environment that asks for DAX
    /// maps the host's page cache pages directly, shared by every guest on
    /// the image. The first touch of each page faults to the host, which is
    /// costly under nested virtualisation.
    Pmem,
}

impl ImageDevice {
    pub fn name(self) -> &'static str {
        match self {
            ImageDevice::Disk => "disk",
            ImageDevice::Pmem => "pmem",
        }
    }

    pub fn parse(s: &str) -> Option<ImageDevice> {
        match s {
            "" | "disk" => Some(ImageDevice::Disk),
            "pmem" => Some(ImageDevice::Pmem),
            _ => None,
        }
    }
}

/// Told each step of a boot.
pub type Progress = Arc<dyn Fn(&str) + Send + Sync>;

/// One machine: what it boots, what it is given, and where it keeps what
/// it keeps.
#[derive(Clone, Default)]
pub struct InstanceConfig {
    /// Names the machine's sockets; unique on the host.
    pub id: String,
    /// The monitor's name for the machine.
    pub name: String,
    /// The monitor ("cloud-hypervisor", "krun"); empty is the host's own.
    pub monitor: String,
    /// The monitor's program; none finds it (ch::find, krun::find).
    pub monitor_bin: Option<PathBuf>,
    /// gvproxy, a libkrun guest's network on macOS; none finds it.
    pub gvproxy: Option<PathBuf>,
    /// Holds what outlives a boot: the backends' saved state, a suspended
    /// machine's snapshot, and the monitor's log.
    pub dir: PathBuf,
    pub kernel: PathBuf,
    /// The guest agent, each boot's initramfs and then its init.
    pub agent: PathBuf,
    /// The image's root filesystem, when it is a directory.
    pub base: PathBuf,
    /// An EROFS image of the base, attached as base_device, which the guest
    /// mounts with DAX if base_dax.
    pub base_file: PathBuf,
    pub base_device: ImageDevice,
    pub base_dax: bool,
    /// The writable layer first, which the agent mounts as /dev/vda, then
    /// any read-only ones.
    pub disks: Vec<ch::Disk>,
    pub memory_mib: u32,
    /// Above memory_mib, what the memory can be resized up to while the
    /// machine runs.
    pub memory_max_mib: u32,
    /// Sizes the machine's swap disk, swap.img in dir; 0 gives it none.
    pub swap_mib: u64,
    pub cpus: u32,
    /// Outbound networking.
    pub net: bool,
    pub gpu: bool,
    /// Offers Vulkan, with gpu.
    pub gpu_venus: bool,
    /// Carries Vulkan state across a suspend.
    pub gpu_venus_restore: bool,
    pub gpu_window_mib: u32,
    /// The GPU backend's environment, which chooses its renderer.
    pub gpu_env: Vec<(String, String)>,
    /// Draws the guest's OpenGL with Mesa's zink on its Vulkan, under
    /// libkrun on macOS, where the GPU has no OpenGL of its own; the guest's
    /// CPU draws it otherwise.
    pub gpu_zink: bool,
    /// Receives the guest's console.
    pub console_file: PathBuf,
    pub extra_cmdline: String,
    /// The root of the files environments share, which the guest mounts
    /// over NFS on vsock, served as the view says; none serves none.
    pub shared_files: Option<(PathBuf, Arc<dyn hangar_nfs::View>)>,
    /// How long the agent has to dial back.
    pub agent_wait: Duration,
    /// Set to true when the boot stops being wanted: one waiting for its
    /// agent gives up, and what it started is stopped again.
    pub cancel: Option<watch::Receiver<bool>>,
    pub progress: Option<Progress>,
    pub log: Log,
}

impl InstanceConfig {
    /// Where the machine's sockets are, readable only by its owner.
    pub fn run_dir(&self) -> PathBuf {
        self.dir.join("run")
    }

    fn progress(&self, step: &str) {
        if let Some(p) = &self.progress {
            p(step);
        }
    }

    /// What the machine runs against.
    fn record(&self) -> Record {
        Record {
            base: suspend::fingerprint(&self.base),
            image: format!(
                "{}:{}:{}",
                self.base_device.name(),
                self.base_dax,
                suspend::fingerprint(&self.base_file)
            ),
            read_only: self
                .disks
                .iter()
                .filter(|d| d.read_only)
                .map(|d| suspend::fingerprint(&d.path))
                .collect(),
            krun_memory: None,
        }
    }

    /// The next disk's device, after those the guest already knows by
    /// position and label, so none of them moves.
    fn next_disk(&self, disks: usize) -> String {
        format!("vd{}", (b'a' + disks as u8) as char)
    }
}

/// Why and how the monitor exited.
#[derive(Default)]
struct Exit {
    err: Option<String>,
    rebooted: bool,
    panicked: bool,
}

/// The monitor's process.
struct Process {
    pid: u32,
    exited: watch::Receiver<bool>,
    /// Set when the worker stops it, so its exit is not a failure.
    stopping: Arc<AtomicBool>,
}

impl Process {
    fn signal(&self, sig: libc::c_int) {
        if !*self.exited.borrow() {
            // SAFETY: kill has no memory preconditions; the process is not
            // reaped while exited is false, so the ID is still its own.
            unsafe { libc::kill(self.pid as libc::pid_t, sig) };
        }
    }

    async fn wait(&self) {
        let mut rx = self.exited.clone();
        let _ = rx.wait_for(|e| *e).await;
    }

    async fn wait_for(&self, t: Duration) -> bool {
        tokio::time::timeout(t, self.wait()).await.is_ok()
    }

    /// Stops it, as asked: SIGTERM, and after 15 seconds SIGKILL.
    async fn stop(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        self.signal(libc::SIGTERM);
        if !self.wait_for(Duration::from_secs(15)).await {
            self.signal(libc::SIGKILL);
            self.wait().await;
        }
    }
}

/// How the worker asks hangar-krun things, one request at a time: the
/// requests it writes, and the answers, a line each. stale counts answers
/// still to come to requests given up on.
struct KrunPipe {
    requests: std::fs::File,
    answers: mpsc::UnboundedReceiver<String>,
    stale: usize,
}

/// A running machine.
pub struct Instance {
    cfg: InstanceConfig,
    monitor: Monitor,
    process: Process,
    exit: Arc<Mutex<Exit>>,
    api: Option<Api>,
    session: Arc<agent::Session>,
    /// Kept while the machine runs, so an agent restarted in the guest
    /// finds the host listening.
    _server: Option<agent::Server>,
    resumed: bool,
    /// The base of the unix sockets the guest's vsock is carried on.
    vsock: PathBuf,
    gpu: Option<GpuBackend>,
    net: Option<Passt>,
    mac_net: Option<crate::gvproxy::Network>,
    files: Option<SharedFiles>,
    krun: Option<tokio::sync::Mutex<KrunPipe>>,
    /// The memory a machine under libkrun booted with, and its virtio-mem
    /// region beyond it, zero if it has none.
    krun_boot: u32,
    krun_region: u32,
    krun_dir: Option<PathBuf>,
    closed: tokio::sync::Mutex<bool>,
}

/// What a boot has started so far, torn down again if it fails.
#[derive(Default)]
struct Parts {
    process: Option<Process>,
    api: Option<Api>,
    server: Option<agent::Server>,
    vsock: PathBuf,
    gpu: Option<GpuBackend>,
    net: Option<Passt>,
    mac_net: Option<crate::gvproxy::Network>,
    files: Option<SharedFiles>,
    krun: Option<KrunPipe>,
    krun_boot: u32,
    krun_region: u32,
    krun_dir: Option<PathBuf>,
}

impl Parts {
    /// Releases everything: the monitor goes before its backends, since
    /// tearing a vhost-user backend out from under a live monitor makes it
    /// report a broken device.
    async fn close(&mut self) {
        self.server = None;
        if let Some(p) = &self.process {
            p.stop().await;
        }
        drop(self.krun.take());
        if let Some(f) = self.files.take() {
            f.close();
        }
        if let Some(g) = self.gpu.take() {
            g.close().await;
        }
        if let Some(n) = self.net.take() {
            n.close().await;
        }
        if let Some(n) = self.mac_net.take() {
            n.close().await;
        }
        if let Some(d) = self.krun_dir.take() {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

impl Instance {
    /// Starts a machine and waits for its agent: it resumes it if dir holds
    /// a snapshot of it taken against the same base and read-only disks,
    /// and boots it from its disks otherwise. On error, everything it
    /// started has been stopped again.
    pub async fn boot(mut cfg: InstanceConfig) -> io::Result<Instance> {
        if cfg.agent_wait.is_zero() {
            cfg.agent_wait = Duration::from_secs(180);
        }
        std::fs::create_dir_all(&cfg.dir)?;
        let run = cfg.run_dir();
        std::fs::create_dir_all(&run)?;
        set_mode(&run, 0o700)?;

        let mut resuming = false;
        if let Some(rec) = suspend::suspended(&cfg.dir) {
            if rec.same_machine(&cfg.record()) {
                resuming = true;
            } else {
                // The guest would find files it holds open replaced. Its
                // memory is given up and it boots from its disks.
                cfg.log.warn(
                    "the base or a read-only disk changed while the machine was suspended; booting it afresh",
                    &[],
                );
                suspend::discard(&cfg.dir);
            }
        }
        if !resuming {
            suspend::clear_backend_state(&cfg.dir);
            crate::make_swap(&cfg.dir, cfg.swap_mib)
                .map_err(|e| err(format!("making the swap disk: {e}")))?;
        }
        if cfg.swap_mib > 0 {
            cfg.extra_cmdline += &format!(" hangar.swap=/dev/{}", cfg.next_disk(cfg.disks.len()));
            cfg.disks.push(ch::Disk {
                path: crate::swap::swap_file(&cfg.dir),
                direct: true,
                ..Default::default()
            });
        }
        let caps = host::detect(&cfg.monitor)?;

        let initrd = run.join("initrd.img");
        crate::write_initrd(&cfg.agent, &initrd)?;
        // Each start writes its console and monitor logs anew, as new files:
        // a reader following one knows it is another log by that, even once
        // it has grown past where the last ended.
        for f in [cfg.console_file.clone(), cfg.dir.join("monitor.log")] {
            match std::fs::remove_file(&f) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        let exit = Arc::new(Mutex::new(Exit::default()));
        let mut parts = Parts::default();
        let started = match caps.monitor {
            Monitor::Krun => start_krun(&cfg, &caps, &initrd, resuming, &exit, &mut parts).await,
            Monitor::CloudHypervisor => {
                start_cloud_hypervisor(&cfg, &caps, &initrd, resuming, &exit, &mut parts).await
            }
        };
        let session = match started {
            Ok(()) => await_agent(&cfg, &parts, &exit).await,
            Err(e) => Err(e),
        };
        let session = match session {
            Ok(s) => s,
            Err(e) => {
                parts.close().await;
                return Err(e);
            }
        };
        cfg.log.info(
            "agent connected",
            &[
                ("hostname", &session.hello.hostname),
                ("kernel", &session.hello.kernel),
                ("up_seconds", &(session.hello.boot_micros as f64 / 1e6)),
                ("resumed", &resuming),
            ],
        );
        // The guest's clock is wrong on arrival: a fresh boot starts from the
        // image's build time and a resume from the moment of the suspend.
        if let Err(e) = session
            .set_clock(SystemTime::now(), Duration::from_secs(5))
            .await
        {
            cfg.log
                .warn("could not set the guest clock", &[("err", &e)]);
        }
        if resuming {
            // Running again, so the snapshot is spent: resuming from it a
            // second time would roll the guest back under its own disks.
            suspend::discard(&cfg.dir);
        }
        Ok(Instance {
            _server: parts.server.take(),
            monitor: caps.monitor,
            process: parts.process.take().expect("a started monitor"),
            exit,
            api: parts.api.take(),
            session: Arc::new(session),
            resumed: resuming,
            vsock: std::mem::take(&mut parts.vsock),
            gpu: parts.gpu.take(),
            net: parts.net.take(),
            mac_net: parts.mac_net.take(),
            files: parts.files.take(),
            krun: parts.krun.take().map(tokio::sync::Mutex::new),
            krun_boot: parts.krun_boot,
            krun_region: parts.krun_region,
            krun_dir: parts.krun_dir.take(),
            closed: tokio::sync::Mutex::new(false),
            cfg,
        })
    }

    /// The agent's session with the guest.
    pub fn session(&self) -> &Arc<agent::Session> {
        &self.session
    }

    /// Whether the machine came back from a suspend rather than booting.
    pub fn resumed(&self) -> bool {
        self.resumed
    }

    pub fn monitor(&self) -> Monitor {
        self.monitor
    }

    /// The monitor's process ID.
    pub fn pid(&self) -> u32 {
        self.process.pid
    }

    /// Waits for the monitor to exit.
    pub async fn exited(&self) {
        self.process.wait().await;
    }

    pub fn has_exited(&self) -> bool {
        *self.process.exited.borrow()
    }

    /// Whether the monitor exited because the guest rebooted.
    pub fn rebooted(&self) -> bool {
        self.exit.lock().unwrap().rebooted
    }

    /// Whether the guest's kernel panicked, which it follows by rebooting.
    pub fn panicked(&self) -> bool {
        self.exit.lock().unwrap().panicked
    }

    /// Why the monitor exited, if not by being asked to.
    pub fn err(&self) -> Option<String> {
        self.exit.lock().unwrap().err.clone()
    }

    /// The processes that serve the guest beside its monitor: the GPU
    /// backend, if it has one, and the others.
    pub fn support_pids(&self) -> (Option<u32>, Vec<u32>) {
        let mut others: Vec<u32> = self.net.iter().map(Passt::pid).collect();
        others.extend(self.mac_net.iter().map(crate::gvproxy::Network::pid));
        (self.gpu.as_ref().map(GpuBackend::pid), others)
    }

    /// The machine's memory, as its monitor has it.
    pub async fn memory(&self) -> io::Result<Memory> {
        if self.monitor == Monitor::Krun {
            return self.krun_memory().await;
        }
        match &self.api {
            Some(api) => api.memory().await,
            None => Err(err("the monitor has no API socket")),
        }
    }

    /// Asks the guest to have mib MiB of memory, or as near as it can be
    /// resized to, and gives what it asked for. The guest plugs memory in
    /// and gives it back in its own time; memory says how far it has got.
    pub async fn resize_memory(&self, mib: u32) -> io::Result<u32> {
        let m = self.memory().await?;
        if !m.resizable() {
            return Err(err(
                "it booted with no room to resize its memory; it takes a new size when it next starts",
            ));
        }
        let want = m.fit(mib);
        if want == m.requested {
            return Ok(want);
        }
        if self.monitor == Monitor::Krun {
            self.ask_krun(
                &format!("memory {}", want - m.boot),
                Duration::from_secs(10),
            )
            .await?;
            return Ok(want);
        }
        self.api
            .as_ref()
            .expect("an API")
            .resize_memory(want)
            .await?;
        Ok(want)
    }

    /// Opens a connection to a port in the guest, as its monitor carries
    /// vsock.
    pub async fn dial_guest(&self, port: u32) -> io::Result<GuestConn> {
        match self.monitor {
            Monitor::Krun => guest::dial_krun(&self.vsock, port).await,
            Monitor::CloudHypervisor => guest::dial_cloud_hypervisor(&self.vsock, port).await,
        }
    }

    /// Sends hangar-krun a request and gives its answer, or the error it
    /// answers with.
    async fn ask_krun(&self, req: &str, timeout: Duration) -> io::Result<String> {
        let Some(k) = &self.krun else {
            return Err(err("the machine is not under libkrun"));
        };
        let mut k = k.lock().await;
        writeln!(k.requests, "{req}").map_err(|e| err(format!("asking hangar-krun: {e}")))?;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            tokio::select! {
                got = k.answers.recv() => {
                    let Some(got) = got else {
                        return Err(err("hangar-krun has stopped"));
                    };
                    if k.stale > 0 {
                        k.stale -= 1;
                        continue;
                    }
                    if let Some(msg) = got.strip_prefix("error: ") {
                        return Err(err(msg));
                    }
                    return Ok(got);
                }
                _ = self.process.wait() => return Err(err("the machine has stopped")),
                _ = tokio::time::sleep_until(deadline) => {
                    k.stale += 1;
                    return Err(err(format!("hangar-krun did not answer {req:?} within {timeout:?}")));
                }
            }
        }
    }

    /// The memory of a machine under libkrun: what it booted with and,
    /// with a virtio-mem region, what hangar-krun says of the region.
    async fn krun_memory(&self) -> io::Result<Memory> {
        let mut m = Memory {
            boot: self.krun_boot,
            max: self.krun_boot + self.krun_region,
            requested: self.krun_boot,
            actual: self.krun_boot,
        };
        if self.krun_region == 0 {
            return Ok(m);
        }
        let got = self.ask_krun("memory", Duration::from_secs(10)).await?;
        let mut f = got.split_whitespace().map(str::parse::<u32>);
        match (f.next(), f.next()) {
            (Some(Ok(have)), Some(Ok(requested))) => {
                m.actual += have;
                m.requested += requested;
                Ok(m)
            }
            _ => Err(err(format!("hangar-krun's memory: {got:?}"))),
        }
    }

    /// Writes the machine to its directory and stops it; boot resumes it.
    /// If it cannot, the guest carries on running as it was, and the
    /// instance with it.
    ///
    /// The guest is paused first so nothing it holds changes while it is
    /// written down; each backend then records what the guest is holding,
    /// and only then is the snapshot taken, so they all describe the same
    /// instant.
    pub async fn suspend(&self) -> io::Result<()> {
        if self.monitor == Monitor::Krun {
            return self.suspend_krun().await;
        }
        let c = &self.cfg;
        let api = self.api.as_ref().expect("an API");
        let snap = c.dir.join(SNAPSHOT_DIR);
        let partial = c.dir.join(format!("{SNAPSHOT_DIR}.new"));
        let written: io::Result<()> = async {
            api.pause()
                .await
                .map_err(|e| err(format!("pausing: {e}")))?;
            if let Some(gpu) = &self.gpu {
                gpu.save_state(Duration::from_secs(30))
                    .await
                    .map_err(|e| err(format!("saving the GPU backend's state: {e}")))?;
            }
            let _ = std::fs::remove_dir_all(&partial);
            std::fs::create_dir_all(&partial)?;
            set_mode(&partial, 0o700)?;
            api.snapshot(&partial)
                .await
                .map_err(|e| err(format!("taking the snapshot: {e}")))?;
            let _ = std::fs::remove_dir_all(&snap);
            std::fs::rename(&partial, &snap)?;
            suspend::mark(&c.dir, &c.record())
        }
        .await;
        if let Err(e) = written {
            suspend::discard(&c.dir);
            if let Err(re) = api.resume().await {
                c.log.warn(
                    "could not resume the guest after a failed suspend",
                    &[("err", &re)],
                );
            }
            return Err(e);
        }
        // With the state on disk the monitor has nothing left to do.
        if let Err(e) = api.shutdown().await {
            c.log
                .warn("stopping the monitor after suspending", &[("err", &e)]);
        }
        self.process.wait_for(Duration::from_secs(15)).await;
        self.close().await;
        Ok(())
    }

    /// Has hangar-krun write the machine to its snapshot, records it, and
    /// stops hangar-krun. If it cannot, the guest carries on running.
    async fn suspend_krun(&self) -> io::Result<()> {
        let c = &self.cfg;
        let snap = c.dir.join(SNAPSHOT_DIR);
        let partial = c.dir.join(format!("{SNAPSHOT_DIR}.new"));
        let _ = std::fs::remove_dir_all(&partial);
        if let Err(e) = self
            .ask_krun(
                &format!("suspend {}", partial.display()),
                Duration::from_secs(300),
            )
            .await
        {
            let _ = std::fs::remove_dir_all(&partial);
            return Err(err(format!("suspending: {e}")));
        }
        let recorded: io::Result<()> = (|| {
            let _ = std::fs::remove_dir_all(&snap);
            std::fs::rename(&partial, &snap)?;
            let mut rec = c.record();
            rec.krun_memory = Some(KrunMemory {
                boot: self.krun_boot,
                region: self.krun_region,
            });
            suspend::mark(&c.dir, &rec)
        })();
        let exit = || {
            if let Some(k) = &self.krun
                && let Ok(mut k) = k.try_lock()
            {
                let _ = writeln!(k.requests, "exit");
            }
        };
        if let Err(e) = recorded {
            // The machine is paused, written down and of no further use:
            // the guest boots afresh next time.
            suspend::discard(&c.dir);
            exit();
            return Err(e);
        }
        // Recorded, so stopping the machine keeps its swap.
        exit();
        self.process.wait_for(Duration::from_secs(15)).await;
        self.close().await;
        Ok(())
    }

    /// Powers the guest off so it flushes its disks, forces the matter if
    /// it does not go in time, and releases the instance.
    pub async fn shutdown(&self) {
        // poweroff does not return once it has worked: the connection goes
        // with the guest. Its answer is not waited for.
        let s = self.session.clone();
        tokio::spawn(async move {
            let _ = s
                .exec(Duration::from_secs(60), &["systemctl", "poweroff"])
                .await;
        });
        if !self.process.wait_for(Duration::from_secs(90)).await {
            self.cfg
                .log
                .warn("the guest did not power off; stopping the monitor", &[]);
            self.process.stopping.store(true, Ordering::SeqCst);
            let asked = match &self.api {
                Some(api) => tokio::time::timeout(Duration::from_secs(5), api.shutdown())
                    .await
                    .is_ok_and(|r| r.is_ok()),
                None => false,
            };
            if !asked {
                self.process.signal(libc::SIGTERM);
            }
            if !self.process.wait_for(Duration::from_secs(10)).await {
                self.process.signal(libc::SIGKILL);
            }
        }
        self.close().await;
    }

    /// Releases everything the instance holds. The monitor goes first, if
    /// it has not gone already, then its backends.
    pub async fn close(&self) {
        let mut closed = self.closed.lock().await;
        if *closed {
            return;
        }
        *closed = true;
        self.process.stop().await;
        if let Some(f) = &self.files {
            f.close();
        }
        if let Some(g) = &self.gpu {
            g.close().await;
        }
        if let Some(n) = &self.net {
            n.close().await;
        }
        if let Some(n) = &self.mac_net {
            n.close().await;
        }
        if let Some(d) = &self.krun_dir {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

fn set_mode(p: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode))
}

/// Where a Cloud Hypervisor machine's passt has its socket: passt's
/// AppArmor profile allows it nowhere else.
fn passt_dir(id: &str) -> io::Result<PathBuf> {
    let d = std::env::temp_dir().join(format!("hangar-{id}"));
    std::fs::create_dir_all(&d)?;
    set_mode(&d, 0o700)?;
    Ok(d)
}

/// Where a machine under libkrun has its vsock sockets: under /tmp, since a
/// unix socket's path is at most 103 bytes on macOS, which a worker's state
/// directory leaves too little of.
fn krun_dir(id: &str) -> io::Result<PathBuf> {
    let d = PathBuf::from(format!("/tmp/hangar-{id}"));
    std::fs::create_dir_all(&d)?;
    set_mode(&d, 0o700)?;
    Ok(d)
}

/// The monitor's last word, for explaining why it exited.
fn last_line(path: &Path) -> String {
    let Ok(b) = std::fs::read_to_string(path) else {
        return String::new();
    };
    match b.trim().lines().last().map(str::trim) {
        Some(l) if !l.is_empty() => format!(": {l}"),
        _ => String::new(),
    }
}

/// A pipe, both ends closed on exec.
fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as RawFd; 2];
    // SAFETY: fds holds the two descriptors pipe writes.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    for fd in fds {
        // SAFETY: fd is one of the pipe's, open.
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    // SAFETY: the descriptors are new, and owned here alone.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// Starts a monitor, its output into dir's monitor.log, with fd3 as its
/// descriptor 3, and watches for it to exit.
fn spawn_monitor(
    cfg: &InstanceConfig,
    mut cmd: tokio::process::Command,
    name: &'static str,
    fd3: OwnedFd,
    fd4: Option<OwnedFd>,
    exit: &Arc<Mutex<Exit>>,
) -> io::Result<Process> {
    let log_path = cfg.dir.join("monitor.log");
    let log = std::fs::File::create(&log_path)?;
    cmd.stdout(log.try_clone()?).stderr(log);
    let a = fd3.as_raw_fd();
    let b = fd4.as_ref().map_or(-1, |f| f.as_raw_fd());
    let fd4_given = fd4.is_some();
    // SAFETY: the closure calls only async-signal-safe functions.
    unsafe {
        cmd.pre_exec(move || {
            // Each is copied clear of 3 and 4 before either is placed, so
            // placing one cannot close the other. dup2's copies stay open
            // across exec; the copies between close.
            let a = libc::fcntl(a, libc::F_DUPFD_CLOEXEC, 5);
            let b = if b < 0 {
                b
            } else {
                libc::fcntl(b, libc::F_DUPFD_CLOEXEC, 5)
            };
            if a < 0
                || (fd4_given && b < 0)
                || libc::dup2(a, 3) < 0
                || (b >= 0 && libc::dup2(b, 4) < 0)
            {
                return Err(io::Error::last_os_error());
            }
            // The monitor dies with the worker on Linux; on macOS
            // hangar-krun watches its stdin, a pipe from the worker, instead.
            #[cfg(target_os = "linux")]
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| err(format!("starting {name}: {e}")))?;
    drop((fd3, fd4));
    let pid = child.id().unwrap_or(0);
    let (tx, exited) = watch::channel(false);
    let stopping = Arc::new(AtomicBool::new(false));
    {
        let (exit, stopping, dir) = (exit.clone(), stopping.clone(), cfg.dir.clone());
        tokio::spawn(async move {
            let st = child.wait().await;
            let failed = !st.as_ref().is_ok_and(|s| s.success());
            {
                let mut e = exit.lock().unwrap();
                if failed && !stopping.load(Ordering::SeqCst) && !e.rebooted {
                    let why = match &st {
                        Ok(s) => s.to_string(),
                        Err(x) => x.to_string(),
                    };
                    e.err = Some(format!("{name} exited: {why}{}", last_line(&log_path)));
                }
            }
            crate::drop_swap(&dir, suspend::suspended(&dir).is_some());
            let _ = tx.send(true);
        });
    }
    Ok(Process {
        pid,
        exited,
        stopping,
    })
}

/// Starts the machine under Cloud Hypervisor, resuming it from its snapshot
/// if resuming, and listens for its agent.
async fn start_cloud_hypervisor(
    cfg: &InstanceConfig,
    caps: &Caps,
    initrd: &Path,
    resuming: bool,
    exit: &Arc<Mutex<Exit>>,
    parts: &mut Parts,
) -> io::Result<()> {
    let run = cfg.run_dir();
    let mut ccfg = ch::Config {
        name: cfg.name.clone(),
        kernel: cfg.kernel.clone(),
        initrd: Some(initrd.to_path_buf()),
        disks: cfg.disks.clone(),
        memory_mb: cfg.memory_mib,
        memory_max_mb: cfg.memory_max_mib,
        cpus: cfg.cpus,
        console_file: cfg.console_file.clone(),
        console_tty: caps.console_tty.into(),
        extra_cmdline: cfg.extra_cmdline.clone(),
        guest_cid: GUEST_CID,
        vsock_socket: run.join("vsock.sock"),
        api_socket: Some(run.join("api.sock")),
        ..Default::default()
    };
    // The guest's initramfs mounts the base as the lower layer: the EROFS
    // image from the device named on the command line.
    match cfg.base_device {
        ImageDevice::Disk => {
            ccfg.extra_cmdline += &format!(" hangar.image=/dev/{}", cfg.next_disk(cfg.disks.len()));
            ccfg.disks.push(ch::Disk {
                path: cfg.base_file.clone(),
                read_only: true,
                ..Default::default()
            });
        }
        ImageDevice::Pmem => {
            ccfg.pmem = vec![cfg.base_file.clone()];
            ccfg.extra_cmdline += " hangar.image=/dev/pmem0";
            if cfg.base_dax {
                ccfg.extra_cmdline += " hangar.image_dax=1";
            }
        }
    }
    if cfg.net {
        let net = Passt::start(&passt_dir(&cfg.id)?.join("net.sock"), true).await?;
        ccfg.net_socket = net.socket().map(Path::to_path_buf);
        parts.net = Some(net);
    }
    if cfg.gpu {
        let gpu = GpuBackend::start(
            &run.join("gpu.sock"),
            cfg.gpu_venus,
            cfg.gpu_venus_restore,
            Some(&cfg.dir.join("gpu.json")),
            &cfg.gpu_env,
        )
        .await?;
        ccfg.gpu_socket = Some(gpu.socket().to_path_buf());
        ccfg.gpu_shm_mib = if cfg.gpu_window_mib == 0 {
            512
        } else {
            cfg.gpu_window_mib
        };
        parts.gpu = Some(gpu);
    }
    // The agent dials early in the boot, so it is listened for before the
    // monitor starts. The monitor binds the vsock path itself, and one left
    // by a monitor that was killed would stop it starting.
    let api_socket = ccfg.api_socket.clone().unwrap_or_default();
    let _ = std::fs::remove_file(&ccfg.vsock_socket);
    let _ = std::fs::remove_file(&api_socket);
    parts.vsock = ccfg.vsock_socket.clone();
    parts.server = Some(
        agent::Server::listen_hybrid(&ccfg.vsock_socket, GUEST_CID)
            .map_err(|e| err(format!("listening for the agent: {e}")))?,
    );
    if let Some((root, view)) = &cfg.shared_files {
        parts.files = Some(
            SharedFiles::serve(&ccfg.vsock_socket, GUEST_CID, root, view.clone())
                .map_err(|e| err(format!("serving the shared files: {e}")))?,
        );
    }

    // The monitor starts from its snapshot with nothing but its API socket:
    // the machine, its devices and their sockets are all in the snapshot, at
    // the paths they had, which the backends are listening on again.
    let args = if resuming {
        cfg.progress("resuming");
        vec![
            "--api-socket".into(),
            api_socket.to_string_lossy().into_owned(),
        ]
    } else {
        cfg.progress("booting");
        ccfg.args()?
    };
    launch_cloud_hypervisor(cfg, args, exit, parts)?;
    let api = Api::new(&api_socket);
    if resuming {
        api.wait_ready(Duration::from_secs(10)).await?;
        api.restore(&cfg.dir.join(SNAPSHOT_DIR))
            .await
            .map_err(|e| err(format!("restoring the snapshot: {e}")))?;
        if let Some(gpu) = &parts.gpu {
            gpu.wait_restored(Duration::from_secs(120))
                .await
                .map_err(|e| err(format!("restoring the GPU backend: {e}")))?;
        }
        api.resume().await?;
    }
    parts.api = Some(api);
    Ok(())
}

/// Starts Cloud Hypervisor with args. A guest that reboots is reset inside
/// the monitor, which loses the socket the host reaches the guest's vsock
/// on. So the monitor reports its events, and a reboot ends it: the
/// machine is booted again whole.
fn launch_cloud_hypervisor(
    cfg: &InstanceConfig,
    mut args: Vec<String>,
    exit: &Arc<Mutex<Exit>>,
    parts: &mut Parts,
) -> io::Result<()> {
    let bin = match &cfg.monitor_bin {
        Some(b) => b.clone(),
        None => ch::find()?,
    };
    let (events, events_w) = pipe()?;
    args.extend(["--event-monitor".into(), "fd=3".into()]);
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(&args).stdin(Stdio::null());
    let process = spawn_monitor(cfg, cmd, "cloud-hypervisor", events_w, None, exit)?;
    let (exit, pid) = (exit.clone(), process.pid);
    // The monitor's events until it exits: a panic is noted, and a reboot
    // stops it. The guest has flushed its disks by then, and what the
    // monitor wrote of them is in the host's page cache, so killing it loses
    // nothing.
    std::thread::spawn(move || {
        #[derive(serde::Deserialize)]
        struct Event {
            #[serde(default)]
            source: String,
            #[serde(default)]
            event: String,
        }
        let r = std::io::BufReader::new(std::fs::File::from(events));
        for ev in serde_json::Deserializer::from_reader(r).into_iter::<Event>() {
            let Ok(ev) = ev else { return };
            if ev.source == "guest" && ev.event == "panic" {
                exit.lock().unwrap().panicked = true;
            }
            if ev.source == "vm" && ev.event == "rebooting" {
                exit.lock().unwrap().rebooted = true;
                // SAFETY: kill has no memory preconditions; the pipe is open
                // while the process is, so it is not yet reaped.
                unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
            }
        }
    });
    parts.process = Some(process);
    Ok(())
}

/// Starts the machine under hangar-krun -- libkrun -- resuming it from its
/// snapshot if resuming, and listens for its agent. A machine is resumed
/// configured as it was booted: libkrun loads the snapshot into it, with
/// new sockets and backends behind its devices.
async fn start_krun(
    cfg: &InstanceConfig,
    caps: &Caps,
    initrd: &Path,
    resuming: bool,
    exit: &Arc<Mutex<Exit>>,
    parts: &mut Parts,
) -> io::Result<()> {
    let dir = krun_dir(&cfg.id)?;
    parts.krun_dir = Some(dir.clone());
    let vsock = dir.join("v");
    parts.vsock = vsock.clone();

    // A resumed machine is laid out as it was when suspended; one suspended
    // with no layout recorded had all its memory from boot.
    let (mut boot, mut region, mut plugged) = (cfg.memory_mib, 0, 0);
    match suspend::suspended(&cfg.dir).and_then(|r| r.krun_memory) {
        Some(m) if resuming => (boot, region) = (m.boot, m.region),
        _ if !resuming => (boot, region, plugged) = ch::hotplug(cfg.memory_mib, cfg.memory_max_mib),
        _ => {}
    }
    (parts.krun_boot, parts.krun_region) = (boot, region);
    let s = |p: &Path| p.to_string_lossy().into_owned();
    // The monitor's end of passt's socket, which it inherits as NET_FD.
    let mut net_fd = None;
    let mut kc = krun::Config {
        cpus: cfg.cpus,
        memory_mib: boot,
        hotplug_mib: region,
        plugged_mib: plugged,
        kernel: s(&cfg.kernel),
        initrd: s(initrd),
        console: s(&cfg.console_file),
        host_ports: BTreeMap::from([(agent::PORT, s(&hybrid_path(&vsock, agent::PORT)))]),
        ..Default::default()
    };
    for (n, d) in cfg.disks.iter().enumerate() {
        kc.disks.push(krun::Disk {
            id: cfg.next_disk(n),
            path: s(&d.path),
            read_only: d.read_only,
            direct: d.direct,
        });
    }
    let mut extra = cfg.extra_cmdline.clone();
    match cfg.base_device {
        ImageDevice::Disk => {
            let dev = cfg.next_disk(cfg.disks.len());
            extra += &format!(" hangar.image=/dev/{dev}");
            kc.disks.push(krun::Disk {
                id: dev,
                path: s(&cfg.base_file),
                read_only: true,
                ..Default::default()
            });
        }
        ImageDevice::Pmem => {
            return Err(err(
                "libkrun attaches an image as a disk only (vm.image_device disk), not pmem",
            ));
        }
    }
    kc.cmdline = ch::Config {
        console_tty: caps.console_tty.into(),
        extra_cmdline: extra,
        ..Default::default()
    }
    .cmdline();
    for p in GUEST_PORTS {
        kc.guest_ports.insert(p, s(&guest::krun_socket(&vsock, p)));
    }
    if cfg.net && caps.os != "linux" {
        let sock = dir.join("net");
        let bin = crate::gvproxy::find(cfg.gvproxy.as_deref())?;
        let net = crate::gvproxy::Network::start(&bin, &sock, &cfg.dir.join("net.log"))
            .await
            .map_err(|e| err(format!("starting the network: {e}")))?;
        parts.mac_net = Some(net);
        kc.net = Some(krun::Net {
            socket: s(&sock),
            mac: GUEST_MAC.into(),
            ..Default::default()
        });
    } else if cfg.net {
        // passt, as under Cloud Hypervisor, speaking its stream protocol on
        // a socket hangar-krun inherits.
        let (net, sock) = Passt::start_connected()
            .await
            .map_err(|e| err(format!("starting the network: {e}")))?;
        parts.net = Some(net);
        kc.net = Some(krun::Net {
            mac: GUEST_MAC.into(),
            stream: true,
            fd: Some(NET_FD),
            ..Default::default()
        });
        net_fd = Some(sock);
    }
    if cfg.gpu {
        kc.gpu = Some(krun::Gpu {
            window_mib: if cfg.gpu_window_mib == 0 {
                512
            } else {
                cfg.gpu_window_mib
            },
            virgl: caps.os == "linux",
        });
        if caps.os != "linux" {
            // The GPU is Vulkan alone (Venus), with no OpenGL of its own
            // (virgl): left to choose, Mesa would take virgl for it, and the
            // desktop would draw nothing. So Mesa is told what draws OpenGL,
            // for every process: zink, its OpenGL on Vulkan, where the Mac's
            // driver lets it share images with the compositor, and the CPU
            // where not.
            kc.cmdline += if cfg.gpu_zink {
                " systemd.setenv=MESA_LOADER_DRIVER_OVERRIDE=zink"
            } else {
                " systemd.setenv=LIBGL_ALWAYS_SOFTWARE=1"
            };
        }
    }
    // The agent dials early in the boot, so it is listened for before the
    // monitor starts.
    parts.server = Some(
        agent::Server::listen_hybrid(&vsock, GUEST_CID)
            .map_err(|e| err(format!("listening for the agent: {e}")))?,
    );
    if let Some((root, view)) = &cfg.shared_files {
        kc.host_ports.insert(
            SHARED_FILES_PORT,
            s(&hybrid_path(&vsock, SHARED_FILES_PORT)),
        );
        parts.files = Some(
            SharedFiles::serve(&vsock, GUEST_CID, root, view.clone())
                .map_err(|e| err(format!("serving the shared files: {e}")))?,
        );
    }
    if resuming {
        kc.restore = s(&cfg.dir.join(SNAPSHOT_DIR));
    }
    let conf = cfg.run_dir().join("krun.json");
    std::fs::write(
        &conf,
        serde_json::to_vec_pretty(&kc).map_err(io::Error::other)?,
    )?;
    set_mode(&conf, 0o600)?;
    cfg.progress(if resuming { "resuming" } else { "booting" });

    // hangar-krun stops the machine when its stdin closes: when the worker
    // exits, or dies. The worker's requests go down it, and hangar-krun
    // answers on descriptor 3.
    let bin = match &cfg.monitor_bin {
        Some(b) => b.clone(),
        None => krun::find()?,
    };
    let (watch_r, requests) = pipe()?;
    let (replies, reply_w) = pipe()?;
    let mut cmd = tokio::process::Command::new(bin);
    cmd.arg(&conf)
        .stdin(Stdio::from(watch_r))
        // The GPU renders as the worker's configuration has it.
        .envs(cfg.gpu_env.iter().map(|(k, v)| (k, v)));
    if cfg.gpu_venus_restore {
        // virglrenderer's render server records what rebuilding a Vulkan
        // program takes, which carries it across a suspend.
        cmd.env("VKR_RECORD", "1");
    }
    let process = spawn_monitor(cfg, cmd, "hangar-krun", reply_w, net_fd, exit)?;
    let (tx, answers) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let mut r = std::io::BufReader::new(std::fs::File::from(replies));
        let mut line = String::new();
        loop {
            line.clear();
            match std::io::BufRead::read_line(&mut r, &mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) => {
                    if tx.send(line.trim().to_string()).is_err() {
                        return;
                    }
                }
            }
        }
    });
    parts.krun = Some(KrunPipe {
        requests: std::fs::File::from(requests),
        answers,
        stale: 0,
    });
    parts.process = Some(process);
    Ok(())
}

/// Waits for the guest's agent to dial back, and gives up early if the
/// monitor exits.
async fn await_agent(
    cfg: &InstanceConfig,
    parts: &Parts,
    exit: &Arc<Mutex<Exit>>,
) -> io::Result<agent::Session> {
    let server = parts.server.as_ref().expect("an agent listener");
    let process = parts.process.as_ref().expect("a started monitor");
    let mut cancel = cfg.cancel.clone();
    let cancelled = async {
        match cancel.as_mut() {
            Some(c) => {
                let _ = c.wait_for(|c| *c).await;
            }
            None => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        _ = cancelled => Err(err("the boot was called off")),
        s = server.accept(Some(cfg.agent_wait)) => s.map_err(|e| {
            err(format!("the agent did not connect within {:?}: {e}", cfg.agent_wait))
        }),
        _ = process.wait() => Err(match exit.lock().unwrap().err.clone() {
            Some(e) => err(e),
            None => err("the machine stopped before its agent connected"),
        }),
    }
}
