// SPDX-License-Identifier: Apache-2.0

//! The device: virtqueues in, rendering out.
//!
//! Blob resources are the one place this differs from a device living inside
//! a monitor. The window a guest maps them through belongs to the monitor, so
//! the resource's memory is sent there to be placed rather than mapped here.

use std::io;
use std::mem::size_of;
use std::os::fd::AsFd;


use log::{debug, warn};
use rutabaga_gfx::{
    ResourceCreate3D, ResourceCreateBlob, Rutabaga, RutabagaBuilder, RutabagaComponentType,
    RutabagaFence, RutabagaFenceHandler, RutabagaHandle, RutabagaIovec, Transfer3D,
    RUTABAGA_CAPSET_VENUS, RUTABAGA_CAPSET_VIRGL, RUTABAGA_CAPSET_VIRGL2,
};
use vhost::vhost_user::message::{
    VhostUserMMap, VhostUserMMapFlags, VhostUserProtocolFeatures, VhostUserVirtioFeatures,
};
use vhost::vhost_user::{Backend, VhostUserFrontendReqHandler};
use vhost_user_backend::{VhostUserBackendMut, VringRwLock, VringT};
use virtio_queue::QueueT;
use virtio_bindings::bindings::virtio_config::{VIRTIO_F_NOTIFY_ON_EMPTY, VIRTIO_F_VERSION_1};
use virtio_bindings::bindings::virtio_ring::VIRTIO_RING_F_EVENT_IDX;
use vm_memory::{
    ByteValued, Bytes, GuestAddress, GuestAddressSpace, GuestMemoryAtomic, GuestMemoryMmap,
};
use vmm_sys_util::epoll::EventSet;

use crate::protocol::*;
use crate::replay::{self, Blob, Context, Create3D, Kind, Resource, Session};
use vmm_sys_util::eventfd::EventFd;

