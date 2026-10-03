// SPDX-License-Identifier: Apache-2.0

//! hangar-router: a FUSE filesystem that is an environment's root.
//!
//! It merges a writable upper directory over read-only lower ones, as
//! overlayfs would (`union.rs`), and is where shared paths will be routed
//! (`docs/root-filesystem.md`). File contents go straight to the real file
//! (FUSE passthrough), and entries and attributes are cached by the kernel
//! for a long time, since nothing but the router changes the layers.

mod union;

use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use fuser::{Config, MountOption, SessionACL};

#[derive(Parser, Debug)]
#[command(about = "An environment's root: a writable layer merged over read-only ones, as FUSE")]
struct Args {
    /// The writable layer.
    #[arg(long)]
    upper: PathBuf,

    /// A read-only layer under it, topmost first. May be given more than
    /// once, or not at all.
    #[arg(long)]
    lower: Vec<PathBuf>,

    /// Where to mount the router.
    #[arg(long)]
    mountpoint: PathBuf,

    /// Worker threads serving requests.
    #[arg(long, default_value_t = 4)]
    threads: usize,

    /// How long the kernel may keep entries and attributes, in seconds.
    #[arg(long, default_value_t = 3600)]
    ttl: u64,

    /// Serve requests over FUSE's io_uring transport, with this many ring
    /// entries for each CPU, as well as on the worker threads, which then
    /// carry only forgets and interrupts. 0 is /dev/fuse alone.
    #[arg(long, default_value_t = 0)]
    uring: usize,

    /// Serve file contents through the router rather than passing them to
    /// the backing file.
    #[arg(long)]
    no_passthrough: bool,
}

/// Raises the open file limit as far as the kernel allows.
///
/// The router holds a descriptor for every file the kernel knows about, and
/// with long cache timeouts the kernel knows about everything recently seen:
/// a checkout of a large repository alone is tens of thousands. As root it can
/// raise its hard limit to the kernel's own ceiling.
fn raise_file_limit() {
    let ceiling = std::fs::read_to_string("/proc/sys/fs/nr_open")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok());
    let mut lim = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } != 0 {
        log::warn!("could not read the open file limit: {}", std::io::Error::last_os_error());
        return;
    }
    let want = ceiling.unwrap_or(lim.rlim_max).max(lim.rlim_max);
    let raised = libc::rlimit { rlim_cur: want, rlim_max: want };
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raised) } == 0 {
        log::info!("open file limit raised to {want}");
        return;
    }
    // Not root: as far as the hard limit.
    lim.rlim_cur = lim.rlim_max;
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lim) } != 0 {
        log::warn!("could not raise the open file limit: {}", std::io::Error::last_os_error());
    }
}

/// Turns on the kernel's FUSE over io_uring, which is off unless the fuse
/// module's enable_uring parameter is set. Without it the router serves
/// /dev/fuse alone.
fn enable_uring() {
    const PARAM: &str = "/sys/module/fuse/parameters/enable_uring";
    if std::fs::read_to_string(PARAM).is_ok_and(|v| v.trim() == "Y") {
        return;
    }
    if let Err(e) = std::fs::write(PARAM, "Y") {
        log::warn!("enabling FUSE over io_uring ({PARAM}): {e}");
    }
}

fn main() {
    env_logger::init();
    let args = Args::parse();
    raise_file_limit();

    let fs = union::Router::new(
        &args.upper,
        &args.lower,
        Duration::from_secs(args.ttl),
        !args.no_passthrough,
    )
    .unwrap_or_else(|e| {
        eprintln!("hangar-router: opening the layers: {e}");
        std::process::exit(1);
    })
    .with_uring(args.uring > 0);
    if args.uring > 0 {
        enable_uring();
    }

    let mut cfg = Config::default();
    cfg.mount_options.extend([
        MountOption::FSName("hangar-router".to_string()),
        MountOption::Subtype("hangar-router".to_string()),
        // Permissions are the kernel's to check, from the attributes the
        // router reports: the router itself acts as root.
        MountOption::DefaultPermissions,
        MountOption::Dev,
        MountOption::Suid,
    ]);
    cfg.acl = SessionACL::All;
    cfg.n_threads = Some(args.threads);
    cfg.clone_fd = true;
    if args.uring > 0 {
        cfg.io_uring = Some(args.uring);
    }

    if let Err(e) = fuser::mount(fs, &args.mountpoint, &cfg) {
        eprintln!("hangar-router: mounting at {}: {e}", args.mountpoint.display());
        std::process::exit(1);
    }
}
