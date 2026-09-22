// SPDX-License-Identifier: Apache-2.0

//! A virtio-gpu device served over vhost-user and rendered by rutabaga_gfx.
//!
//! The monitor carries the guest's virtqueues to this process and publishes a
//! shared memory window on its behalf; everything about rendering happens
//! here. Keeping it out of the monitor means the renderer's syscall surface,
//! its native libraries and its crashes all belong to a process that holds no
//! guest memory mapping it did not ask for.

mod device;
mod protocol;

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use clap::Parser;
use vhost_user_backend::VhostUserDaemon;
use vm_memory::{GuestMemoryAtomic, GuestMemoryMmap};

use crate::device::{GpuBackend, GpuConfig};

#[derive(Parser, Debug)]
#[command(about = "virtio-gpu vhost-user backend")]
struct Args {
    /// Where to listen for the monitor.
    #[arg(long)]
    socket: PathBuf,

    /// Offer OpenGL through virgl.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    virgl: bool,

    /// Offer Vulkan through venus, which needs a shared memory window and
    /// virglrenderer's render server.
    #[arg(long, default_value_t = false, action = clap::ArgAction::Set)]
    venus: bool,

    /// The identifier the guest looks the shared memory window up by.
    #[arg(long, default_value_t = 1)]
    shm_id: u8,
}

fn main() {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info"),
    )
    .init();

    let args = Args::parse();
    let config = GpuConfig {
        virgl: args.virgl,
        venus: args.venus,
        shm_id: args.shm_id,
    };

    let backend = match GpuBackend::new(config) {
        Ok(b) => Arc::new(RwLock::new(b)),
        Err(e) => {
            log::error!("virtio-gpu backend: {e}");
            std::process::exit(1);
        }
    };

    let mut daemon = VhostUserDaemon::new(
        "hangar-gpu".to_string(),
        backend.clone(),
        GuestMemoryAtomic::new(GuestMemoryMmap::new()),
    )
    .expect("building the daemon");

    let _ = std::fs::remove_file(&args.socket);
    let mut listener = vhost::vhost_user::Listener::new(&args.socket, true)
        .expect("listening on the socket");

    log::info!("virtio-gpu backend listening on {}", args.socket.display());
    if let Err(e) = daemon.start(&mut listener) {
        log::error!("daemon: {e:?}");
        std::process::exit(1);
    }
    if let Err(e) = daemon.wait() {
        log::error!("daemon exited: {e:?}");
    }
    let _ = std::fs::remove_file(&args.socket);
}