/// How the device was asked to be configured.
#[derive(Clone, Debug)]
pub struct GpuConfig {
    pub virgl: bool,
    pub venus: bool,
    pub shm_id: u8,
    /// Where the set of live objects is kept, so a backend standing in for
    /// one the guest was already talking to knows which of the identifiers it
    /// holds stand for nothing.
    pub state: Option<std::path::PathBuf>,
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Descriptor chain carried no readable command")]
    NoCommand,
    #[error("Descriptor chain left nowhere to put the response")]
    NoResponseBuffer,
    #[error("Guest memory access failed")]
    GuestMemory(#[source] vm_memory::GuestMemoryError),
    #[error("Renderer unavailable: {0}")]
    Renderer(String),
    #[error("The monitor has not offered a shared memory window")]
    NoWindow,
    #[error("Mapping into the monitor's window failed")]
    Map(#[source] io::Error),
    #[error("No blob of that id was created here")]
    UnknownBlob,
}

fn capset_mask(config: &GpuConfig) -> u64 {
    let mut m = 0u64;
    if config.virgl {
        m |= (1 << RUTABAGA_CAPSET_VIRGL) | (1 << RUTABAGA_CAPSET_VIRGL2);
    }
    if config.venus {
        m |= 1 << RUTABAGA_CAPSET_VENUS;
    }
    m
}

fn capset_count(config: &GpuConfig) -> u32 {
    let mut n = 0;
    if config.virgl {
        n += 2;
    }
    if config.venus {
        n += 1;
    }
    n
}

/// Build the renderer.
///
/// EGL and the surfaceless platform are selected explicitly. A host with no
/// `/dev/dri` has no GBM device, and virglrenderer dereferences a null
/// context if it probes for one. Venus additionally needs virglrenderer's
/// render server, without which its capset reads back as zeroes.
fn build_rutabaga(config: &GpuConfig, retired: Retired) -> Result<Rutabaga, Error> {
    let handler = RutabagaFenceHandler::new(move |f: RutabagaFence| retired.retire(&f));
    RutabagaBuilder::new(capset_mask(&config), handler)
        .set_use_egl(true)
        .set_use_surfaceless(true)
        .set_use_vulkan(config.venus)
        .set_use_render_server(config.venus)
        .set_default_component(RutabagaComponentType::VirglRenderer)
        .build()
        .map_err(|e| Error::Renderer(format!("{e}")))
}

/// The newest fence the renderer has retired on each timeline.
///
/// A guest waits on a fence by id, and ids only grow along a timeline -- the
/// device's own, or one ring of one context -- so the newest retired id is
/// enough to answer whether any earlier one has.
#[derive(Clone, Default)]
struct Retired(std::sync::Arc<std::sync::Mutex<std::collections::HashMap<(u32, u8), u64>>>);

impl Retired {
    fn timeline(f: &RutabagaFence) -> (u32, u8) {
        if f.flags & VIRTIO_GPU_FLAG_INFO_RING_IDX != 0 {
            (f.ctx_id, f.ring_idx)
        } else {
            (0, 0)
        }
    }

    fn retire(&self, f: &RutabagaFence) {
        let mut m = self.0.lock().unwrap();
        let e = m.entry(Self::timeline(f)).or_insert(0);
        *e = (*e).max(f.fence_id);
    }

    fn has_retired(&self, f: &RutabagaFence) -> bool {
        self.0
            .lock()
            .unwrap()
            .get(&Self::timeline(f))
            .is_some_and(|&id| id >= f.fence_id)
    }
}

/// How long a fenced command may take before its response is sent anyway.
///
/// Holding a response forever would hang the guest outright; answering late
/// only risks it reading before the renderer has finished writing, which a
/// renderer that takes this long has other problems than.
const FENCE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// One command: the bytes the guest wrote, and where its answer goes.
/// The objects a guest has been given and has not destroyed.
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct Live {
    pub resources: std::collections::BTreeSet<u32>,
    pub contexts: std::collections::BTreeSet<u32>,
}


struct Request {
    body: Vec<u8>,
    resp_addrs: Vec<(GuestAddress, u32)>,
}

thread_local! {
    /// The renderer is neither Send nor Sync, so it lives on the worker
    /// thread that drives it rather than in the backend the daemon shares
    /// between threads. It is built on first use, which is also the first
    /// point at which the monitor has told us anything.
    static RENDERER: std::cell::RefCell<Option<Rutabaga>> =
        const { std::cell::RefCell::new(None) };
}

pub struct GpuBackend {
    config: GpuConfig,
    mem: Option<GuestMemoryAtomic<GuestMemoryMmap>>,
    /// The channel the monitor gave us for asking it to place memory in the
    /// guest's window.
    frontend: Option<Backend>,
    /// Where the next blob is placed inside that window.
    next_offset: u64,
    /// What each mapped resource was given, so it can be taken back.
    placed: Vec<(u32, u64, u64)>,
    /// How large each blob was created, which an exported handle does not
    /// carry and the monitor needs in order to place it.
    blob_sizes: Vec<(u32, u64)>,
    event_idx: bool,
    acked_features: u64,
    /// What the guest has built, so it can be built again; see `replay`.
    rec: Session,
    /// Objects a previous process made that could not be built again. The
    /// guest still refers to them and nothing behind them exists, so they
    /// are answered rather than guessed at: see `lost_resource`.
    lost: Live,
    /// Fences the renderer has finished with.
    retired: Retired,
    /// Signalled to have the worker thread, which owns the renderer, write
    /// the session out.
    save_evt: EventFd,
    /// Signalled to have the worker thread rebuild a restored session.
    restore_evt: EventFd,
    /// A session read at startup, waiting for guest memory and the window
    /// channel before it can be rebuilt.
    pending: Option<(Session, Vec<u8>)>,
    restore_kicked: bool,
}

/// Device events beyond the queues and the exit event, which vhost-user
/// reserves.
pub const SAVE_EVENT: u16 = NUM_QUEUES as u16 + 1;
pub const RESTORE_EVENT: u16 = NUM_QUEUES as u16 + 2;

#[repr(C)]
struct VirglBox {
    x: u32,
    y: u32,
    z: u32,
    w: u32,
    h: u32,
    d: u32,
}

// rutabaga's own transfer_write only writes from a resource's attached
// backing, and refuses a buffer. The renderer beneath it takes one, so the
// contents of a restored resource are written through it directly. The
// library is the one rutabaga already links.
#[link(name = "virglrenderer")]
extern "C" {
    fn virgl_renderer_transfer_write_iov(
        handle: u32,
        ctx_id: u32,
        level: i32,
        stride: u32,
        layer_stride: u32,
        box_: *mut VirglBox,
        offset: u64,
        iovec: *mut libc::iovec,
        iovec_cnt: libc::c_uint,
    ) -> libc::c_int;
}

impl GpuBackend {
    pub fn new(config: GpuConfig) -> Result<Self, Error> {
        log::info!("offering {} capset(s)", capset_count(&config));
        Ok(GpuBackend {
            config,
            mem: None,
            frontend: None,
            next_offset: 0,
            placed: Vec::new(),
            blob_sizes: Vec::new(),
            event_idx: false,
            acked_features: 0,
            rec: Session::default(),
            lost: Live::default(),
            retired: Retired::default(),
            save_evt: EventFd::new(libc::EFD_NONBLOCK).map_err(|e| Error::Renderer(format!("{e}")))?,
            restore_evt: EventFd::new(libc::EFD_NONBLOCK).map_err(|e| Error::Renderer(format!("{e}")))?,
            pending: None,
            restore_kicked: false,
        })
    }

    /// Ask the monitor to place a blob in the guest's window.
    ///
    /// The monitor owns the window, so the resource's memory is exported as a
    /// descriptor and sent there rather than mapped in this process.
    fn map_blob(
        &mut self,
        rutabaga: &mut Rutabaga,
        resource_id: u32,
        guest_offset: u64,
    ) -> Result<u32, Error> {
        let size = self
            .blob_sizes
            .iter()
            .find(|(id, _)| *id == resource_id)
            .map(|(_, len)| *len)
            .ok_or(Error::UnknownBlob)?;

        let handle = rutabaga
            .export_blob(resource_id)
            .map_err(|e| Error::Renderer(format!("{e}")))?;
        let RutabagaHandle::MagmaGpuHandle(handle) = handle else {
            return Err(Error::Renderer("blob exported an unusable handle".into()));
        };

        let req = VhostUserMMap {
            shmid: self.config.shm_id,
            padding: [0; 7],
            fd_offset: 0,
            shm_offset: guest_offset,
            len: size,
            flags: VhostUserMMapFlags::WRITABLE.bits(),
        };
        self.frontend
            .as_ref()
            .ok_or(Error::NoWindow)?
            .shmem_map(&req, &handle.os_handle.as_fd())
            .map_err(Error::Map)?;

        self.placed
            .retain(|(id, ..)| *id != resource_id);
        self.placed.push((resource_id, guest_offset, size));
        rutabaga
            .map_info(resource_id)
            .map_err(|e| Error::Renderer(format!("{e}")))
    }

    /// Take a blob back out of the window.
    fn unmap_blob(&mut self, rutabaga: &mut Rutabaga, resource_id: u32) -> Result<(), Error> {
        let frontend = self.frontend.as_ref().ok_or(Error::NoWindow)?;
        let Some(pos) = self.placed.iter().position(|(id, ..)| *id == resource_id) else {
            return Ok(());
        };
        let (_, offset, len) = self.placed.remove(pos);
        let req = VhostUserMMap {
            shmid: self.config.shm_id,
            padding: [0; 7],
            fd_offset: 0,
            shm_offset: offset,
            len,
            flags: 0,
        };
        frontend.shmem_unmap(&req).map_err(Error::Map)?;
        rutabaga
            .unmap(resource_id)
            .map_err(|e| Error::Renderer(format!("{e}")))
    }

    fn iovec(
        mem: &GuestMemoryMmap,
        addr: u64,
        len: u32,
    ) -> Option<RutabagaIovec> {
        use vm_memory::GuestMemory;
        let start = vm_memory::GuestAddress(addr);
        let len = len as usize;
        if len == 0 {
            return None;
        }
        // get_slice fails unless the whole span sits in one region.
        let slice = mem.get_slice(start, len).ok()?;
        Some(RutabagaIovec {
            base: slice.ptr_guard_mut().as_ptr() as *mut std::ffi::c_void,
            len,
        })
    }

    fn header_of(type_: u32, request: &CtrlHeader) -> CtrlHeader {
        CtrlHeader {
            type_,
            // The fence flags are added by the queue loop, and only once the
            // fence has retired; see process_queue.
            flags: 0,
            fence_id: request.fence_id,
            ctx_id: request.ctx_id,
            ring_idx: request.ring_idx,
            padding: [0; 3],
        }
    }

    fn ok_or_err(result: Result<(), impl std::fmt::Display>, what: &str) -> u32 {
        match result {
            Ok(()) => VIRTIO_GPU_RESP_OK_NODATA,
            Err(e) => {
                warn!("virtio-gpu {what}: {e}");
                VIRTIO_GPU_RESP_ERR_UNSPEC
            }
        }
    }

    /// Takes a session read at startup, to be rebuilt once the monitor has
    /// connected; see `kick_restore`.
    pub fn set_pending(&mut self, session: Session, contents: Vec<u8>) {
        self.pending = Some((session, contents));
    }

    /// The eventfds the worker thread is told to save and restore through.
    pub fn save_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
        self.save_evt.as_raw_fd()
    }

    pub fn restore_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
        self.restore_evt.as_raw_fd()
    }

    /// Asks for the session to be written out; see `save_session`.
    pub fn request_save(&self) -> io::Result<()> {
        self.save_evt.write(1)
    }

    fn record_3d(&mut self, id: u32, c: &ResourceCreate3D) {
        self.rec.resources.insert(
            id,
            Resource {
                kind: Kind::ThreeD(Create3D {
                    target: c.target,
                    format: c.format,
                    bind: c.bind,
                    width: c.width,
                    height: c.height,
                    depth: c.depth,
                    array_size: c.array_size,
                    last_level: c.last_level,
                    nr_samples: c.nr_samples,
                    flags: c.flags,
                }),
                backing: None,
                mapped_at: None,
                contents: Vec::new(),
            },
        );
    }

