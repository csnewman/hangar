//! Cloud Hypervisor's command line for one environment's machine.
//!
//! Direct kernel boot, virtio-blk for the read-only image and for writable
//! storage, and guest memory as a shared memfd on transparent huge pages.
//!
//! Guest memory is always shared: the GPU's backend is vhost-user, which
//! reads the guest's memory itself, and what a guest costs the host is read
//! from the memfd's allocated blocks. Shared memory takes huge pages only
//! when the host allows it, which is what hugepages::check verifies.

use crate::err;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long a guest's page goes untouched before DAMON reclaims it, handing
/// it back to the host. Read again, a page comes from the host's own page
/// cache: the guest's disks are files there.
const RECLAIM_MIN_AGE: Duration = Duration::from_secs(5 * 60);

/// Bounds a disk's queues. Each is a thread in the monitor with an io_uring
/// and its own descriptors, half a dozen of them: past a few queues the
/// guest gains little, and every disk of a large machine having one per
/// vCPU costs hundreds of threads and thousands of descriptors.
const MAX_DISK_QUEUES: u32 = 8;

/// The depth of each of the GPU's two virtqueues, which has to match what
/// the backend offers.
pub const QUEUE_SIZE: u32 = 256;

/// A block device attached to the guest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Disk {
    pub path: PathBuf,
    pub read_only: bool,
    /// Bypasses the host's page cache, for a disk the guest writes only
    /// what it has taken out of its memory: its swap. Cached on the host,
    /// what it swapped out would be in memory still.
    pub direct: bool,
}

/// One environment's machine.
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub name: String,
    /// The guest kernel.
    pub kernel: PathBuf,
    pub initrd: Option<PathBuf>,
    /// The first becomes /dev/vda.
    pub disks: Vec<Disk>,
    pub memory_mb: u32,
    pub cpus: u32,
    /// Above memory_mb, lets the guest's memory be resized while it runs,
    /// from boot_memory_mb(memory_mb) up to memory_max_mb. Zero gives it
    /// memory_mb for as long as it runs.
    pub memory_max_mb: u32,
    /// Receives the guest's serial console.
    pub console_file: PathBuf,
    /// The serial device as the guest sees it, for the kernel command line.
    pub console_tty: String,
    /// Appended to the generated kernel command line.
    pub extra_cmdline: String,
    /// The guest's vsock context ID; zero leaves the agent channel out.
    /// Cloud Hypervisor implements vsock in userspace rather than through
    /// the host kernel, so vsock_socket is where the host end lives.
    pub guest_cid: u32,
    pub vsock_socket: PathBuf,
    /// Files given to the guest as virtio-pmem, read-only: the guest's
    /// writes are discarded, and the host's page cache pages are mapped
    /// copy-on-write, so every guest on a file shares them.
    pub pmem: Vec<PathBuf>,
    /// The guest's network's vhost-user socket, which the monitor listens
    /// on and passt is handed a connection to; none leaves the guest with
    /// no network.
    pub net_socket: Option<PathBuf>,
    /// A hangar-gpu vhost-user socket: the guest gets a virtio-gpu device
    /// rendered by that process.
    pub gpu_socket: Option<PathBuf>,
    /// Sizes the window the guest maps blob resources into.
    pub gpu_shm_mib: u32,
    /// Where the running monitor is driven from: suspended, and its memory
    /// resized.
    pub api_socket: Option<PathBuf>,
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

impl Config {
    fn defaults(&mut self) {
        if self.memory_mb == 0 {
            self.memory_mb = 4096;
        }
        if self.cpus == 0 {
            self.cpus = 2;
        }
        if self.name.is_empty() {
            self.name = "hangar-env".into();
        }
        if self.console_tty.is_empty() {
            self.console_tty = "ttyAMA0".into();
        }
    }

