//! The agent as a worker VM's whole system, on Proxmox: the VM boots the
//! release ISO every environment boots, and the agent, as init, finds a data
//! disk (serial `hangar-data`) where an environment would have its writable
//! disk, and runs the worker instead of an image. There is no distribution
//! underneath: the release ISO holds the kernel, the agent (this initramfs,
//! with mke2fs) and the worker, built static, with the certificates it
//! trusts.
//!
//! As PID 1 it gives the data disk a filesystem the first time and mounts it
//! at /data, copies the worker from the release ISO (LUN 1) onto it and lets
//! the ISO go, puts the guest's firewall up with the hub's port open, gets
//! an address by DHCP (`crate::dhcp`), and answers the guest agent channel
//! (`crate::qga`), through which whoever made the VM writes its settings
//! (SETTINGS, JSON), kept on the data disk for every later boot, since
//! nothing of them is in the VM's configuration. With settings and an
//! address it writes the worker's configuration and runs the worker, again
//! whenever it exits or either changes, and reaps whatever else exits. A
//! shutdown through the agent channel stops the worker, unmounts the data
//! disk and powers off.
//!
//! Nothing runs from the ISO, so the worker can change the VM's to another
//! release's as it runs, as it does to move to the release its server names,
//! then reboot into it. The worker of the release booted before is kept, and
//! run should the new one keep failing.

/// Where the worker VM's maker writes its settings through the guest agent
/// channel: root's alone.
pub const SETTINGS: &str = "/dev/hangar/worker.json";

#[cfg(target_os = "linux")]
pub use role::{is_worker, power_off, run};

#[cfg(target_os = "linux")]
mod role {
    use std::io;
    use std::net::Ipv4Addr;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::Path;
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::time::{Duration, Instant};

    use serde::{Deserialize, Serialize};
    use serde_json::json;
    use tokio::sync::watch;

    use super::SETTINGS;
    use crate::dhcp::Lease;
    use crate::init::boot;

    /// The kernel command line's word for a VM whose settings come through
    /// the guest agent channel: one booted from the release ISO.
    const CHANNEL_ARG: &str = "hangar.hub=channel";
    const DATA_SERIAL: &str = "hangar-data";
    const UPPER_SERIAL: &str = "hangar-upper";
    const DATA: &str = "/data";
    /// Where the release ISO is mounted while its worker is copied off it.
    const RELEASE: &str = "/release";
    const RELEASE_DEVICE: &str = "scsi:1";
    /// The worker of the release booted, and of the one before it, as the
    /// ISO has them: the worker, the certificates it trusts, its version.
    const CURRENT: &str = "/data/release";
    const PREVIOUS: &str = "/data/release-previous";
    const RELEASE_FILES: [&str; 3] = ["hangar-worker", "ca-certificates.crt", "VERSION"];
    /// How many times a new release's worker may fail, within how long of
    /// the boot, before the one before it is run.
    const FAILURES: usize = 3;
    const FAILING_WITHIN: Duration = Duration::from_secs(600);
    /// The settings, kept.
    const KEPT: &str = "/data/worker.json";
    const CONFIG: &str = "/data/worker.yaml";
    const HUB_PORT: u16 = 7443;
    const HOSTNAME: &str = "hangar-worker";
    /// How long the disks that tell a worker VM from an environment are
    /// waited for.
    const DISKS_WITHIN: Duration = Duration::from_secs(10);
    /// How long the worker has to stop before it is killed.
    const STOP_WITHIN: Duration = Duration::from_secs(60);

    /// The worker that runs, by PID, or 0.
    static WORKER: AtomicI32 = AtomicI32::new(0);

    /// What the worker VM's maker gives it.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Settings {
        /// The Hangar server, as the worker reaches it.
        server: String,
        /// The token the worker enrols with, the first time.
        bootstrap_token: String,
        /// The worker's name in Hangar, which templates place by.
        node: String,
        proxmox: Proxmox,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Proxmox {
        url: String,
        token: String,
        #[serde(default)]
        fingerprint: String,
        node: String,
        storage: String,
        #[serde(default)]
        iso_storage: String,
        bridge: String,
        #[serde(default)]
        pool: String,
        prefix: String,
        /// A release ISO environments boot, by its volume's ID; empty, the
        /// one the worker VM booted.
        #[serde(default)]
        release_iso: String,
    }

