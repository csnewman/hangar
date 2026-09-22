// SPDX-License-Identifier: Apache-2.0

//! The device: FUSE requests in, mapping requests out.

use std::io;
use std::os::fd::{BorrowedFd, RawFd};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fuse_backend_rs::abi::virtio_fs::RemovemappingOne;
use fuse_backend_rs::api::server::Server;
use fuse_backend_rs::passthrough::{CachePolicy, Config as PassthroughConfig, PassthroughFs};
use fuse_backend_rs::transport::{FsCacheReqHandler, Reader, VirtioFsWriter, Writer};
use vhost::vhost_user::message::{VhostUserMMap, VhostUserMMapFlags, VhostUserProtocolFeatures,
    VhostUserVirtioFeatures};
use vhost::vhost_user::{Backend, VhostUserFrontendReqHandler};
use vhost_user_backend::{VhostUserBackendMut, VringRwLock, VringT};
use virtio_queue::QueueT;
use virtio_bindings::bindings::virtio_config::{VIRTIO_F_NOTIFY_ON_EMPTY, VIRTIO_F_VERSION_1};
use virtio_bindings::bindings::virtio_ring::VIRTIO_RING_F_EVENT_IDX;
use vm_memory::{ByteValued, GuestAddressSpace, GuestMemoryAtomic, GuestMemoryMmap};
use vmm_sys_util::epoll::EventSet;

use crate::fsopts;
use crate::state::Session;

/// Tags are a fixed-width field in the configuration space.
const TAG_LEN: usize = 36;
const QUEUE_SIZE: u16 = 1024;
/// One high-priority queue and one request queue, which is the minimum a
/// guest driver expects.
const NUM_QUEUES: usize = 2;

/// How long the guest may trust what it has been told about a file.
///
/// A day rather than a literal forever because the FUSE field is a duration,
/// and an environment does not outlive one.
const CACHE_FOREVER: Duration = Duration::from_secs(86_400);