    /// The monitor's arguments.
    pub fn args(&mut self) -> io::Result<Vec<String>> {
        self.defaults();
        if self.kernel.as_os_str().is_empty() {
            return Err(err("kernel path is required"));
        }
        if self.console_file.as_os_str().is_empty() {
            return Err(err("a console file is required"));
        }
        if self.disks.is_empty() {
            return Err(err("at least one disk is required"));
        }
        if self.guest_cid != 0 && self.vsock_socket.as_os_str().is_empty() {
            return Err(err("a guest CID needs a vsock socket path"));
        }
        let mut args: Vec<String> = vec![
            "--kernel".into(),
            s(&self.kernel),
            "--cpus".into(),
            format!("boot={}", self.cpus),
            // shared=on is not optional: vhost-user backends read and write
            // the guest's memory directly. Huge pages come from the host's
            // shmem policy rather than from a flag here -- Cloud Hypervisor
            // asks for them with madvise.
            "--memory".into(),
            self.memory(),
            // free-page-reporting is the whole point of the balloon: the
            // guest hands back pages as it frees them and the host takes
            // them without anyone deciding a target size.
            "--balloon".into(),
            "size=0,free_page_reporting=on".into(),
            // The guest says when its kernel panics, before it reboots, so
            // the host can tell a panic from a reboot.
            "--pvpanic".into(),
        ];
        if let Some(initrd) = &self.initrd {
            args.extend(["--initramfs".into(), s(initrd)]);
        }
        // The guest console is a serial port, so the emulated console
        // device is turned off rather than left to compete for the same
        // output.
        args.extend([
            "--console".into(),
            "off".into(),
            "--serial".into(),
            format!("file={}", s(&self.console_file)),
        ]);
        args.push("--disk".into());
        for d in &self.disks {
            // image_type=raw is stated rather than detected: these are raw
            // images, and letting the monitor guess invites it to read a
            // guest-writable file as a QCOW header. A queue per vCPU, so
            // each submits its own I/O without a lock shared with the
            // others, up to MAX_DISK_QUEUES.
            let mut spec = format!(
                "path={},image_type=raw,num_queues={}",
                s(&d.path),
                self.cpus.min(MAX_DISK_QUEUES)
            );
            if d.read_only {
                spec += ",readonly=on";
            }
            if d.direct {
                spec += ",direct=on";
            }
            args.push(spec);
        }
        for f in &self.pmem {
            args.extend(["--pmem".into(), format!("file={},discard_writes=on", s(f))]);
        }
        if self.guest_cid != 0 {
            args.extend([
                "--vsock".into(),
                format!("cid={},socket={}", self.guest_cid, s(&self.vsock_socket)),
            ]);
        }
        if let Some(net) = &self.net_socket {
            // The monitor listens, and passt, the backend, is handed the
            // connection the worker makes (Passt::connect_vhost_user):
            // passt accepting on a path is refused by AppArmor once it has
            // sandboxed itself.
            args.extend([
                "--net".into(),
                format!("vhost_user=on,socket={},vhost_mode=server", s(net)),
            ]);
        }
        if let Some(gpu) = &self.gpu_socket {
            // The device type is named rather than numbered, and the window
            // is published by the monitor for the backend to map blobs
            // into. virtio-gpu defines 1 as its host-visible window's
            // identifier.
            let mut spec = format!(
                "socket={},device_type=gpu,queue_sizes=[{QUEUE_SIZE},{QUEUE_SIZE}]",
                s(gpu)
            );
            if self.gpu_shm_mib > 0 {
                spec += &format!(",shm_size={}M,shm_id=1", self.gpu_shm_mib);
            }
            args.extend(["--generic-vhost-user".into(), spec]);
        }
        if let Some(api) = &self.api_socket {
            args.extend(["--api-socket".into(), s(api)]);
        }
        args.extend(["--cmdline".into(), self.cmdline()]);
        Ok(args)
    }

    /// The --memory argument: memory_mb, all of it there from boot, or with
    /// memory_max_mb, a boot size and the rest of memory_mb plugged into a
    /// region that reaches memory_max_mb.
    fn memory(&self) -> String {
        let (boot, region, plugged) = hotplug(self.memory_mb, self.memory_max_mb);
        if region == 0 {
            return format!("size={boot}M,shared=on");
        }
        let mut arg =
            format!("size={boot}M,shared=on,hotplug_method=virtio-mem,hotplug_size={region}M");
        if plugged > 0 {
            arg += &format!(",hotplugged_size={plugged}M");
        }
        arg
    }

