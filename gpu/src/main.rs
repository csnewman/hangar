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
mod replay;

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use clap::Parser;
use vhost_user_backend::VhostUserDaemon;
use vm_memory::{GuestMemoryAtomic, GuestMemoryMmap};
use vmm_sys_util::epoll::EventSet;

use crate::device::{GpuBackend, GpuConfig, RESTORE_EVENT, SAVE_EVENT};
use crate::replay::Session;

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

/// Has the session written out whenever SIGUSR1 arrives.
///
/// A signal rather than a socket because the only thing that ever asks is
/// whatever is about to snapshot this guest, and it already knows how to find
/// the process. The signal is blocked in every thread first, so it is
/// delivered to this one rather than interrupting a command midway, and the
/// write itself happens on the worker thread, which is the only one that can
/// reach the renderer.
fn save_on_signal(backend: Arc<RwLock<GpuBackend>>) {
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
        if let Err(e) = backend.read().unwrap().request_save() {
            log::error!("asking the worker to save: {e}");
        }
    });
}

/// Reads a saved session: the record, and the contents file beside it.
fn load_session(path: &std::path::Path) -> std::io::Result<(Session, Vec<u8>)> {
    let session: Session = serde_json::from_slice(&std::fs::read(path)?)
        .map_err(|e| std::io::Error::other(format!("{e}")))?;
    let contents = std::fs::read(path.with_extension("bin")).unwrap_or_default();
    Ok((session, contents))
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
    // already talking to. What the guest built is rebuilt once the monitor
    // has connected and handed over guest memory; see kick_restore.
    if let Some(p) = args.state.as_ref() {
        let _ = std::fs::remove_file(p.with_extension("ready"));
        if p.exists() {
            match load_session(p) {
                Ok((session, contents)) => {
                    log::info!(
                        "gpu session to restore: {} resources, {} contexts",
                        session.resources.len(),
                        session.contexts.len()
                    );
                    built.set_pending(session, contents);
                }
                Err(e) => log::error!("reading the gpu session from {}: {e}", p.display()),
            }
        }
    }
    let (save_fd, restore_fd) = (built.save_fd(), built.restore_fd());
    let backend = Arc::new(RwLock::new(built));
    if args.state.is_some() {
        save_on_signal(Arc::clone(&backend));
    }

    let mut daemon = VhostUserDaemon::new(
        "hangar-gpu".to_string(),
        backend.clone(),
        GuestMemoryAtomic::new(GuestMemoryMmap::new()),
    )
    .expect("building the daemon");

    // Saving and restoring have to run on the thread that owns the renderer,
    // which is the one serving the queues, so they arrive as events there.
    for (fd, event) in [(save_fd, SAVE_EVENT), (restore_fd, RESTORE_EVENT)] {
        daemon.get_epoll_handlers()[0]
            .register_listener(fd, EventSet::IN, event as u64)
            .expect("registering a worker event");
    }

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