#[derive(Clone, Debug)]
pub struct FsConfig {
    pub shared_dir: PathBuf,
    pub tag: String,
    pub dax_min_file_size: u64,
    pub shm_id: u8,
    /// Where the session is written to and read back from. Without one the
    /// guest cannot outlive this process.
    pub state: Option<PathBuf>,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioFsConfig {
    tag: [u8; TAG_LEN],
    num_request_queues: u32,
}
// SAFETY: plain data.
unsafe impl ByteValued for VirtioFsConfig {}

impl Default for VirtioFsConfig {
    fn default() -> Self {
        Self {
            tag: [0; TAG_LEN],
            num_request_queues: 1,
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Cannot export {0}: {1}")]
    SharedDir(PathBuf, #[source] io::Error),
    #[error("The tag is longer than the {TAG_LEN} bytes the config space holds")]
    TagTooLong,
    #[error("Reading the session from {0}: {1}")]
    State(PathBuf, #[source] io::Error),
}

/// Turns the filesystem's mapping requests into requests to the monitor.
///
/// This is the whole of DAX on this side. `moffset` is an offset the guest
/// chose inside the window it read out of the shared memory capability, so
/// mapping is asking the monitor to place the host's file at the address the
/// guest is already expecting to find it.
struct CacheHandler {
    frontend: Arc<Mutex<Option<Backend>>>,
    shm_id: u8,
}

impl FsCacheReqHandler for CacheHandler {
    fn map(
        &mut self,
        foffset: u64,
        moffset: u64,
        len: u64,
        flags: u64,
        fd: RawFd,
    ) -> io::Result<()> {
        use fuse_backend_rs::abi::virtio_fs::SetupmappingFlags;
        let writable =
            SetupmappingFlags::from_bits_truncate(flags).contains(SetupmappingFlags::WRITE);
        let mut mmap_flags = 0u64;
        if writable {
            mmap_flags |= VhostUserMMapFlags::WRITABLE.bits();
        }

        let req = VhostUserMMap {
            shmid: self.shm_id,
            padding: [0; 7],
            fd_offset: foffset,
            shm_offset: moffset,
            len,
            flags: mmap_flags,
        };
        let guard = self.frontend.lock().unwrap();
        let frontend = guard.as_ref().ok_or_else(|| {
            log::error!("setupmapping with no channel to the monitor");
            io::Error::from_raw_os_error(libc::ENOSYS)
        })?;
        // SAFETY: the descriptor belongs to the filesystem and outlives the
        // call, which is synchronous.
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        let result = frontend.shmem_map(&req, &borrowed);
        log::info!(
            "setupmapping fd_offset={foffset:#x} shm_offset={moffset:#x} len={len:#x} \
             writable={writable} -> {:?}",
            result.as_ref().map(|_| ()).map_err(|e| e.to_string())
        );
        result.map(|_| ())
    }

    fn unmap(&mut self, requests: Vec<RemovemappingOne>) -> io::Result<()> {
        let guard = self.frontend.lock().unwrap();
        let frontend = guard
            .as_ref()
            .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOSYS))?;
        for r in requests {
            let req = VhostUserMMap {
                shmid: self.shm_id,
                padding: [0; 7],
                fd_offset: 0,
                shm_offset: r.moffset,
                len: r.len,
                flags: 0,
            };
            log::info!("removemapping shm_offset={:#x} len={:#x}", r.moffset, r.len);
            frontend.shmem_unmap(&req)?;
        }
        Ok(())
    }
}

/// Looks up every path the saved session names, so the filesystem holds the
/// nodeids the guest is still using.
///
/// A path that has gone is skipped: losing one file is better than refusing
/// to bring the guest back at all, and the guest finds out the same way it
/// would have without a restore.
fn restore_inodes(fs: &fsopts::Fs, session: &Session) {
    let order = session.replay_order();
    if order.is_empty() {
        return;
    }
    let ctx = fuse_backend_rs::api::filesystem::Context::default();
    let (mut ok, mut moved, mut gone) = (0usize, 0usize, 0usize);
    for (inode, path) in order {
        match fs.relookup(&ctx, &path) {
            Ok(got) if got == inode => ok += 1,
            Ok(got) => {
                moved += 1;
                log::warn!("{} came back as {got:#x}, not {inode:#x}", path.display());
            }
            Err(_) => gone += 1,
        }
    }
    log::info!("restored {ok} inodes, {moved} moved, {gone} gone");
}

pub struct FsBackend {
    config: FsConfig,
    vconfig: VirtioFsConfig,
    server: Arc<Server<Arc<fsopts::Fs>>>,
    mem: Option<GuestMemoryAtomic<GuestMemoryMmap>>,
    frontend: Arc<Mutex<Option<Backend>>>,
    event_idx: bool,
    session: Arc<Session>,
    fs: Arc<fsopts::Fs>,
    /// Mappings read from a saved session, waiting for a channel to the
    /// monitor to make them again.
    pending: Vec<crate::state::Mapping>,
}

impl FsBackend {
    pub fn new(config: FsConfig) -> Result<Self, Error> {
        let dir = config
            .shared_dir
            .canonicalize()
            .map_err(|e| Error::SharedDir(config.shared_dir.clone(), e))?;

        let cfg = PassthroughConfig {
            root_dir: dir.to_string_lossy().into_owned(),
            // What is exported is an image layer the host assembled, and the
            // host is its only writer, so the guest may hold what it is told
            // indefinitely.
            cache_policy: CachePolicy::Always,
            entry_timeout: CACHE_FOREVER,
            attr_timeout: CACHE_FOREVER,
            no_open: true,
            no_opendir: true,
            writeback: true,
            xattr: true,
            do_import: true,
            use_host_ino: true,
            // Below this a plain read is cheaper than a FUSE round trip plus
            // an mmap. Zero turns mapping off, which is virtiofsd's behaviour.
            dax_file_size: (config.dax_min_file_size > 0).then_some(config.dax_min_file_size),
            ..Default::default()
        };

        let passthrough = PassthroughFs::<()>::new(cfg)
            .map_err(|e| Error::SharedDir(dir.clone(), e))?;
        passthrough
            .import()
            .map_err(|e| Error::SharedDir(dir.clone(), e))?;

        // A session on disk means this process is standing in for one the
        // guest was already talking to, so the nodeids it holds have to mean
        // the same files again before it sends a single request.
        let (session, pending) = match config.state.as_ref().filter(|p| p.exists()) {
            Some(p) => {
                let s = Session::load(p).map_err(|e| Error::State(p.clone(), e))?;
                let pending = s.mappings();
                (Arc::new(s), pending)
            }
            None => (Arc::new(Session::new()), Vec::new()),
        };
        let fs = Arc::new(fsopts::Fs::new(passthrough, Arc::clone(&session)));
        restore_inodes(&fs, &session);

        let mut vconfig = VirtioFsConfig::default();
        let tag = config.tag.as_bytes();
        if tag.len() > TAG_LEN {
            return Err(Error::TagTooLong);
        }
        vconfig.tag[..tag.len()].copy_from_slice(tag);
        vconfig.num_request_queues = 1;

        Ok(FsBackend {
            config,
            vconfig,
            server: Arc::new(Server::new(Arc::clone(&fs))),
            mem: None,
            frontend: Arc::new(Mutex::new(None)),
            event_idx: false,
            session,
            fs,
            pending,
        })
    }

    /// The record of what the guest is holding.
    pub fn session(&self) -> Arc<Session> {
        Arc::clone(&self.session)
    }

    /// Serve every FUSE message the guest has queued.
    fn process_queue(&mut self, vring: &VringRwLock) -> io::Result<bool> {
        let Some(atomic) = self.mem.clone() else {
            return Ok(false);
        };
        let mut used = false;
        loop {
            let Some(chain) = vring
                .get_mut()
                .get_queue_mut()
                .pop_descriptor_chain(atomic.memory())
            else {
                break;
            };
            let head = chain.head_index();
            let guard = atomic.memory();
            let mem: &GuestMemoryMmap = &guard;

            let written = {
                let Ok(reader) = Reader::from_descriptor_chain(mem, chain.clone()) else {
                    log::error!("fs: unreadable request, skipping the chain");
                    vring
                        .get_mut()
                        .get_queue_mut()
                        .add_used(mem, head, 0)
                        .map_err(|e| io::Error::other(format!("returning a chain: {e}")))?;
                    used = true;
                    continue;
                };
                let writer = match VirtioFsWriter::new(mem, chain) {
                    Ok(w) => w,
                    Err(e) => {
                        log::error!("fs: preparing a reply: {e}");
                        return Err(io::Error::other("bad chain"));
                    }
                };
                let mut cache = CacheHandler {
                    frontend: Arc::clone(&self.frontend),
                    shm_id: self.config.shm_id,
                };
                match self.server.handle_message(
                    reader,
                    Writer::VirtioFs(writer),
                    Some(&mut cache as &mut dyn FsCacheReqHandler),
                    None,
                ) {
                    Ok(n) => n as u32,
                    Err(e) => {
                        log::error!("fs: serving a request: {e}");
                        0
                    }
                }
            };

            vring
                .get_mut()
                .get_queue_mut()
                .add_used(mem, head, written)
                .map_err(|e| io::Error::other(format!("returning a chain: {e}")))?;
            used = true;
        }
        Ok(used)
    }
}

impl VhostUserBackendMut for FsBackend {
    type Bitmap = ();
    type Vring = VringRwLock;

    fn num_queues(&self) -> usize {
        NUM_QUEUES
    }

    fn max_queue_size(&self) -> usize {
        QUEUE_SIZE as usize
    }

    fn features(&self) -> u64 {
        (1 << VIRTIO_F_VERSION_1)
            | (1 << VIRTIO_F_NOTIFY_ON_EMPTY)
            | (1 << VIRTIO_RING_F_EVENT_IDX)
            | VhostUserVirtioFeatures::PROTOCOL_FEATURES.bits()
    }

    fn protocol_features(&self) -> VhostUserProtocolFeatures {
        VhostUserProtocolFeatures::MQ
            | VhostUserProtocolFeatures::CONFIG
            | VhostUserProtocolFeatures::REPLY_ACK
            | VhostUserProtocolFeatures::BACKEND_REQ
            | VhostUserProtocolFeatures::SHMEM
    }

    fn set_event_idx(&mut self, enabled: bool) {
        self.event_idx = enabled;
    }

    fn get_config(&self, offset: u32, size: u32) -> Vec<u8> {
        let bytes = self.vconfig.as_slice();
        let start = std::cmp::min(offset as usize, bytes.len());
        let end = std::cmp::min(start + size as usize, bytes.len());
        bytes[start..end].to_vec()
    }

    fn update_memory(&mut self, mem: GuestMemoryAtomic<GuestMemoryMmap>) -> io::Result<()> {
        self.mem = Some(mem);
        Ok(())
    }

    fn set_backend_req_fd(&mut self, backend: Backend) {
        // Without this there is no channel to ask for a mapping, so every
        // setupmapping fails and the guest falls back to reading.
        backend.set_shmem_flag(true);
        backend.set_reply_ack_flag(true);
        *self.frontend.lock().unwrap() = Some(backend);

        // The mappings a restored guest is already holding have to be made
        // again before it runs. They cannot be made from here: this is the
        // middle of the monitor's device setup, and asking it to service a
        // hundred and fifty requests before it has finished leaves it unable
        // to answer and it dies. So they go to a thread, which the monitor
        // serves once it is listening.
        if !self.pending.is_empty() {
            let pending = std::mem::take(&mut self.pending);
            let frontend = Arc::clone(&self.frontend);
            let fs = Arc::clone(&self.fs);
            let shm_id = self.config.shm_id;
            std::thread::spawn(move || {
                let mut handler = CacheHandler { frontend, shm_id };
                let ctx = fuse_backend_rs::api::filesystem::Context::default();
                let (mut ok, mut failed) = (0usize, 0usize);
                for m in pending {
                    match fs.remap(&ctx, &m, &mut handler) {
                        Ok(()) => ok += 1,
                        Err(e) => {
                            failed += 1;
                            log::warn!("could not make mapping at {:#x} again: {e}", m.moffset);
                        }
                    }
                }
                log::info!("restored {ok} mappings, {failed} lost");
            });
        }
    }

    fn handle_event(
        &mut self,
        device_event: u16,
        _evset: EventSet,
        vrings: &[VringRwLock],
        _thread_id: usize,
    ) -> io::Result<()> {
        let Some(vring) = vrings.get(device_event as usize) else {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        };
        loop {
            if self.event_idx {
                vring.disable_notification().ok();
            }
            if self.process_queue(vring)? {
                vring.signal_used_queue().ok();
            }
            if !self.event_idx || !vring.enable_notification().unwrap_or(false) {
                break;
            }
        }
        Ok(())
    }
}
