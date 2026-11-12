//! The agent as the initramfs's init. The worker builds each boot's
//! initramfs from the agent alone (`vm.WriteInitrd`), so no image carries
//! an initramfs, and the agent that runs is always the worker's.
//!
//! As PID 1 it assembles the root and hands over to the image's own init:
//!
//! ```text
//! /base   the image, read-only: an EROFS image on the device the kernel
//!         command line names (hangar.image=), with DAX if
//!         hangar.image_dax=1
//! /rw     the environment's writable disk, the first virtio-blk device
//! /lower  overlayfs of the two
//! /root   hangarfs over /lower, which becomes /
//! ```
//!
//! and swaps to the disk hangar.swap= names, if it names one.
//!
//! /var/lib/docker is not part of the root: overlay2 cannot stack on
//! overlayfs. It is /rw/docker, a directory on the same writable disk,
//! mounted there beside the root.

#[cfg(target_os = "linux")]
pub use boot::{AGENT, OOM_DROP_IN, run};

/// The first page of a swap device of `size` bytes: room for a boot
/// sector, then struct swap_header's info at 1024 -- version, last page,
/// bad pages, UUID, label -- and the magic ending the page. None for a size
/// swap cannot be.
pub fn swap_header(size: u64, page: usize, uuid: [u8; 16]) -> Option<Vec<u8>> {
    let pages = size / page as u64;
    if pages < 2 || pages - 1 > u32::MAX as u64 {
        return None;
    }
    let mut h = vec![0; page];
    h[1024..1028].copy_from_slice(&1u32.to_ne_bytes());
    h[1028..1032].copy_from_slice(&((pages - 1) as u32).to_ne_bytes());
    h[1036..1052].copy_from_slice(&uuid);
    h[1052..1052 + 11].copy_from_slice(b"hangar-swap");
    h[page - 10..].copy_from_slice(b"SWAPSPACE2");
    Some(h)
}

/// Whether a machine ID is one: 32 lower-case hex digits.
pub fn valid_machine_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A new machine ID from 16 random bytes: version 4, as systemd makes them.
pub fn machine_id(mut raw: [u8; 16]) -> String {
    raw[6] = raw[6] & 0x0f | 0x40;
    raw[8] = raw[8] & 0x3f | 0x80;
    raw.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(target_os = "linux")]
mod boot {
    use super::{machine_id, swap_header, valid_machine_id};

    use std::ffi::CString;
    use std::fs;
    use std::io::{self, Read, Seek, SeekFrom, Write};
    use std::os::unix::fs::{DirBuilderExt, FileExt, OpenOptionsExt};
    use std::os::unix::process::CommandExt;
    use std::path::Path;
    use std::time::{Duration, Instant};

    /// Where init installs the agent in the root, which the image's
    /// hangar-agent.service runs.
    pub const AGENT: &str = "/usr/local/bin/hangar-agent";

    /// The agent's Rust half, as an earlier agent installed it beside a Go
    /// agent: removed, since nothing runs it.
    const OLD_RUST_HALF: &str = "/usr/local/bin/hangar-agent-rs";

    const UPPER_DISK: &str = "/dev/vda";
    const NEW_ROOT: &str = "/root";
    /// Where the overlay hangarfs serves is mounted.
    const LOWER: &str = "/lower";
    /// How long a device the kernel may still be probing is waited for.
    const DISK_WAIT: Duration = Duration::from_secs(10);

    /// The agent's unit's OOM settings, which init installs with the agent, so
    /// they come with the agent the worker supplies whatever the image's own
    /// unit says.
    ///
    /// The agent is never killed: every terminal and SSH session dies with it.
    /// What it starts is given a score of its own (`user::prepare`).
    ///
    /// OOMPolicy=continue keeps the unit running when the kernel kills one of
    /// its processes. Everything the agent starts is in its unit, and systemd's
    /// default for a service, stop, would end the lot -- every terminal, the
    /// Code service, the desktop -- for one build that ran out of memory.
    pub const OOM_DROP_IN: &str = "[Service]\nOOMScoreAdjust=-1000\nOOMPolicy=continue\n";

    /// In the system's unit search path, beside the agent in /usr/local, so the
    /// image's unit and /etc are left to it and the user.
    const OOM_DROP_IN_PATH: &str = "/usr/local/lib/systemd/system/hangar-agent.service.d/oom.conf";

    /// linux/swap.h's SWAP_FLAG_DISCARD: the device is discarded whole when
    /// swapping to it starts, and each run of pages as it is freed.
    const SWAP_FLAG_DISCARD: libc::c_int = 0x10000;

    /// Assembles the root and executes the image's init. It returns only if
    /// that fails, having said why on the console, and then never: PID 1
    /// exiting panics the kernel, and waiting leaves the message on the
    /// console for whoever reads it.
    pub fn run() -> ! {
        let e = assemble();
        eprintln!("INITRAMFS: {e}");
        eprintln!("INITRAMFS: cannot start the environment; see above");
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }

