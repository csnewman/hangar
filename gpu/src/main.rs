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

use crate::device::{GpuBackend, GpuConfig, Live};

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

    /// Where to keep the set of objects the guest is holding.
    ///
    /// Read at startup if it is there, so a backend standing in for one the
    /// guest was already talking to knows which of its identifiers stand for
    /// nothing. Written on SIGUSR1, which is how a monitor about to snapshot
    /// this guest says so.
    #[arg(long)]
    state: Option<PathBuf>,
}

/// Writes the live objects out whenever SIGUSR1 arrives.
///
/// A signal rather than a socket because the only thing that ever asks is
/// whatever is about to snapshot this guest, and it already knows how to find
/// the process. The signal is blocked in every thread first, so it is
/// delivered to this one rather than interrupting a command midway.
fn save_on_signal(backend: Arc<RwLock<GpuBackend>>, path: PathBuf) {
    // SAFETY: a zeroed sigset is valid for sigemptyset to fill in.
    let mut set: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: set is a valid sigset for the duration of these calls.
    unsafe {
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGUSR1);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }
    std::thread::spawn(move || loop {
        let mut sig: libc::c_int = 0;
        // SAFETY: set and sig are valid for the call, which blocks until a
        // signal in the set is delivered.
        if unsafe { libc::sigwait(&set, &mut sig) } != 0 {
            return;
        }
        let live = backend.read().unwrap().live();
        let (r, c) = (live.resources.len(), live.contexts.len());
        match live.save(&path) {
            Ok(()) => log::info!(
                "gpu state saved: {r} resources, {c} contexts -> {}",
                path.display()
            ),
            Err(e) => log::error!("saving the gpu state to {}: {e}", path.display()),
        }
    });
}

fn main() {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info"),
    )
    .init();

    let args = Args::parse();
    let config = GpuConfig {
        state: args.state.clone(),
        virgl: args.virgl,
        venus: args.venus,
        shm_id: args.shm_id,
    };

    let mut built = match GpuBackend::new(config) {
        Ok(b) => b,
        Err(e) => {
            log::error!("virtio-gpu backend: {e}");
            std::process::exit(1);
        }
    };

    // A state file means this process is standing in for one the guest was
    // already talking to. Nothing behind those objects survived, so they are
    // recorded as lost and answered for rather than guessed at.
    if let Some(p) = args.state.as_ref().filter(|p| p.exists()) {
        match Live::load(p) {
            Ok(live) => built.restore(live),
            Err(e) => log::error!("reading the gpu state from {}: {e}", p.display()),
        }
    }
    let backend = Arc::new(RwLock::new(built));
    if let Some(p) = args.state.clone() {
        save_on_signal(Arc::clone(&backend), p);
    }

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
