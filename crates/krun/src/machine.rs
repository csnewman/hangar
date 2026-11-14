//! The machine a Config describes, built and run under libkrun.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

use hangar_api::monitor::Config;
use krun::{
    BalloonDevice, BlockDevice, ConsoleDevice, DiskFormat, KernelFormat, LogLevel, LogOptions,
    LogStyle, MemDevice, NetDevice, NetFlags, Payload, RngDevice, SyncMode, TsiFlags, VmmBuilder,
    VsockDevice,
};

use crate::control::Control;

/// A disk's queues at most, as Cloud Hypervisor's are bounded: past a few
/// the guest gains little, and each is a thread.
const MAX_DISK_QUEUES: u32 = 8;

/// The offloads virtio-net asks passt for: checksums, TSO and UFO both
/// ways.
const NET_OFFLOADS: u32 = 1 << 0 | 1 << 1 | 1 << 7 | 1 << 10 | 1 << 11 | 1 << 14;

/// The guest's vsock CID.
const GUEST_CID: u64 = 3;

/// Every device on virtio-mmio: libkrun has no virtio-pci on macOS.
#[cfg(target_os = "macos")]
type Devices<'a> = krun::MmioDeviceManager<'a>;

/// Every device on virtio-pci, with an MSI-X vector for each queue:
/// virtio-mmio's interrupt costs two exits to this process each time it
/// fires, which nested virtualization makes dear. libkrun puts a device
/// with shared memory, the GPU, on virtio-mmio itself. Devices take PCI
/// slots in the order they are added, so the disks keep theirs.
#[cfg(not(target_os = "macos"))]
type Devices<'a> = krun::PciDeviceManager<'a>;

fn failed(what: &str) -> impl FnOnce(krun::VmmError) -> String + '_ {
    move |e| format!("{what}: {e}")
}