    fn mount(
        src: &str,
        dst: &str,
        fstype: &str,
        flags: libc::c_ulong,
        data: &str,
    ) -> io::Result<()> {
        let c = |s: &str| CString::new(s).map_err(io::Error::other);
        let (src, dst, fstype, data) = (c(src)?, c(dst)?, c(fstype)?, c(data)?);
        // SAFETY: every pointer is to a live, NUL-terminated string.
        let rc = unsafe {
            libc::mount(
                src.as_ptr(),
                dst.as_ptr(),
                fstype.as_ptr(),
                flags,
                data.as_ptr().cast(),
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn context(what: &str) -> impl Fn(io::Error) -> io::Error + '_ {
        move |e| io::Error::new(e.kind(), format!("{what}: {e}"))
    }

    fn cmdline() -> Vec<String> {
        fs::read_to_string("/proc/cmdline")
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect()
    }

    /// The value of `key=` on the kernel's command line.
    fn arg(args: &[String], key: &str) -> Option<String> {
        args.iter()
            .rev()
            .find_map(|a| a.strip_prefix(key)?.strip_prefix('='))
            .map(str::to_string)
    }

    /// Waits for a device node, which the kernel may still be probing.
    fn wait_for(device: &str) -> io::Result<()> {
        let deadline = Instant::now() + DISK_WAIT;
        while fs::metadata(device).is_err() {
            if Instant::now() > deadline {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("no device at {device}"),
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }

    fn mkdir(dir: &str, mode: u32) -> io::Result<()> {
        fs::DirBuilder::new().recursive(true).mode(mode).create(dir)
    }

    fn assemble() -> io::Error {
        match try_assemble() {
            Ok(never) => match never {},
            Err(e) => e,
        }
    }

    enum Never {}

    fn try_assemble() -> io::Result<Never> {
        for d in ["/proc", "/sys", "/dev", "/base", "/rw", NEW_ROOT] {
            mkdir(d, 0o755)?;
        }
        for (src, dst, fstype) in [
            ("proc", "/proc", "proc"),
            ("sysfs", "/sys", "sysfs"),
            ("devtmpfs", "/dev", "devtmpfs"),
        ] {
            mount(src, dst, fstype, 0, "").map_err(context(&format!("mounting {dst}")))?;
        }
        let args = cmdline();
        mount_base(&args)?;

        wait_for(UPPER_DISK).map_err(context("the writable disk"))?;
        // discard hands freed blocks back to the host as files are deleted:
        // the disk is a sparse file there, and without it everything an
        // environment ever wrote stays allocated. Nothing in the guest can trim
        // it later, for systemd's /run hides where it is mounted.
        mount(UPPER_DISK, "/rw", "ext4", 0, "discard").map_err(context(&format!(
            "mounting the writable layer ({UPPER_DISK})"
        )))?;
        mkdir("/rw/upper", 0o755)?;
        mkdir("/rw/work", 0o755)?;
        // The overlay is hangarfs's lower directory, left in the initramfs:
        // hangarfs holds it for as long as it is mounted. transparent: hangarfs
        // takes no stacking depth of its own, so an overlay can still be
        // mounted on the root.
        mkdir(LOWER, 0o755)?;
        mount(
            "overlay",
            LOWER,
            "overlay",
            0,
            "lowerdir=/base,upperdir=/rw/upper,workdir=/rw/work",
        )
        .map_err(context("stacking overlayfs"))?;
        mount(LOWER, NEW_ROOT, "hangarfs", 0, "transparent")
            .map_err(context("mounting hangarfs over the overlay"))?;

        // Without it the environment runs as it would with none.
        if let Err(e) = swap_on(&args) {
            eprintln!("INITRAMFS: swap: {e}");
        }
        // Without it Docker cannot start, but the environment is still usable.
        if let Err(e) = mount_docker() {
            eprintln!("INITRAMFS: mounting Docker's store: {e}");
        }

        // The agent the worker supplied, installed where the image's
        // hangar-agent.service runs it.
        let root = Path::new(NEW_ROOT);
        copy_file(Path::new("/init"), &root.join(&AGENT[1..]), 0o755)
            .map_err(context("installing the agent"))?;
        let _ = fs::remove_file(root.join(&OLD_RUST_HALF[1..]));
        install_oom_drop_in(root).map_err(context("installing the agent's OOM settings"))?;
        ensure_machine_id(root).map_err(context("giving the environment a machine ID"))?;

        // The layers stay reachable in the new root rather than orphaned, and
        // the kernel's filesystems move with it.
        for (from, to) in [
            ("/base", "/run/hangar/base"),
            ("/rw", "/run/hangar/rw"),
            ("/dev", "/dev"),
            ("/proc", "/proc"),
            ("/sys", "/sys"),
        ] {
            let to = format!("{NEW_ROOT}{to}");
            mkdir(&to, 0o755)?;
            mount(from, &to, "", libc::MS_MOVE, "")
                .map_err(context(&format!("moving {from} into the new root")))?;
        }
        // switch_root: the initramfs's own files are freed, the new root is
        // moved over /, and the image's init replaces this process.
        let _ = fs::remove_file("/init");
        std::env::set_current_dir(NEW_ROOT)?;
        mount(".", "/", "", libc::MS_MOVE, "").map_err(context("moving the new root to /"))?;
        // SAFETY: chroot(2) to the current directory.
        if unsafe { libc::chroot(c".".as_ptr()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        std::env::set_current_dir("/")?;
        for init in [
            "/sbin/init",
            "/lib/systemd/systemd",
            "/usr/lib/systemd/systemd",
        ] {
            if Path::new(init).exists() {
                let e = std::process::Command::new(init).exec();
                return Err(io::Error::new(e.kind(), format!("executing {init}: {e}")));
            }
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the image has no init (/sbin/init)",
        ))
    }

    /// Mounts the image at /base.
    fn mount_base(args: &[String]) -> io::Result<()> {
        let Some(device) = arg(args, "hangar.image") else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "the kernel command line names no image (hangar.image=)",
            ));
        };
        let dax = args.iter().any(|a| a == "hangar.image_dax=1");
        wait_for(&device)?;
        let opts = if dax { "dax=always" } else { "" };
        mount(&device, "/base", "erofs", libc::MS_RDONLY, opts)
            .map_err(context(&format!("mounting the base from {device}")))?;
        eprintln!(
            "INITRAMFS: base from {device} ({})",
            if dax { "erofs, dax" } else { "erofs" }
        );
        Ok(())
    }

    /// Swaps to the disk the kernel command line names (hangar.swap=), if it
    /// names one. The worker gives each boot an empty disk, so the swap header
    /// is written here, for this kernel's page size. Discarding is what hands
    /// the host back the space of what is swapped in again: the disk is a
    /// sparse file there.
    fn swap_on(args: &[String]) -> io::Result<()> {
        let Some(device) = arg(args, "hangar.swap") else {
            return Ok(());
        };
        wait_for(&device)?;
        let mut f = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&device)?;
        let size = f.seek(SeekFrom::End(0))?;
        // SAFETY: sysconf has no preconditions.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let mut uuid = [0; 16];
        fs::File::open("/dev/urandom")?.read_exact(&mut uuid)?;
        let header = swap_header(size, page, uuid).ok_or_else(|| {
            io::Error::other(format!(
                "{device} is {size} bytes, which is no size for swap"
            ))
        })?;
        f.write_all_at(&header, 0)?;
        f.sync_all()?;
        drop(f);
        let path = CString::new(device.as_str()).map_err(io::Error::other)?;
        // SAFETY: swapon(2) of a NUL-terminated path.
        if unsafe { libc::swapon(path.as_ptr(), SWAP_FLAG_DISCARD) } < 0 {
            let e = io::Error::last_os_error();
            return Err(io::Error::new(e.kind(), format!("swapon {device}: {e}")));
        }
        eprintln!("INITRAMFS: swap on {device} ({} MiB)", size >> 20);
        Ok(())
    }

    /// Gives the new root its Docker store at /var/lib/docker.
    fn mount_docker() -> io::Result<()> {
        let target = format!("{NEW_ROOT}/var/lib/docker");
        mkdir(&target, 0o710)?;
        mkdir("/rw/docker", 0o710)?;
        mount("/rw/docker", &target, "", libc::MS_BIND, "")
    }

    fn copy_file(from: &Path, to: &Path, mode: u32) -> io::Result<()> {
        if let Some(dir) = to.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut tmp = to.as_os_str().to_owned();
        tmp.push(".new");
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(&tmp)?;
        io::copy(&mut fs::File::open(from)?, &mut out)?;
        out.flush()?;
        drop(out);
        fs::rename(&tmp, to)
    }

    fn install_oom_drop_in(root: &Path) -> io::Result<()> {
        let p = root.join(&OOM_DROP_IN_PATH[1..]);
        if let Some(dir) = p.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(p, OOM_DROP_IN)
    }

    /// Gives the environment its own machine ID on its first boot, where the
    /// image has none, before systemd starts. With one, systemd does not treat
    /// the boot as a machine's first, whose work -- applying the presets -- the
    /// image did when it was built, and which on some distributions resets the
    /// default target the image set. The ID is kept on the writable layer, so
    /// the environment keeps it.
    fn ensure_machine_id(root: &Path) -> io::Result<()> {
        let path = root.join("etc/machine-id");
        if fs::read_to_string(&path).is_ok_and(|s| valid_machine_id(s.trim())) {
            return Ok(());
        }
        let mut raw = [0; 16];
        fs::File::open("/dev/urandom")?.read_exact(&mut raw)?;
        fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o444)
            .open(&path)?
            .write_all(format!("{}\n", machine_id(raw)).as_bytes())
    }
}