    /// The guest kernel's command line.
    pub fn cmdline(&self) -> String {
        let tty = if self.console_tty.is_empty() {
            "ttyAMA0"
        } else {
            &self.console_tty
        };
        // No root= : the initramfs mounts the base read-only, the upper
        // layer read-write, stacks overlayfs and switch_roots into the
        // result.
        let mut cl = format!("console={tty}");
        cl += " systemd.show_status=1";
        // The machine is the boundary between tenants, and the host
        // protects itself; inside a guest, one owner's processes need not
        // pay to be kept from each other.
        cl += " mitigations=off";
        // Memory plugged in is onlined movable while the guest has three
        // times as much movable as it booted with, the kernel's own
        // allocations keeping the rest, and in the kernel's zone beyond: a
        // guest grown far past its boot size still has room for page tables
        // and caches.
        cl += " memory_hotplug.online_policy=auto-movable";
        // Memory the guest frees goes back to the host by the balloon's
        // free page reporting, but its file cache is not free and would be
        // kept for as long as it runs. DAMON can reclaim pages untouched for
        // RECLAIM_MIN_AGE, whatever memory is free, so the cache of what was
        // read once goes back too; it is set up here and left off, and the
        // agent turns it on while the environment asks for reclaim (the
        // spec's memory_reclaim), widening what it watches to the memory
        // virtio-mem plugs in. A program's memory is the agent's to swap
        // out: DAMON samples memory too coarsely to find it
        // (crates/agent/src/memory.rs).
        cl += &format!(
            " damon_reclaim.min_age={} damon_reclaim.skip_anon=Y \
             damon_reclaim.wmarks_high=1000 damon_reclaim.wmarks_mid=999 damon_reclaim.wmarks_low=0",
            RECLAIM_MIN_AGE.as_micros()
        );
        if !self.extra_cmdline.is_empty() {
            cl += " ";
            cl += &self.extra_cmdline;
        }
        cl
    }
}

/// What Cloud Hypervisor's virtio-mem region is sized in, and what is
/// plugged and unplugged in.
const VIRTIO_MEM_ALIGN_MB: u32 = 128;
const VIRTIO_MEM_BLOCK_MB: u32 = 2;

/// How much of a resizable machine of mb MiB is there from boot: a quarter,
/// in whole 128 MiB, and at least 1 GiB. It can never be resized below it.
/// The rest is virtio-mem, which the guest onlines as movable memory while
/// it is at most three times what it booted with, so that it can be
/// unplugged again; the kernel's own allocations, which cannot move, are
/// left the quarter.
pub fn boot_memory_mb(mb: u32) -> u32 {
    (mb / 4 / VIRTIO_MEM_ALIGN_MB * VIRTIO_MEM_ALIGN_MB)
        .max(1024)
        .min(mb)
}

/// The memory of a machine of mb MiB that can be resized up to max_mb
/// while it runs: boot MiB there from boot, a virtio-mem region of region
/// MiB beyond it reaching max_mb, and in_region MiB of the region plugged
/// in, which make up the rest of mb. A machine that cannot be resized has
/// all of mb from boot and no region.
pub fn hotplug(mb: u32, max_mb: u32) -> (u32, u32, u32) {
    if max_mb <= mb {
        return (mb, 0, 0);
    }
    let boot = boot_memory_mb(mb);
    let region = (max_mb - boot).div_ceil(VIRTIO_MEM_ALIGN_MB) * VIRTIO_MEM_ALIGN_MB;
    (boot, region, plugged(mb as i64 - boot as i64, region))
}

/// mb in whole virtio-mem blocks, rounded up, and at most region.
pub fn plugged(mb: i64, region: u32) -> u32 {
    let b = VIRTIO_MEM_BLOCK_MB as i64;
    let mb = (mb + b - 1).div_euclid(b) * b;
    mb.clamp(0, region as i64) as u32
}

/// Where Cloud Hypervisor lives: on PATH, a local build during
/// development, or where the published image installs it.
pub fn find() -> io::Result<PathBuf> {
    if let Some(p) = std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .map(|d| d.join("cloud-hypervisor"))
        .find(|p| is_executable(p))
    {
        return Ok(p);
    }
    const SEARCH: [&str; 3] = [
        "out/cloud-hypervisor",
        "/usr/local/bin/cloud-hypervisor",
        "/usr/bin/cloud-hypervisor",
    ];
    for p in SEARCH {
        if Path::new(p).exists() {
            return std::path::absolute(p);
        }
    }
    Err(err(format!(
        "cloud-hypervisor not found on PATH or in {}",
        SEARCH.join(" or ")
    )))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}