/// Builds the machine c describes and runs it until the guest exits, then
/// exits the process.
pub fn start(c: &Config, ctl: &Control) -> Result<(), String> {
    // Standard error, which the worker keeps as the monitor's log.
    let level = match std::env::var("HANGAR_KRUN_LOG")
        .ok()
        .and_then(|l| l.parse::<u32>().ok())
    {
        Some(0) => LogLevel::Off,
        Some(1) => LogLevel::Error,
        Some(3) => LogLevel::Info,
        Some(4) => LogLevel::Debug,
        Some(n) if n >= 5 => LogLevel::Trace,
        _ => LogLevel::Warn,
    };
    // SAFETY: standard error is open for the life of the process.
    let stderr = unsafe { BorrowedFd::borrow_raw(2) };
    krun::init_log(Some(stderr), level, LogStyle::Never, LogOptions::empty())
        .map_err(failed("logging"))?;

    let mut devices = Devices::new();

    // The guest's console, hvc0, to the console file. libkrun makes a port
    // the console only with a terminal behind it, so it is given a
    // pseudo-terminal, whose output is copied to the file; the guest reads
    // nothing from it.
    let console = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&c.console)
        .map_err(|e| format!("{}: {e}", c.console))?;
    let tty = console_tty(console)?;
    let mut cb = ConsoleDevice::builder();
    cb.add_tty_port("", tty).map_err(failed("the console"))?;
    devices.add(cb.build().map_err(failed("the console"))?);

    for d in &c.disks {
        let mut blk = BlockDevice::new(&d.id, &d.path, DiskFormat::Raw)
            .map_err(|e| format!("disk {}: {e}", d.id))?;
        blk.set_read_only(d.read_only);
        blk.set_direct_io(d.direct);
        // Relaxed flushes reach the host's buffers; on macOS they do not
        // wait for the drive, as libkrun's own default there.
        blk.set_sync_mode(SyncMode::Relaxed);
        // A queue per vCPU, each with a worker of its own, as Cloud
        // Hypervisor gives a disk, and on Linux, where every disk here is
        // a raw image, a ring per queue reading and writing straight
        // between the image and guest memory. Where there is no ring,
        // reads and writes are served by one pool of threads the queues
        // share.
        blk.set_num_queues(c.cpus.clamp(1, MAX_DISK_QUEUES) as u16);
        blk.set_io_uring(true);
        blk.set_parallel_reads(true);
        blk.set_parallel_writes(true);
        devices.add(blk);
    }

    // vsock without TSI, libkrun's socket redirection, which needs its own
    // patched guest kernel: the guest's sockets are its own.
    let mut vs = VsockDevice::new(GUEST_CID, TsiFlags::empty()).map_err(failed("vsock"))?;
    for (port, sock) in &c.host_ports {
        vs.add_unix_port(*port, sock, false);
    }
    for (port, sock) in &c.guest_ports {
        let _ = std::fs::remove_file(sock);
        vs.add_unix_port(*port, sock, true);
    }
    devices.add(vs);

    if let Some(n) = &c.net {
        let mac = parse_mac(&n.mac).ok_or_else(|| format!("the MAC address {:?}", n.mac))?;
        // passt, which takes the offloads virtio-net asks for.
        let nd = match (n.stream, n.fd) {
            (true, Some(fd)) => {
                // SAFETY: the worker hands the socket down as this
                // descriptor, and nothing else here takes it.
                let fd = unsafe { OwnedFd::from_raw_fd(fd) };
                // Programs this process starts (the render server) must not
                // hold it open, or passt outlives the machine.
                // SAFETY: fd is open.
                unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
                NetDevice::new_unixstream_fd("net0", fd, &mac, NET_OFFLOADS, NetFlags::empty())
            }
            (true, None) => NetDevice::new_unixstream_path(
                "net0",
                &n.socket,
                &mac,
                NET_OFFLOADS,
                NetFlags::empty(),
            ),
            (false, _) => NetDevice::new_unixgram_path("net0", &n.socket, &mac, 0, NetFlags::VFKIT),
        }
        .map_err(failed("the network"))?;
        devices.add(nd);
    }

    if let Some(g) = &c.gpu {
        add_gpu(&mut devices, g)?;
    }

    // Memory the guest frees goes back to the host.
    devices.add(BalloonDevice::new().map_err(failed("the balloon"))?);
    if c.hotplug_mib > 0 {
        let m = MemDevice::new(c.hotplug_mib).map_err(failed("the hotpluggable memory"))?;
        let h = m.handle();
        // A restored machine's device has what it had when it was
        // suspended.
        if c.plugged_mib > 0 && c.restore.is_empty() {
            h.set_requested_mib(c.plugged_mib)
                .map_err(failed("plugging memory in"))?;
        }
        let _ = ctl.memory.set(h);
        devices.add(m);
    }
    devices.add(RngDevice::new().map_err(failed("the random number generator"))?);

    // libkrun loads an x86 kernel as ELF (vmlinux), an arm64 one raw.
    let format = if cfg!(target_arch = "x86_64") {
        KernelFormat::Elf
    } else {
        KernelFormat::Raw
    };
    let initrd = (!c.initrd.is_empty()).then_some(c.initrd.as_str());
    let payload = Payload::load_external(&c.kernel, format, initrd, &c.cmdline)
        .map_err(failed("loading the kernel"))?;

    let mut b = VmmBuilder::new()
        .vcpus(u8::try_from(c.cpus).map_err(|_| format!("{} vCPUs", c.cpus))?)
        .map_err(failed("sizing the machine"))?
        .ram_mib(c.memory_mib)
        .map_err(failed("sizing the machine"))?;
    // Guest memory asks for huge pages: on a host whose THP is madvise, as
    // Ubuntu's is, it otherwise gets 4 KiB pages, and a nested guest then
    // takes minutes to boot.
    if cfg!(target_os = "linux") {
        b = b
            .transparent_huge_pages(true)
            .map_err(failed("sizing the machine"))?;
    }
    // An x86 guest finds its devices, PCI and the GPU's virtio-mmio alike,
    // in the ACPI tables; the kernel has no command-line virtio-mmio
    // devices.
    if cfg!(target_arch = "x86_64") {
        b = b.acpi(true).map_err(failed("describing the machine"))?;
    }
    b = b.payload(payload).devices(devices);
    if !c.restore.is_empty() {
        b = b
            .restore(&c.restore)
            .map_err(failed("restoring the machine"))?;
    }
    let vmm = b.build().map_err(failed("building the machine"))?;
    let handle = vmm
        .handle()
        .map_err(failed("getting the machine's handle"))?;
    let _ = ctl.vmm.set(handle);
    vmm.run();
    std::process::exit(0);
}