    fn forget_resource(&mut self, id: u32) {
        self.rec.resources.remove(&id);
        for c in self.rec.contexts.values_mut() {
            c.attached.remove(&id);
        }
        self.rec.scanouts.retain(|_, r| *r != id);
    }

    /// Starts rebuilding a restored session once everything it needs is
    /// here: guest memory, to find the backing it names, and the channel to
    /// the monitor, to place its blobs. The rebuild runs on the worker thread,
    /// which owns the renderer, so this only wakes it.
    fn kick_restore(&mut self) {
        if self.pending.is_some() && !self.restore_kicked && self.mem.is_some() && self.frontend.is_some() {
            self.restore_kicked = true;
            if let Err(e) = self.restore_evt.write(1) {
                warn!("waking the worker to restore: {e}");
            }
        }
    }

    /// Writes the session out: what was recorded, and the contents of every
    /// resource read back from the renderer.
    ///
    /// Runs on the worker thread with the guest paused, so nothing is being
    /// rendered and nothing changes while it reads.
    fn save_session(&mut self, rutabaga: &mut Rutabaga) -> Result<String, String> {
        let path = self.config.state.clone().ok_or("started without a state file")?;
        let mut session = self.rec.clone();
        let mut data: Vec<u8> = Vec::new();
        let (mut carried, mut skipped) = (0usize, 0usize);

        for (&id, r) in session.resources.iter_mut() {
            r.contents.clear();
            let chunks = match &r.kind {
                Kind::ThreeD(c) => replay::chunks_for(c),
                Kind::Blob(b) => Ok(vec![replay::Chunk {
                    level: u32::MAX, x: 0, y: 0, z: 0, w: 0, h: 0, d: 0,
                    stride: 0, layer_stride: 0, offset: 0, len: b.size,
                }]),
            };
            let chunks = match chunks {
                Ok(c) => c,
                Err(why) => {
                    warn!("resource {id}: contents not carried: {why}");
                    skipped += 1;
                    continue;
                }
            };
            let mut saved = Vec::new();
            let mut ok = true;
            for mut ch in chunks {
                let mut buf = vec![0u8; ch.len as usize];
                let read = if ch.level == u32::MAX {
                    self.blob_bytes(rutabaga, id, &mut buf, false)
                } else {
                    let t = Transfer3D {
                        x: ch.x, y: ch.y, z: ch.z, w: ch.w, h: ch.h, d: ch.d,
                        level: ch.level, stride: ch.stride, layer_stride: ch.layer_stride, offset: 0,
                    };
                    rutabaga
                        .transfer_read(0, id, t, Some(std::io::IoSliceMut::new(&mut buf)))
                        .map_err(|e| format!("{e}"))
                };
                match read {
                    Ok(()) => {
                        ch.offset = data.len() as u64;
                        data.extend_from_slice(&buf);
                        saved.push(ch);
                    }
                    Err(e) => {
                        warn!("resource {id}: reading its contents back: {e}");
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                r.contents = saved;
                carried += 1;
            } else {
                skipped += 1;
            }
        }

        let bin = path.with_extension("bin");
        let write = |p: &std::path::Path, bytes: &[u8]| -> Result<(), String> {
            let tmp = p.with_extension("tmp");
            std::fs::write(&tmp, bytes).map_err(|e| format!("{e}"))?;
            std::fs::rename(&tmp, p).map_err(|e| format!("{e}"))
        };
        // The contents first and the session last: the session appearing is
        // the sign the whole write is done.
        write(&bin, &data)?;
        let json = serde_json::to_vec(&session).map_err(|e| format!("{e}"))?;
        write(&path, &json)?;
        Ok(format!(
            "{} resources ({carried} with contents, {skipped} without), {} contexts, {} bytes of contents",
            session.resources.len(),
            session.contexts.len(),
            data.len()
        ))
    }

    /// Reads or writes a blob's bytes through a mapping of its exported
    /// descriptor.
    fn blob_bytes(&mut self, rutabaga: &mut Rutabaga, id: u32, buf: &mut [u8], write: bool) -> Result<(), String> {
        use std::os::fd::AsRawFd;
        let handle = rutabaga.export_blob(id).map_err(|e| format!("{e}"))?;
        let RutabagaHandle::MagmaGpuHandle(handle) = handle else {
            return Err("blob exported an unusable handle".into());
        };
        let prot = if write { libc::PROT_READ | libc::PROT_WRITE } else { libc::PROT_READ };
        // SAFETY: a fresh mapping of a descriptor we hold, sized to the blob,
        // unmapped before returning.
        unsafe {
            let p = libc::mmap(
                std::ptr::null_mut(),
                buf.len(),
                prot,
                libc::MAP_SHARED,
                handle.os_handle.as_fd().as_raw_fd(),
                0,
            );
            if p == libc::MAP_FAILED {
                return Err(format!("mapping the blob: {}", io::Error::last_os_error()));
            }
            if write {
                std::ptr::copy_nonoverlapping(buf.as_ptr(), p as *mut u8, buf.len());
            } else {
                std::ptr::copy_nonoverlapping(p as *const u8, buf.as_mut_ptr(), buf.len());
            }
            libc::munmap(p, buf.len());
        }
        Ok(())
    }

    fn submit_words(rutabaga: &mut Rutabaga, ctx_id: u32, words: &[u32]) -> Result<(), String> {
        if words.is_empty() {
            return Ok(());
        }
        let mut bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        rutabaga.submit_command(ctx_id, &mut bytes, &[]).map_err(|e| format!("{e}"))
    }

    /// Builds a restored session again in this process's renderer.
    ///
    /// The order is the order of dependence: contexts, then the pipe
    /// resources blobs are made from, then resources, their backing and
    /// contents, the blobs' places in the window, what each context has
    /// attached, and last each context's objects and bindings, which name all
    /// of the above.
    fn restore_session(&mut self, rutabaga: &mut Rutabaga) -> Result<String, String> {
        let (session, data) = self.pending.take().ok_or("nothing to restore")?;
        let atomic = self.mem.clone().ok_or("no guest memory")?;
        let guard = atomic.memory();
        let mem: &GuestMemoryMmap = &guard;
        let mut built = std::collections::BTreeSet::new();
        let mut lost = Live::default();

        for (&id, c) in &session.contexts {
            if let Err(e) = rutabaga.create_context(id, c.init, c.name.as_deref()) {
                warn!("context {id}: {e}");
                lost.contexts.insert(id);
            } else if c.virgl.is_none() {
                // Its protocol is not understood here, so it exists again
                // but empty, and the guest is told so.
                lost.contexts.insert(id);
            }
        }

        for (&ctx_id, c) in &session.contexts {
            let Some(v) = &c.virgl else { continue };
            let ids = session
                .resources
                .values()
                .filter_map(|r| match &r.kind {
                    Kind::Blob(b) if b.ctx_id == ctx_id && b.blob_id != 0 => Some(b.blob_id as u32),
                    _ => None,
                })
                .collect();
            if let Err(e) = Self::submit_words(rutabaga, ctx_id, &v.pipe_resource_commands(&ids)) {
                warn!("context {ctx_id}: recreating pipe resources: {e}");
            }
        }

        for (&id, r) in &session.resources {
            let made = match &r.kind {
                Kind::ThreeD(c) => rutabaga
                    .resource_create_3d(
                        id,
                        ResourceCreate3D {
                            target: c.target,
                            format: c.format,
                            bind: c.bind,
                            width: c.width,
                            height: c.height,
                            depth: c.depth,
                            array_size: c.array_size,
                            last_level: c.last_level,
                            nr_samples: c.nr_samples,
                            flags: c.flags,
                        },
                    )
                    .map_err(|e| format!("{e}")),
                Kind::Blob(b) => {
                    let iovecs = if b.entries.is_empty() {
                        None
                    } else {
                        let v: Option<Vec<_>> = b.entries.iter().map(|&(a, l)| Self::iovec(mem, a, l)).collect();
                        match v {
                            Some(v) => Some(v),
                            None => {
                                warn!("blob {id}: backing outside guest memory");
                                lost.resources.insert(id);
                                continue;
                            }
                        }
                    };
                    let create = ResourceCreateBlob {
                        blob_mem: b.blob_mem,
                        blob_flags: b.blob_flags,
                        blob_id: b.blob_id,
                        size: b.size,
                    };
                    let r = rutabaga.resource_create_blob(b.ctx_id, id, create, iovecs, None).map_err(|e| format!("{e}"));
                    if r.is_ok() {
                        self.blob_sizes.retain(|(i, _)| *i != id);
                        self.blob_sizes.push((id, b.size));
                    }
                    r
                }
            };
            match made {
                Ok(()) => {
                    built.insert(id);
                }
                Err(e) => {
                    warn!("resource {id}: {e}");
                    lost.resources.insert(id);
                }
            }
        }

        let mut contents_ok = 0usize;
        for (&id, r) in &session.resources {
            if !built.contains(&id) {
                continue;
            }
            if let Some(backing) = &r.backing {
                let v: Option<Vec<_>> = backing.iter().map(|&(a, l)| Self::iovec(mem, a, l)).collect();
                match v.map(|v| rutabaga.attach_backing(id, v)) {
                    Some(Ok(())) => {}
                    Some(Err(e)) => warn!("resource {id}: reattaching backing: {e}"),
                    None => warn!("resource {id}: backing outside guest memory"),
                }
            }
            let mut ok = !r.contents.is_empty();
            for ch in &r.contents {
                let end = (ch.offset + ch.len) as usize;
                let Some(bytes) = data.get(ch.offset as usize..end) else {
                    ok = false;
                    break;
                };
                let mut buf = bytes.to_vec();
                if ch.level == u32::MAX {
                    if let Err(e) = self.blob_bytes(rutabaga, id, &mut buf, true) {
                        warn!("blob {id}: writing its contents back: {e}");
                        ok = false;
                    }
                    continue;
                }
                let mut b = VirglBox { x: ch.x, y: ch.y, z: ch.z, w: ch.w, h: ch.h, d: ch.d };
                let mut iov = libc::iovec { iov_base: buf.as_mut_ptr() as *mut _, iov_len: buf.len() };
                // SAFETY: the box and iovec live for the call, and the
                // renderer only reads from the iovec.
                let ret = unsafe {
                    virgl_renderer_transfer_write_iov(id, 0, ch.level as i32, ch.stride, ch.layer_stride, &mut b, 0, &mut iov, 1)
                };
                if ret != 0 {
                    warn!("resource {id}: writing level {} back failed ({ret})", ch.level);
                    ok = false;
                }
            }
            if ok {
                contents_ok += 1;
            }
        }

        for (&id, r) in &session.resources {
            if let (true, Some(offset)) = (built.contains(&id), r.mapped_at) {
                if let Err(e) = self.map_blob(rutabaga, id, offset) {
                    warn!("blob {id}: placing it in the window again: {e}");
                }
            }
        }

        for (&ctx_id, c) in &session.contexts {
            for &res in &c.attached {
                if built.contains(&res) {
                    if let Err(e) = rutabaga.context_attach_resource(ctx_id, res) {
                        warn!("context {ctx_id}: attaching resource {res}: {e}");
                    }
                }
            }
        }

        let mut replayed = 0usize;
        for (&ctx_id, c) in &session.contexts {
            let Some(v) = &c.virgl else { continue };
            if lost.contexts.contains(&ctx_id) {
                continue;
            }
            match Self::submit_words(rutabaga, ctx_id, &v.replay(&built)) {
                Ok(()) => replayed += 1,
                Err(e) => {
                    warn!("context {ctx_id}: replaying its state: {e}");
                    lost.contexts.insert(ctx_id);
                }
            }
        }

        for (&scanout, &res) in &session.scanouts {
            if built.contains(&res) {
                let _ = rutabaga.set_scanout(scanout, res, None);
            }
        }

        let summary = format!(
            "{} of {} resources rebuilt ({contents_ok} with contents), {replayed} of {} contexts replayed, {} resources and {} contexts lost",
            built.len(),
            session.resources.len(),
            session.contexts.len(),
            lost.resources.len(),
            lost.contexts.len()
        );
        let mut rec = session;
        for id in &lost.resources {
            rec.resources.remove(id);
        }
        for id in &lost.contexts {
            if let Some(c) = rec.contexts.get_mut(id) {
                c.virgl = None;
            }
        }
        for r in rec.resources.values_mut() {
            r.contents.clear();
        }
        self.rec = rec;
        self.lost = lost;
        Ok(summary)
    }

    /// Says a restored session is whole again; whatever resumes the guest
    /// waits for this.
    fn mark_ready(&self, summary: &str) {
        if let Some(p) = self.config.state.as_ref().map(|p| p.with_extension("ready")) {
            if let Err(e) = std::fs::write(&p, format!("{summary}\n")) {
                warn!("marking the restore complete at {}: {e}", p.display());
            }
        }
    }

    /// Waits for the renderer to retire a fence.
    fn wait_fence(&self, rutabaga: &mut Rutabaga, fence: &RutabagaFence) {
        let start = std::time::Instant::now();
        loop {
            rutabaga.event_poll();
            if self.retired.has_retired(fence) {
                return;
            }
            if start.elapsed() > FENCE_TIMEOUT {
                warn!(
                    "fence {} on ctx {} ring {} did not retire in {:?}; answering anyway",
                    fence.fence_id, fence.ctx_id, fence.ring_idx, FENCE_TIMEOUT
                );
                return;
            }
            std::thread::sleep(std::time::Duration::from_micros(50));
        }
    }

    /// Answers for an object that did not survive a restore.
    ///
    /// The point is that the guest is told precisely what is wrong. Left to
    /// itself the renderer would report that some resource id is unknown,
    /// which is indistinguishable from the guest having invented one, and a
    /// caller cannot tell a bug from a restore. Worse, an id could be handed
    /// to a resource created later and the guest would read another object's
    /// memory believing it was its own.
    fn lost_resource(&self, hdr: &CtrlHeader, id: u32) -> Option<Vec<u8>> {
        if !self.lost.resources.contains(&id) {
            return None;
        }
        log::debug!("resource {id} did not survive the restore");
        Some(Self::err(hdr, VIRTIO_GPU_RESP_ERR_INVALID_RESOURCE_ID))
    }

    fn lost_context(&self, hdr: &CtrlHeader) -> Option<Vec<u8>> {
        if hdr.ctx_id == 0 || !self.lost.contexts.contains(&hdr.ctx_id) {
            return None;
        }
        log::debug!("context {} did not survive the restore", hdr.ctx_id);
        Some(Self::err(hdr, VIRTIO_GPU_RESP_ERR_INVALID_CONTEXT_ID))
    }

    /// The resource a command is about, where it is the first field of the
    /// payload. Every command that names one puts it there.
    fn resource_of(type_: u32, payload: &[u8]) -> Option<u32> {
        const NAMES_A_RESOURCE: &[u32] = &[
            VIRTIO_GPU_CMD_RESOURCE_UNREF,
            VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING,
            VIRTIO_GPU_CMD_RESOURCE_DETACH_BACKING,
            VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D,
            VIRTIO_GPU_CMD_RESOURCE_MAP_BLOB,
            VIRTIO_GPU_CMD_RESOURCE_UNMAP_BLOB,
        ];
        if !NAMES_A_RESOURCE.contains(&type_) {
            return None;
        }
        payload
            .get(..4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn dispatch(&mut self, rutabaga: &mut Rutabaga, body: &[u8], mem: &GuestMemoryMmap) -> Vec<u8> {
        let hdr = match CtrlHeader::from_slice(&body[..size_of::<CtrlHeader>()]) {
            Some(h) => *h,
            None => return Vec::new(),
        };
        let payload = &body[size_of::<CtrlHeader>()..];

        // An identifier from before a restore stands for nothing, and is
        // refused here rather than deeper down where it would be
        // indistinguishable from the guest having invented one.
        if !self.lost.resources.is_empty() || !self.lost.contexts.is_empty() {
            if let Some(resp) = self.lost_context(&hdr) {
                return resp;
            }
            if let Some(id) = Self::resource_of(hdr.type_, payload) {
                if let Some(resp) = self.lost_resource(&hdr, id) {
                    return resp;
                }
            }
        }

        match hdr.type_ {
            VIRTIO_GPU_CMD_GET_DISPLAY_INFO => {
                let mut resp = RespDisplayInfo {
                    hdr: Self::header_of(VIRTIO_GPU_RESP_OK_DISPLAY_INFO, &hdr),
                    ..Default::default()
                };
                // One scanout, which is what a headless environment needs;
                // the rest stay disabled.
                resp.pmodes[0] = DisplayOne {
                    r: Rect {
                        x: 0,
                        y: 0,
                        width: DEFAULT_WIDTH,
                        height: DEFAULT_HEIGHT,
                    },
                    enabled: 1,
                    flags: 0,
                };
                resp.as_slice().to_vec()
            }

            VIRTIO_GPU_CMD_GET_CAPSET_INFO => {
                let req = match GetCapsetInfo::from_slice(
                    payload.get(..size_of::<GetCapsetInfo>()).unwrap_or(&[]),
                ) {
                    Some(r) => *r,
                    None => {
                        return Self::header_of(VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER, &hdr)
                            .as_slice()
                            .to_vec()
                    }
                };
                match rutabaga.get_capset_info(req.capset_index) {
                    Ok((capset_id, capset_max_version, capset_max_size)) => RespCapsetInfo {
                        hdr: Self::header_of(VIRTIO_GPU_RESP_OK_CAPSET_INFO, &hdr),
                        capset_id,
                        capset_max_version,
                        capset_max_size,
                        padding: 0,
                    }
                    .as_slice()
                    .to_vec(),
                    Err(e) => {
                        warn!("get_capset_info({}) failed: {e}", req.capset_index);
                        Self::header_of(VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER, &hdr)
                            .as_slice()
                            .to_vec()
                    }
                }
            }

            VIRTIO_GPU_CMD_GET_CAPSET => {
                let req = match GetCapset::from_slice(
                    payload.get(..size_of::<GetCapset>()).unwrap_or(&[]),
                ) {
                    Some(r) => *r,
                    None => {
                        return Self::header_of(VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER, &hdr)
                            .as_slice()
                            .to_vec()
                    }
                };
                match rutabaga
                    .get_capset(req.capset_id, req.capset_version)
                {
                    Ok(caps) => {
                        let mut out =
                            Self::header_of(VIRTIO_GPU_RESP_OK_CAPSET, &hdr).as_slice().to_vec();
                        out.extend_from_slice(&caps);
                        out
                    }
                    Err(e) => {
                        warn!("get_capset({}) failed: {e}", req.capset_id);
                        Self::header_of(VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER, &hdr)
                            .as_slice()
                            .to_vec()
                    }
                }
            }

            VIRTIO_GPU_CMD_RESOURCE_CREATE_2D => {
                let Some(req) = ResourceCreate2D::from_slice(
                    payload.get(..size_of::<ResourceCreate2D>()).unwrap_or(&[]),
                ) else {
                    return Self::err(&hdr, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER);
                };
                // A 2D resource is a 3D one with a single level and no depth.
                let create = ResourceCreate3D {
                    target: 2,
                    format: req.format,
                    bind: (1 << 1) | (1 << 2),
                    width: req.width,
                    height: req.height,
                    depth: 1,
                    array_size: 1,
                    last_level: 0,
                    nr_samples: 0,
                    flags: 0,
                };
                self.lost.resources.remove(&req.resource_id);
                let code = Self::ok_or_err(
                    rutabaga.resource_create_3d(req.resource_id, create),
                    "resource_create_2d",
                );
                if code == VIRTIO_GPU_RESP_OK_NODATA {
                    self.record_3d(req.resource_id, &create);
                }
                Self::err(&hdr, code)
            }

            VIRTIO_GPU_CMD_RESOURCE_CREATE_3D => {
                let Some(req) = ResourceCreate3DReq::from_slice(
                    payload.get(..size_of::<ResourceCreate3DReq>()).unwrap_or(&[]),
                ) else {
                    return Self::err(&hdr, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER);
                };
                let create = ResourceCreate3D {
                    target: req.target,
                    format: req.format,
                    bind: req.bind,
                    width: req.width,
                    height: req.height,
                    depth: req.depth,
                    array_size: req.array_size,
                    last_level: req.last_level,
                    nr_samples: req.nr_samples,
                    flags: req.flags,
                };
                self.lost.resources.remove(&req.resource_id);
                let code = Self::ok_or_err(
                    rutabaga.resource_create_3d(req.resource_id, create),
                    "resource_create_3d",
                );
                if code == VIRTIO_GPU_RESP_OK_NODATA {
                    self.record_3d(req.resource_id, &create);
                }
                Self::err(&hdr, code)
            }

            VIRTIO_GPU_CMD_RESOURCE_UNREF => {
                let Some(req) = ResourceUnref::from_slice(
                    payload.get(..size_of::<ResourceUnref>()).unwrap_or(&[]),
                ) else {
                    return Self::err(&hdr, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER);
                };
                self.forget_resource(req.resource_id);
                let code = Self::ok_or_err(
                    rutabaga.unref_resource(req.resource_id),
                    "resource_unref",
                );
                Self::err(&hdr, code)
            }

            VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING => {
                let Some(req) = AttachBacking::from_slice(
                    payload.get(..size_of::<AttachBacking>()).unwrap_or(&[]),
                ) else {
                    return Self::err(&hdr, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER);
                };
                let entries = &payload[size_of::<AttachBacking>()..];
                let want = req.nr_entries as usize * size_of::<MemEntry>();
                if entries.len() < want {
                    return Self::err(&hdr, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER);
                }
                let mut vecs = Vec::with_capacity(req.nr_entries as usize);
                let mut phys = Vec::with_capacity(req.nr_entries as usize);
                for i in 0..req.nr_entries as usize {
                    let off = i * size_of::<MemEntry>();
                    let Some(e) =
                        MemEntry::from_slice(&entries[off..off + size_of::<MemEntry>()])
                    else {
                        return Self::err(&hdr, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER);
                    };
                    match Self::iovec(mem, e.addr, e.length) {
                        Some(v) => vecs.push(v),
                        None => {
                            warn!("attach_backing: entry outside guest memory");
                            return Self::err(&hdr, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER);
                        }
                    }
                    phys.push((e.addr, e.length));
                }
                let code = Self::ok_or_err(
                    rutabaga.attach_backing(req.resource_id, vecs),
                    "attach_backing",
                );
                if code == VIRTIO_GPU_RESP_OK_NODATA {
                    if let Some(r) = self.rec.resources.get_mut(&req.resource_id) {
                        r.backing = Some(phys);
                    }
                }
                Self::err(&hdr, code)
            }

            VIRTIO_GPU_CMD_RESOURCE_DETACH_BACKING => {
                let Some(req) = ResourceUnref::from_slice(
                    payload.get(..size_of::<ResourceUnref>()).unwrap_or(&[]),
                ) else {
                    return Self::err(&hdr, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER);
                };
                let code = Self::ok_or_err(
                    rutabaga.detach_backing(req.resource_id),
                    "detach_backing",
                );
                if let Some(r) = self.rec.resources.get_mut(&req.resource_id) {
                    r.backing = None;
                }
                Self::err(&hdr, code)
            }

            VIRTIO_GPU_CMD_SET_SCANOUT => {
                let Some(req) = SetScanout::from_slice(
                    payload.get(..size_of::<SetScanout>()).unwrap_or(&[]),
                ) else {
                    return Self::err(&hdr, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER);
                };
                // Nothing is displayed on a host with no output, so the
                // scanout is recorded and the guest told it succeeded.
                let code = Self::ok_or_err(
                    rutabaga.set_scanout(req.scanout_id, req.resource_id, None),
                    "set_scanout",
                );
                if code == VIRTIO_GPU_RESP_OK_NODATA {
                    if req.resource_id == 0 {
                        self.rec.scanouts.remove(&req.scanout_id);
                    } else {
                        self.rec.scanouts.insert(req.scanout_id, req.resource_id);
                    }
                }
                Self::err(&hdr, code)
            }

            VIRTIO_GPU_CMD_RESOURCE_FLUSH => {
                let Some(req) = ResourceFlush::from_slice(
                    payload.get(..size_of::<ResourceFlush>()).unwrap_or(&[]),
                ) else {
                    return Self::err(&hdr, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER);
                };
                let code = Self::ok_or_err(
                    rutabaga.resource_flush(req.resource_id),
                    "resource_flush",
                );
                Self::err(&hdr, code)
            }

            VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D => {
                let Some(req) = TransferToHost2D::from_slice(
                    payload.get(..size_of::<TransferToHost2D>()).unwrap_or(&[]),
                ) else {
                    return Self::err(&hdr, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER);
                };
                let transfer = Transfer3D {
                    x: req.r.x,
                    y: req.r.y,
                    z: 0,
                    w: req.r.width,
                    h: req.r.height,
                    d: 1,
                    level: 0,
                    stride: 0,
                    layer_stride: 0,
                    offset: req.offset,
                };
                let code = Self::ok_or_err(
                    rutabaga
                        .transfer_write(0, req.resource_id, transfer, None),
                    "transfer_to_host_2d",
                );
                Self::err(&hdr, code)
            }

            VIRTIO_GPU_CMD_TRANSFER_TO_HOST_3D | VIRTIO_GPU_CMD_TRANSFER_FROM_HOST_3D => {
                let Some(req) = TransferHost3D::from_slice(
                    payload.get(..size_of::<TransferHost3D>()).unwrap_or(&[]),
                ) else {
                    return Self::err(&hdr, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER);
                };
                let transfer = Transfer3D {
                    x: req.box_[0],
                    y: req.box_[1],
                    z: req.box_[2],
                    w: req.box_[3],
                    h: req.box_[4],
                    d: req.box_[5],
                    level: req.level,
                    stride: req.stride,
                    layer_stride: req.layer_stride,
                    offset: req.offset,
                };
                let result = if hdr.type_ == VIRTIO_GPU_CMD_TRANSFER_TO_HOST_3D {
                    rutabaga
                        .transfer_write(hdr.ctx_id, req.resource_id, transfer, None)
                } else {
                    rutabaga
                        .transfer_read(hdr.ctx_id, req.resource_id, transfer, None)
                };
                let code = Self::ok_or_err(result, "transfer_3d");
                Self::err(&hdr, code)
            }

            VIRTIO_GPU_CMD_CTX_CREATE => {
                let Some(req) =
                    CtxCreate::from_slice(payload.get(..size_of::<CtxCreate>()).unwrap_or(&[]))
                else {
                    return Self::err(&hdr, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER);
                };
                let nlen = std::cmp::min(req.nlen as usize, req.debug_name.len());
                let name = std::str::from_utf8(&req.debug_name[..nlen]).ok();
                self.lost.contexts.remove(&hdr.ctx_id);
                let code = Self::ok_or_err(
                    rutabaga
                        .create_context(hdr.ctx_id, req.context_init, name),
                    "ctx_create",
                );
                if code == VIRTIO_GPU_RESP_OK_NODATA {
                    self.rec
                        .contexts
                        .insert(hdr.ctx_id, Context::new(req.context_init, name.map(String::from)));
                }
                Self::err(&hdr, code)
            }

            VIRTIO_GPU_CMD_CTX_DESTROY => {
                self.rec.contexts.remove(&hdr.ctx_id);
                let code =
                    Self::ok_or_err(rutabaga.destroy_context(hdr.ctx_id), "ctx_destroy");
                Self::err(&hdr, code)
            }

            VIRTIO_GPU_CMD_CTX_ATTACH_RESOURCE | VIRTIO_GPU_CMD_CTX_DETACH_RESOURCE => {
                let Some(req) = CtxResource::from_slice(
                    payload.get(..size_of::<CtxResource>()).unwrap_or(&[]),
                ) else {
                    return Self::err(&hdr, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER);
                };
                let result = if hdr.type_ == VIRTIO_GPU_CMD_CTX_ATTACH_RESOURCE {
                    rutabaga
                        .context_attach_resource(hdr.ctx_id, req.resource_id)
                } else {
                    rutabaga
                        .context_detach_resource(hdr.ctx_id, req.resource_id)
                };
                let code = Self::ok_or_err(result, "ctx_attach_or_detach");
                if code == VIRTIO_GPU_RESP_OK_NODATA {
                    if let Some(c) = self.rec.contexts.get_mut(&hdr.ctx_id) {
                        if hdr.type_ == VIRTIO_GPU_CMD_CTX_ATTACH_RESOURCE {
                            c.attached.insert(req.resource_id);
                        } else {
                            c.attached.remove(&req.resource_id);
                        }
                    }
                }
                Self::err(&hdr, code)
            }

            VIRTIO_GPU_CMD_SUBMIT_3D => {
                let Some(req) =
                    CmdSubmit::from_slice(payload.get(..size_of::<CmdSubmit>()).unwrap_or(&[]))
                else {
                    return Self::err(&hdr, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER);
                };
                let start = size_of::<CmdSubmit>();
                let end = start + req.size as usize;
                if payload.len() < end {
                    return Self::err(&hdr, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER);
                }
                let mut cmds = payload[start..end].to_vec();
                let code = Self::ok_or_err(
                    rutabaga.submit_command(hdr.ctx_id, &mut cmds, &[]),
                    "submit_3d",
                );
                if code == VIRTIO_GPU_RESP_OK_NODATA {
                    if let Some(v) = self.rec.contexts.get_mut(&hdr.ctx_id).and_then(|c| c.virgl.as_mut()) {
                        v.observe(&payload[start..end]);
                    }
                }
                Self::err(&hdr, code)
            }

            VIRTIO_GPU_CMD_RESOURCE_CREATE_BLOB => {
                let Some(req) = ResourceCreateBlobReq::from_slice(
                    payload.get(..size_of::<ResourceCreateBlobReq>()).unwrap_or(&[]),
                ) else {
                    return Self::err(&hdr, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER);
                };
                // A blob may be backed by guest pages, by host memory, or by
                // both. Only the guest-backed form carries an entry list.
                let entries = &payload[size_of::<ResourceCreateBlobReq>()..];
                let want = req.nr_entries as usize * size_of::<MemEntry>();
                let mut blob_phys = Vec::new();
                let iovecs = if req.nr_entries == 0 {
                    None
                } else if entries.len() < want {
                    return Self::err(&hdr, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER);
                } else {
                    let mut vecs = Vec::with_capacity(req.nr_entries as usize);
                    for i in 0..req.nr_entries as usize {
                        let off = i * size_of::<MemEntry>();
                        let Some(e) =
                            MemEntry::from_slice(&entries[off..off + size_of::<MemEntry>()])
                        else {
                            return Self::err(&hdr, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER);
                        };
                        match Self::iovec(mem, e.addr, e.length) {
                            Some(v) => vecs.push(v),
                            None => {
                                warn!("create_blob: entry outside guest memory");
                                return Self::err(&hdr, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER);
                            }
                        }
                        blob_phys.push((e.addr, e.length));
                    }
                    Some(vecs)
                };
                let create = ResourceCreateBlob {
                    blob_mem: req.blob_mem,
                    blob_flags: req.blob_flags,
                    blob_id: req.blob_id,
                    size: req.size,
                };
                let result = rutabaga.resource_create_blob(
                    hdr.ctx_id,
                    req.resource_id,
                    create,
                    iovecs,
                    None,
                );
                if result.is_ok() {
                    self.blob_sizes.retain(|(id, _)| *id != req.resource_id);
                    self.blob_sizes.push((req.resource_id, req.size));
                    self.lost.resources.remove(&req.resource_id);
                    self.rec.resources.insert(
                        req.resource_id,
                        Resource {
                            kind: Kind::Blob(Blob {
                                ctx_id: hdr.ctx_id,
                                blob_mem: req.blob_mem,
                                blob_flags: req.blob_flags,
                                blob_id: req.blob_id,
                                size: req.size,
                                entries: blob_phys,
                            }),
                            backing: None,
                            mapped_at: None,
                            contents: Vec::new(),
                        },
                    );
                }
                let code = Self::ok_or_err(result, "resource_create_blob");
                Self::err(&hdr, code)
            }

            VIRTIO_GPU_CMD_RESOURCE_MAP_BLOB => {
                let Some(req) = ResourceMapBlob::from_slice(
                    payload.get(..size_of::<ResourceMapBlob>()).unwrap_or(&[]),
                ) else {
                    return Self::err(&hdr, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER);
                };
                // The window belongs to the monitor, so the resource's memory
                // is sent there to be placed rather than mapped here.
                match self.map_blob(rutabaga, req.resource_id, req.offset) {
                    Ok(map_info) => {
                        if let Some(r) = self.rec.resources.get_mut(&req.resource_id) {
                            r.mapped_at = Some(req.offset);
                        }
                        RespMapInfo {
                            hdr: Self::header_of(VIRTIO_GPU_RESP_OK_MAP_INFO, &hdr),
                            map_info,
                            padding: 0,
                        }
                        .as_slice()
                        .to_vec()
                    }
                    Err(e) => {
                        warn!("virtio-gpu map_blob: {e}");
                        Self::err(&hdr, VIRTIO_GPU_RESP_ERR_UNSPEC)
                    }
                }
            }

            VIRTIO_GPU_CMD_RESOURCE_UNMAP_BLOB => {
                let Some(req) = ResourceUnref::from_slice(
                    payload.get(..size_of::<ResourceUnref>()).unwrap_or(&[]),
                ) else {
                    return Self::err(&hdr, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER);
                };
                let code = Self::ok_or_err(self.unmap_blob(rutabaga, req.resource_id), "unmap_blob");
                if let Some(r) = self.rec.resources.get_mut(&req.resource_id) {
                    r.mapped_at = None;
                }
                Self::err(&hdr, code)
            }

            // Nothing is displayed, so a cursor is accepted and dropped. The
            // guest still needs the queue drained and acknowledged.
            VIRTIO_GPU_CMD_UPDATE_CURSOR | VIRTIO_GPU_CMD_MOVE_CURSOR => {
                Self::err(&hdr, VIRTIO_GPU_RESP_OK_NODATA)
            }

            other => {
                debug!("virtio-gpu command {other:#06x} not handled");
                Self::err(&hdr, VIRTIO_GPU_RESP_ERR_UNSPEC)
            }
        }
    }

    /// A bare header carrying one response code.
    fn err(hdr: &CtrlHeader, code: u32) -> Vec<u8> {
        Self::header_of(code, hdr).as_slice().to_vec()
    }


    /// Split a chain into the command the guest wrote and the buffers it left
    /// for the reply.
    fn split(
        mem: &GuestMemoryMmap,
        chain: &mut virtio_queue::DescriptorChain<
            vm_memory::GuestMemoryLoadGuard<GuestMemoryMmap>,
        >,
    ) -> Result<Request, Error> {
        let mut body = Vec::new();
        let mut resp_addrs = Vec::new();
        for desc in chain.by_ref() {
            if desc.len() == 0 {
                continue;
            }
            if desc.is_write_only() {
                resp_addrs.push((desc.addr(), desc.len()));
            } else {
                let mut buf = vec![0u8; desc.len() as usize];
                mem.read_slice(&mut buf, desc.addr())
                    .map_err(Error::GuestMemory)?;
                body.extend_from_slice(&buf);
            }
        }
        if body.len() < size_of::<CtrlHeader>() {
            return Err(Error::NoCommand);
        }
        if resp_addrs.is_empty() {
            return Err(Error::NoResponseBuffer);
        }
        Ok(Request { body, resp_addrs })
    }

    fn reply(
        mem: &GuestMemoryMmap,
        resp_addrs: &[(GuestAddress, u32)],
        bytes: &[u8],
    ) -> Result<u32, Error> {
        let mut written = 0usize;
        for (addr, len) in resp_addrs {
            if written >= bytes.len() {
                break;
            }
            let n = std::cmp::min(*len as usize, bytes.len() - written);
            mem.write_slice(&bytes[written..written + n], *addr)
                .map_err(Error::GuestMemory)?;
            written += n;
        }
        Ok(written as u32)
    }

    /// Serve every chain the guest has queued.
    fn process_queue(&mut self, rutabaga: &mut Rutabaga, vring: &VringRwLock) -> Result<bool, Error> {
        let Some(atomic) = self.mem.clone() else {
            return Ok(false);
        };
        let guard = atomic.memory();
        let mem: &GuestMemoryMmap = &guard;

        let mut used = false;
        loop {
            let Some(mut chain) = vring
                .get_mut()
                .get_queue_mut()
                .pop_descriptor_chain(atomic.memory())
            else {
                break;
            };
            let head = chain.head_index();

            let written = match Self::split(mem, &mut chain) {
                Ok(Request { body, resp_addrs }) => {
                    let mut resp = self.dispatch(rutabaga, &body, mem);
                    // A fenced command is complete only once the renderer
                    // says so, and the guest learns that from this response:
                    // it signals every fence up to the one named here, but
                    // only if the response carries the fence flag. So the
                    // response is held until the fence retires, and then
                    // carries it.
                    if let Some(h) = CtrlHeader::from_slice(&body[..size_of::<CtrlHeader>()])
                        .filter(|h| h.flags & VIRTIO_GPU_FLAG_FENCE != 0)
                    {
                        let fence = RutabagaFence {
                            flags: h.flags,
                            fence_id: h.fence_id,
                            ctx_id: h.ctx_id,
                            ring_idx: h.ring_idx,
                        };
                        match rutabaga.create_fence(fence) {
                            Ok(()) => self.wait_fence(rutabaga, &fence),
                            Err(e) => warn!("virtio-gpu create_fence: {e}"),
                        }
                        if resp.len() >= 8 {
                            let flags = h.flags & (VIRTIO_GPU_FLAG_FENCE | VIRTIO_GPU_FLAG_INFO_RING_IDX);
                            resp[4..8].copy_from_slice(&flags.to_le_bytes());
                        }
                    }
                    Self::reply(mem, &resp_addrs, &resp)?
                }
                Err(e) => {
                    warn!("Malformed virtio-gpu request: {e}");
                    0
                }
            };

            vring
                .get_mut()
                .get_queue_mut()
                .add_used(mem, head, written)
                .map_err(|e| Error::Renderer(format!("returning a chain: {e}")))?;
            used = true;
        }
        Ok(used)
    }
}

impl VhostUserBackendMut for GpuBackend {
    type Bitmap = ();
    type Vring = VringRwLock;

    fn num_queues(&self) -> usize {
        NUM_QUEUES
    }

    fn max_queue_size(&self) -> usize {
        QUEUE_SIZE as usize
    }

    fn features(&self) -> u64 {
        // PROTOCOL_FEATURES is what lets the two sides negotiate everything
        // else. Without it the monitor cannot even read the config space, so
        // the guest sees a device with no capsets and no features at all.
        (1 << VIRTIO_F_VERSION_1)
            | (1 << VIRTIO_F_NOTIFY_ON_EMPTY)
            | (1 << VIRTIO_RING_F_EVENT_IDX)
            | (1u64 << VIRTIO_GPU_F_VIRGL)
            | (1u64 << VIRTIO_GPU_F_CONTEXT_INIT)
            | (1u64 << VIRTIO_GPU_F_RESOURCE_BLOB)
            | VhostUserVirtioFeatures::PROTOCOL_FEATURES.bits()
    }

    fn acked_features(&mut self, features: u64) {
        self.acked_features = features;
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
        let config = VirtioGpuConfig {
            events_read: 0,
            events_clear: 0,
            num_scanouts: 1,
            num_capsets: capset_count(&self.config),
        };
        let bytes = config.as_slice();
        let start = std::cmp::min(offset as usize, bytes.len());
        let end = std::cmp::min(start + size as usize, bytes.len());
        bytes[start..end].to_vec()
    }

    fn update_memory(
        &mut self,
        mem: GuestMemoryAtomic<GuestMemoryMmap>,
    ) -> io::Result<()> {
        self.mem = Some(mem);
        self.kick_restore();
        Ok(())
    }

    fn set_backend_req_fd(&mut self, backend: Backend) {
        // Without this the device has no way to ask for a blob to be placed,
        // so blob resources simply do not work.
        backend.set_shmem_flag(true);
        backend.set_reply_ack_flag(true);
        self.frontend = Some(backend);
        self.kick_restore();
    }

    fn handle_event(
        &mut self,
        device_event: u16,
        _evset: EventSet,
        vrings: &[VringRwLock],
        _thread_id: usize,
    ) -> io::Result<()> {
        let config = self.config.clone();
        RENDERER.with_borrow_mut(|slot| {
            let rutabaga = match slot {
                Some(r) => r,
                None => slot.insert(
                    build_rutabaga(&config, self.retired.clone()).map_err(|e| io::Error::other(format!("{e}")))?,
                ),
            };

            if device_event == SAVE_EVENT {
                let _ = self.save_evt.read();
                match self.save_session(rutabaga) {
                    Ok(summary) => log::info!("gpu session saved: {summary}"),
                    Err(e) => log::error!("saving the gpu session: {e}"),
                }
                return Ok(());
            }
            if device_event == RESTORE_EVENT {
                let _ = self.restore_evt.read();
                let summary = match self.restore_session(rutabaga) {
                    Ok(summary) => summary,
                    Err(e) => format!("failed: {e}"),
                };
                log::info!("gpu session restored: {summary}");
                self.mark_ready(&summary);
                return Ok(());
            }

            let Some(vring) = vrings.get(device_event as usize) else {
                return Err(io::Error::from_raw_os_error(libc::EINVAL));
            };

            // The specification's loop for a device that negotiated the event
            // index: silence the queue, drain it, then re-arm, which is also
            // what publishes the point the guest should kick again.
            loop {
                if self.event_idx {
                    vring.disable_notification().ok();
                }
                let used = self
                    .process_queue(rutabaga, vring)
                    .map_err(|e| io::Error::other(format!("{e}")))?;
                if used {
                    vring.signal_used_queue().ok();
                }
                if !self.event_idx || !vring.enable_notification().unwrap_or(false) {
                    break;
                }
            }
            Ok(())
        })
    }
}
