// SPDX-License-Identifier: Apache-2.0

//! A virtio-fs device served over vhost-user, with a DAX window.
//!
//! What distinguishes it from `virtiofsd` is that it answers `setupmapping`.
//! virtiofsd returns `ENOSYS` for it, so a guest can never map a file and DAX
//! is unreachable no matter what the monitor offers. Here a mapping request
//! becomes a `SHMEM_MAP` to the monitor, which places the file in the window
//! the guest already has mapped.

mod fsopts;
mod server;

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use clap::Parser;
use vhost_user_backend::VhostUserDaemon;
use vm_memory::{GuestMemoryAtomic, GuestMemoryMmap};

use crate::server::{FsBackend, FsConfig};

#[derive(Parser, Debug)]
#[command(about = "virtio-fs vhost-user backend with DAX")]
struct Args {
    /// Where to listen for the monitor.
    #[arg(long)]
    socket: PathBuf,

    /// The directory to export.
    #[arg(long)]
    shared_dir: PathBuf,

    /// The mount tag the guest uses.
    #[arg(long, default_value = "hangar-base")]
    tag: String,

    /// Map files at least this large rather than reading them. Zero turns
    /// mapping off entirely, which is virtiofsd's behaviour.
    #[arg(long, default_value_t = 4096)]
    dax_min_file_size: u64,

    /// The identifier the guest looks the DAX window up by. virtio-fs
    /// defines 0 for its cache.
    #[arg(long, default_value_t = 0)]
    shm_id: u8,
}

/// Raises the open file limit to the hard limit.
///
/// A passthrough filesystem holds a descriptor per inode the guest has open,
/// so the guest's idea of how many files it may open is really this process's.
/// At the default soft limit a guest doing ordinary work -- containerd
/// starting, a build running -- reaches it and sees EMFILE from operations
/// that have nothing wrong with them.
fn raise_file_limit() {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: lim is a valid rlimit for the kernel to fill in.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } != 0 {
        log::warn!("could not read the open file limit: {}", std::io::Error::last_os_error());
        return;
    }
    if lim.rlim_cur >= lim.rlim_max {
        return;
    }
    lim.rlim_cur = lim.rlim_max;
    // SAFETY: lim is a valid rlimit and rlim_cur is within rlim_max.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lim) } != 0 {
        log::warn!("could not raise the open file limit: {}", std::io::Error::last_os_error());
        return;
    }
    log::info!("open file limit raised to {}", lim.rlim_max);
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = Args::parse();
    raise_file_limit();

    let config = FsConfig {
        shared_dir: args.shared_dir.clone(),
        tag: args.tag.clone(),
        dax_min_file_size: args.dax_min_file_size,
        shm_id: args.shm_id,
    };

    let backend = match FsBackend::new(config) {
        Ok(b) => Arc::new(RwLock::new(b)),
        Err(e) => {
            log::error!("virtio-fs backend: {e}");
            std::process::exit(1);
        }
    };

    let mut daemon = VhostUserDaemon::new(
        "hangar-fs".to_string(),
        backend,
        GuestMemoryAtomic::new(GuestMemoryMmap::new()),
    )
    .expect("building the daemon");

    let _ = std::fs::remove_file(&args.socket);
    let mut listener =
        vhost::vhost_user::Listener::new(&args.socket, true).expect("listening on the socket");

    log::info!(
        "virtio-fs backend on {} exporting {} as {}, dax from {} bytes",
        args.socket.display(),
        args.shared_dir.display(),
        args.tag,
        args.dax_min_file_size
    );
    if let Err(e) = daemon.start(&mut listener) {
        log::error!("daemon: {e:?}");
        std::process::exit(1);
    }
    if let Err(e) = daemon.wait() {
        log::error!("daemon exited: {e:?}");
    }
    let _ = std::fs::remove_file(&args.socket);
}