#[cfg(feature = "gpu")]
fn add_gpu(devices: &mut Devices<'_>, g: &hangar_api::monitor::Gpu) -> Result<(), String> {
    use krun::{GpuDevice, VirglRendererFlags as F};
    // virglrenderer's VIRGL_RENDERER_NO_VIRGL, which libkrun passes on
    // though it does not name it.
    const NO_VIRGL: u32 = 1 << 7;
    let flags = if g.virgl {
        // OpenGL and Vulkan, rendered as the Linux GPU backend renders
        // them: EGL with no display, so a host without one renders on its
        // CPU, and Venus in virglrenderer's render server.
        F::USE_EGL
            | F::USE_SURFACELESS
            | F::THREAD_SYNC
            | F::USE_ASYNC_FENCE_CB
            | F::VENUS
            | F::RENDER_SERVER
    } else {
        // Vulkan alone, through Venus in virglrenderer's render server.
        F::from_bits_retain(
            (F::VENUS | F::RENDER_SERVER | F::THREAD_SYNC | F::USE_ASYNC_FENCE_CB).bits()
                | NO_VIRGL,
        )
    };
    let gpu = GpuDevice::new(flags, crate::display::backend()?)
        .shm_size((g.window_mib as usize) << 20)
        // The guest's pages are the host's (a 16 KiB kernel on the Mac), so
        // its blobs need no aligning, and its Mesa (before 26.2) can't
        // create a Venus instance when asked to align them.
        .blob_alignment(false);
    devices.add(gpu);
    Ok(())
}

#[cfg(not(feature = "gpu"))]
fn add_gpu(_: &mut Devices<'_>, _: &hangar_api::monitor::Gpu) -> Result<(), String> {
    Err("the GPU: this hangar-krun was built without one".into())
}

/// A pseudo-terminal whose output is copied to console, raw, so the guest's
/// output reaches the file as it wrote it; the terminal's end, for the
/// guest's console port, is held for the life of the process.
fn console_tty(mut console: File) -> Result<BorrowedFd<'static>, String> {
    let (mut master, mut slave) = (0, 0);
    // SAFETY: openpty fills the two descriptors.
    if unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    } != 0
    {
        return Err(format!("the console: {}", std::io::Error::last_os_error()));
    }
    // SAFETY: openpty returned these descriptors, which nothing else owns.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    // SAFETY: tcgetattr fills t for a terminal descriptor.
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(slave.as_raw_fd(), &mut t) } == 0 {
        t.c_oflag &= !libc::OPOST;
        t.c_lflag &= !(libc::ECHO | libc::ICANON);
        unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &t) };
    }
    let mut out = File::from(master);
    // For debugging a guest by hand: what is written to the FIFO named by
    // HANGAR_KRUN_CONSOLE_IN is typed at its console. The worker never sets
    // it.
    if let Some(fifo) = std::env::var_os("HANGAR_KRUN_CONSOLE_IN") {
        let mut typed = out.try_clone().map_err(|e| format!("the console: {e}"))?;
        std::thread::spawn(move || {
            while let Ok(mut f) = File::open(&fifo) {
                let _ = std::io::copy(&mut f, &mut typed);
            }
        });
    }
    std::thread::spawn(move || {
        let mut buf = [0u8; 16 << 10];
        while let Ok(n) = out.read(&mut buf) {
            if n == 0 || console.write_all(&buf[..n]).is_err() {
                break;
            }
        }
    });
    let fd = slave.as_raw_fd();
    std::mem::forget(slave);
    // SAFETY: the descriptor was leaked above, so stays open as long as
    // the process.
    Ok(unsafe { BorrowedFd::borrow_raw(fd) })
}

fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let mut mac = [0u8; 6];
    let mut parts = s.split(':');
    for b in &mut mac {
        *b = u8::from_str_radix(parts.next()?, 16).ok()?;
    }
    parts.next().is_none().then_some(mac)
}