    /// Whether this VM is a worker VM: booted from the release ISO, with a
    /// data disk where an environment has its writable disk.
    pub fn is_worker(args: &[String]) -> bool {
        if !args.iter().any(|a| a == CHANNEL_ARG) {
            return false;
        }
        let deadline = Instant::now() + DISKS_WITHIN;
        loop {
            for e in std::fs::read_dir("/sys/block")
                .into_iter()
                .flatten()
                .flatten()
            {
                match std::fs::read_to_string(e.path().join("serial")).as_deref() {
                    Ok(s) if s.trim() == DATA_SERIAL => return true,
                    Ok(s) if s.trim() == UPPER_SERIAL => return false,
                    _ => {}
                }
            }
            if Instant::now() > deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Runs the worker VM. It returns only if it cannot, having said why on
    /// the console, and then never: PID 1 exiting panics the kernel.
    pub fn run() -> ! {
        match prepare() {
            Ok(fresh) => match tokio::runtime::Runtime::new() {
                Ok(rt) => rt.block_on(serve(fresh)),
                Err(e) => eprintln!("INITRAMFS: worker VM: {e}"),
            },
            Err(e) => eprintln!("INITRAMFS: worker VM: {e}"),
        }
        eprintln!("INITRAMFS: cannot run the worker; see above");
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }

    /// Readies the machine to run the worker; whether the release booted
    /// is another than the boot before's.
    fn prepare() -> io::Result<bool> {
        // SAFETY: the name is valid for its length.
        unsafe { libc::sethostname(HOSTNAME.as_ptr().cast(), HOSTNAME.len()) };
        let data = boot::device(&format!("serial:{DATA_SERIAL}"))
            .map_err(boot::context("the data disk"))?;
        boot::format_if_new(&data, DATA_SERIAL)
            .map_err(boot::context(&format!("making a filesystem on {data}")))?;
        boot::mkdir(DATA, 0o755)?;
        boot::mount(&data, DATA, "ext4", 0, "discard")
            .map_err(boot::context(&format!("mounting the data disk ({data})")))?;
        let release = boot::device(RELEASE_DEVICE).map_err(boot::context("the release ISO"))?;
        boot::mkdir(RELEASE, 0o755)?;
        boot::mount(&release, RELEASE, "iso9660", libc::MS_RDONLY, "").map_err(boot::context(
            &format!("mounting the release ISO ({release})"),
        ))?;
        let fresh = take_release();
        // SAFETY: umount2(2) of a path of ours.
        unsafe {
            let r = std::ffi::CString::new(RELEASE).unwrap_or_default();
            libc::umount2(r.as_ptr(), libc::MNT_DETACH);
        }
        let fresh = fresh.map_err(boot::context("copying the worker off the release ISO"))?;
        for (d, mode) in [("/etc", 0o755), ("/tmp", 0o1777), ("/run", 0o755)] {
            boot::mkdir(d, mode)?;
        }
        // There is no distribution's: localhost resolves by this alone.
        std::fs::write(
            "/etc/hosts",
            format!("127.0.0.1 localhost\n::1 localhost\n127.0.1.1 {HOSTNAME}\n"),
        )?;
        crate::firewall::open_tcp(HUB_PORT);
        crate::firewall::install().map_err(boot::context("putting the firewall in place"))?;
        eprintln!(
            "INITRAMFS: worker VM: data disk {data}, release {} from {release}{}",
            version(CURRENT),
            if fresh { " (new)" } else { "" }
        );
        Ok(fresh)
    }

    /// Copies the release's worker off its ISO, mounted at RELEASE, to
    /// CURRENT, unless it is there already, moving what was there to
    /// PREVIOUS; whether it was not there.
    fn take_release() -> io::Result<bool> {
        let iso = Path::new(RELEASE);
        let same = RELEASE_FILES.iter().all(|f| {
            match (
                std::fs::read(iso.join(f)),
                std::fs::read(Path::new(CURRENT).join(f)),
            ) {
                (Ok(a), Ok(b)) => a == b,
                _ => false,
            }
        });
        if same {
            return Ok(false);
        }
        let new = format!("{CURRENT}.new");
        let _ = std::fs::remove_dir_all(&new);
        std::fs::create_dir(&new)?;
        for f in RELEASE_FILES {
            std::fs::copy(iso.join(f), Path::new(&new).join(f))?;
        }
        if Path::new(CURRENT).exists() {
            let _ = std::fs::remove_dir_all(PREVIOUS);
            std::fs::rename(CURRENT, PREVIOUS)?;
        }
        std::fs::rename(&new, CURRENT)?;
        // SAFETY: sync(2).
        unsafe { libc::sync() };
        Ok(true)
    }

    /// The version of the release whose worker is in dir.
    fn version(dir: &str) -> String {
        std::fs::read_to_string(Path::new(dir).join("VERSION"))
            .map(|v| v.trim().to_string())
            .unwrap_or_else(|_| "unknown".into())
    }

    async fn serve(fresh: bool) {
        std::thread::spawn(reap);
        tokio::spawn(crate::qga::serve());
        tokio::spawn(crate::firewall::keep());
        let (tx, rx) = watch::channel(None);
        tokio::spawn(crate::dhcp::run(HOSTNAME.to_string(), tx));
        supervise(rx, fresh).await
    }

    /// Runs the worker with the settings and the address it has, again
    /// whenever it exits or either changes. A new release's worker that
    /// fails FAILURES times within FAILING_WITHIN of the boot gives way to
    /// the release's before it, for the rest of the boot.
    async fn supervise(mut lease: watch::Receiver<Option<Lease>>, fresh: bool) {
        let (exited_tx, mut exited) = watch::channel((0u64, 0i32));
        EXITED.get_or_init(|| exited_tx);
        let booted = Instant::now();
        let mut failures = 0;
        let mut dir = CURRENT;
        let mut said = false;
        loop {
            let Some(settings) = read_settings() else {
                if !said {
                    said = true;
                    eprintln!("hangar-agent: worker VM: waiting for its settings ({SETTINGS})");
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            };
            let address = match lease.wait_for(Option::is_some).await {
                Ok(l) => l
                    .as_ref()
                    .map(|l| l.address)
                    .unwrap_or(Ipv4Addr::UNSPECIFIED),
                Err(_) => return std::future::pending().await,
            };
            if let Err(e) = configure(&settings, address) {
                eprintln!("hangar-agent: worker VM: writing the worker's configuration: {e}");
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
            exited.borrow_and_update();
            let pid = match start(dir) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("hangar-agent: worker VM: starting the worker: {e}");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };
            eprintln!(
                "hangar-agent: worker VM: the worker of release {} runs (pid {pid}) at {address}",
                version(dir)
            );
            let restart = loop {
                tokio::select! {
                    _ = exited.changed() => break false,
                    _ = lease.changed() => {
                        let now = lease.borrow().as_ref().map(|l| l.address);
                        if now != Some(address) {
                            break true;
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_secs(2)) => {
                        if read_settings().as_ref() != Some(&settings) {
                            break true;
                        }
                    }
                }
            };
            if restart {
                eprintln!(
                    "hangar-agent: worker VM: its settings or address changed; restarting the worker"
                );
                stop(pid).await;
            } else {
                let status = exited.borrow().1;
                if status != 0 && fresh && dir == CURRENT && booted.elapsed() < FAILING_WITHIN {
                    failures += 1;
                    if failures >= FAILURES && Path::new(PREVIOUS).join("hangar-worker").exists() {
                        eprintln!(
                            "hangar-agent: worker VM: the worker of release {} failed {failures} times; running release {}'s",
                            version(CURRENT),
                            version(PREVIOUS)
                        );
                        dir = PREVIOUS;
                    }
                }
                eprintln!("hangar-agent: worker VM: the worker exited; starting it again");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }

    /// The settings: those written through the agent channel, once whole,
    /// kept on the data disk; else those kept.
    fn read_settings() -> Option<Settings> {
        if let Some(s) = std::fs::read(SETTINGS)
            .ok()
            .and_then(|b| serde_json::from_slice::<Settings>(&b).ok())
        {
            let kept = std::fs::read(KEPT)
                .ok()
                .and_then(|b| serde_json::from_slice::<Settings>(&b).ok());
            if kept.as_ref() != Some(&s)
                && let Err(e) = write_private(KEPT, &serde_json::to_vec(&s).unwrap_or_default())
            {
                eprintln!("hangar-agent: worker VM: keeping its settings: {e}");
            }
            return Some(s);
        }
        std::fs::read(KEPT)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
    }

    /// Writes the worker's configuration and the tokens it reads, all on
    /// the data disk. JSON is YAML, which the worker reads.
    fn configure(s: &Settings, address: Ipv4Addr) -> io::Result<()> {
        let p = &s.proxmox;
        write_private("/data/bootstrap-token", s.bootstrap_token.as_bytes())?;
        write_private("/data/proxmox-token", p.token.as_bytes())?;
        let config = json!({
            "server": {
                "url": s.server,
                "token_file": "/data/bootstrap-token",
                "credential_file": "/data/worker-credential",
            },
            "node": {
                "name": s.node,
                "labels": {"runtime": "proxmox", "host": s.node},
            },
            "storage": {
                "images": "/data/images",
                "environments": "/data/env",
                "caches": "/data/caches",
                "files": "/data/files",
            },
            "runtime": "proxmox",
            // Environments boot the release ISO this VM booted (worker_vm),
            // whose agent is this initramfs's init.
            "vm": {"agent": "/init"},
            "hub": {
                "listen": format!("0.0.0.0:{HUB_PORT}"),
                "address": format!("{address}:{HUB_PORT}"),
            },
            "proxmox": {
                "url": p.url,
                "token_file": "/data/proxmox-token",
                "fingerprint": p.fingerprint,
                "node": p.node,
                "storage": p.storage,
                "iso_storage": p.iso_storage,
                "bridge": p.bridge,
                "pool": p.pool,
                "prefix": p.prefix,
                "mke2fs": "/mke2fs",
                "release_iso": p.release_iso,
                "worker_vm": true,
            },
        });
        write_private(
            CONFIG,
            &serde_json::to_vec_pretty(&config).map_err(io::Error::other)?,
        )
    }

    fn write_private(path: &str, b: &[u8]) -> io::Result<()> {
        let tmp = format!("{path}.new");
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        io::Write::write_all(&mut f, b)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    }

    /// Starts the worker of the release in dir, its output on the console.
    fn start(dir: &str) -> io::Result<i32> {
        let dir = Path::new(dir);
        let child = std::process::Command::new(dir.join("hangar-worker"))
            .args(["-config", CONFIG])
            .env_clear()
            .env("PATH", dir)
            .env("HOME", DATA)
            .env("SSL_CERT_FILE", dir.join("ca-certificates.crt"))
            .current_dir(DATA)
            .spawn()?;
        let pid = child.id() as i32;
        WORKER.store(pid, Ordering::SeqCst);
        // Reaped by reap, which sees every child exit.
        std::mem::forget(child);
        Ok(pid)
    }

    /// Asks the worker to stop, and kills it if it has not within
    /// STOP_WITHIN.
    async fn stop(pid: i32) {
        // SAFETY: kill(2) of a process of ours.
        unsafe { libc::kill(pid, libc::SIGTERM) };
        let deadline = tokio::time::Instant::now() + STOP_WITHIN;
        while WORKER.load(Ordering::SeqCst) == pid {
            if tokio::time::Instant::now() > deadline {
                // SAFETY: as above.
                unsafe { libc::kill(pid, libc::SIGKILL) };
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// Counts the worker's exits, with the last's wait status.
    static EXITED: std::sync::OnceLock<watch::Sender<(u64, i32)>> = std::sync::OnceLock::new();

    /// Reaps every child that exits, as PID 1 must, and says when the
    /// worker has.
    fn reap() {
        loop {
            let mut status = 0;
            // SAFETY: waitpid(2) with a status to fill.
            let pid = unsafe { libc::waitpid(-1, &mut status, 0) };
            if pid < 0 {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            if WORKER
                .compare_exchange(pid, 0, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                eprintln!("hangar-agent: worker VM: the worker (pid {pid}) exited ({status:#x})");
                if let Some(tx) = EXITED.get() {
                    tx.send_modify(|(n, st)| {
                        *n += 1;
                        *st = status;
                    });
                }
            }
        }
    }

    /// Stops the worker, unmounts the data disk and powers the VM off, or
    /// restarts it, as the guest agent channel asks (verb: poweroff, halt,
    /// reboot).
    pub fn power_off(verb: &str) {
        let cmd = if verb == "reboot" {
            libc::LINUX_REBOOT_CMD_RESTART
        } else {
            libc::LINUX_REBOOT_CMD_POWER_OFF
        };
        std::thread::spawn(move || {
            let pid = WORKER.load(Ordering::SeqCst);
            if pid > 0 {
                // SAFETY: kill(2) of a process of ours.
                unsafe { libc::kill(pid, libc::SIGTERM) };
                let deadline = Instant::now() + STOP_WITHIN;
                while WORKER.load(Ordering::SeqCst) == pid && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
            // SAFETY: sync(2), umount2(2) and reboot(2) of this machine,
            // which is the point.
            unsafe {
                libc::sync();
                let data = std::ffi::CString::new(DATA).unwrap_or_default();
                libc::umount2(data.as_ptr(), libc::MNT_DETACH);
                libc::sync();
                libc::reboot(cmd);
            }
        });
    }
}
