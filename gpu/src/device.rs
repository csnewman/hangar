// SPDX-License-Identifier: Apache-2.0

//! The device: virtqueues in, rendering out.
//!
//! Blob resources are the one place this differs from a device living inside
//! a monitor. The window a guest maps them through belongs to the monitor, so
//! the resource's memory is sent there to be placed rather than mapped here.

use std::io;
use std::mem::size_of;
use std::os::unix::io::{AsRawFd, BorrowedFd};
use std::sync::{Arc, Mutex};

use log::{debug, warn};
use rutabaga_gfx::{
    ResourceCreate3D, ResourceCreateBlob, Rutabaga, RutabagaBuilder, RutabagaComponentType,
    RutabagaFence, RutabagaFenceHandler, RutabagaIovec, Transfer3D, RUTABAGA_CAPSET_VENUS,
    RUTABAGA_CAPSET_VIRGL, RUTABAGA_CAPSET_VIRGL2,
};
use vhost::vhost_user::message::{VhostUserMMap, VhostUserMMapFlags, VhostUserProtocolFeatures};
use vhost::vhost_user::{Backend, VhostUserFrontendReqHandler};
use vhost_user_backend::{VhostUserBackendMut, VringRwLock, VringT};
use virtio_bindings::bindings::virtio_config::{VIRTIO_F_NOTIFY_ON_EMPTY, VIRTIO_F_VERSION_1};
use virtio_bindings::bindings::virtio_ring::VIRTIO_RING_F_EVENT_IDX;
use vm_memory::{ByteValued, Bytes, GuestAddress, GuestMemoryAtomic, GuestMemoryMmap};
use vmm_sys_util::epoll::EventSet;

use crate::protocol::*;

/// How the device was asked to be configured.
#[derive(Clone, Copy, Debug)]
pub struct GpuConfig {
    pub virgl: bool,
    pub venus: bool,
    pub shm_id: u8,
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
}

fn capset_mask(config: GpuConfig) -> u64 {
    let mut m = 0u64;
    if config.virgl {
        m |= (1 << RUTABAGA_CAPSET_VIRGL) | (1 << RUTABAGA_CAPSET_VIRGL2);
    }
    if config.venus {
        m |= 1 << RUTABAGA_CAPSET_VENUS;
    }
    m
}

fn capset_count(config: GpuConfig) -> u32 {
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
fn build_rutabaga(config: GpuConfig) -> Result<Rutabaga, Error> {
    let handler = RutabagaFenceHandler::new(|_f: RutabagaFence| {});
    RutabagaBuilder::new(capset_mask(config), handler)
        .set_use_egl(true)
        .set_use_surfaceless(true)
        .set_use_vulkan(config.venus)
        .set_use_render_server(config.venus)
        .set_default_component(RutabagaComponentType::VirglRenderer)
        .build()
        .map_err(|e| Error::Renderer(format!("{e}")))
}

/// One command: the bytes the guest wrote, and where its answer goes.
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
    event_idx: bool,
    acked_features: u64,
}

impl GpuBackend {
    pub fn new(config: GpuConfig) -> Result<Self, Error> {
        log::info!("offering {} capset(s)", capset_count(config));
        Ok(GpuBackend {
            config,
            mem: None,
            frontend: None,
            next_offset: 0,
            placed: Vec::new(),
            event_idx: false,
            acked_features: 0,
        })
    }

    /// Ask the monitor to place a blob in the guest's window.
    fn map_blob(&mut self, rutabaga: &mut Rutabaga, resource_id: u32, guest_offset: u64) -> Result<u32, Error> {
        let frontend = self.frontend.as_ref().ok_or(Error::NoWindow)?;
        let mapping = rutabaga
            .export_blob(resource_id)
            .map_err(|e| Error::Renderer(format!("{e}")))?;
        let size = mapping.size;
        let req = VhostUserMMap {
            shmid: self.config.shm_id,
            padding: [0; 7],
            fd_offset: 0,
            shm_offset: guest_offset,
            len: size,
            flags: VhostUserMMapFlags::WRITABLE.bits(),
        };
        // SAFETY: the descriptor belongs to the handle returned above and
        // outlives this call.
        let fd = unsafe { BorrowedFd::borrow_raw(mapping.os_handle.as_raw_fd()) };
        frontend.shmem_map(&req, &fd).map_err(Error::Map)?;
        self.placed.push((resource_id, guest_offset, size));
        let _ = guest_offset;
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

    fn ok_or_err(result: Result<(), impl std::fmt::Display>, what: &str) -> u32 {
        match result {
            Ok(()) => VIRTIO_GPU_RESP_OK_NODATA,
            Err(e) => {
                warn!("virtio-gpu {what}: {e}");
                VIRTIO_GPU_RESP_ERR_UNSPEC
            }
        }
    }

    fn dispatch(&mut self, rutabaga: &mut Rutabaga, body: &[u8], mem: &GuestMemoryMmap) -> Vec<u8> {
        let hdr = match CtrlHeader::from_slice(&body[..size_of::<CtrlHeader>()]) {
            Some(h) => *h,
            None => return Vec::new(),
        };
        let payload = &body[size_of::<CtrlHeader>()..];

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
                let code = Self::ok_or_err(
                    rutabaga.resource_create_3d(req.resource_id, create),
                    "resource_create_2d",
                );
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
                let code = Self::ok_or_err(
                    rutabaga.resource_create_3d(req.resource_id, create),
                    "resource_create_3d",
                );
                Self::err(&hdr, code)
            }

            VIRTIO_GPU_CMD_RESOURCE_UNREF => {
                let Some(req) = ResourceUnref::from_slice(
                    payload.get(..size_of::<ResourceUnref>()).unwrap_or(&[]),
                ) else {
                    return Self::err(&hdr, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER);
                };
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
                }
                let code = Self::ok_or_err(
                    rutabaga.attach_backing(req.resource_id, vecs),
                    "attach_backing",
                );
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
                let code = Self::ok_or_err(
                    rutabaga
                        .create_context(hdr.ctx_id, req.context_init, name),
                    "ctx_create",
                );
                Self::err(&hdr, code)
            }

            VIRTIO_GPU_CMD_CTX_DESTROY => {
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
                    }
                    Some(vecs)
                };
                let create = ResourceCreateBlob {
                    blob_mem: req.blob_mem,
                    blob_flags: req.blob_flags,
                    blob_id: req.blob_id,
                    size: req.size,
                };
                let code = Self::ok_or_err(
                    rutabaga.resource_create_blob(
                        hdr.ctx_id,
                        req.resource_id,
                        create,
                        iovecs,
                        None,
                    ),
                    "resource_create_blob",
                );
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
                    Ok(map_info) => RespMapInfo {
                        hdr: Self::header_of(VIRTIO_GPU_RESP_OK_MAP_INFO, &hdr),
                        map_info,
                        padding: 0,
                    }
                    .as_slice()
                    .to_vec(),
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
                    let resp = self.dispatch(rutabaga, &body, mem);
                    // A fenced command is only complete once the renderer says
                    // so, which is what the guest waits on.
                    if let Some(h) = CtrlHeader::from_slice(&body[..size_of::<CtrlHeader>()])
                        .filter(|h| h.flags & VIRTIO_GPU_FLAG_FENCE != 0)
                    {
                        let fence = RutabagaFence {
                            flags: h.flags,
                            fence_id: h.fence_id,
                            ctx_id: h.ctx_id,
                            ring_idx: h.ring_idx,
                        };
                        if let Err(e) = rutabaga.create_fence(fence) {
                            warn!("virtio-gpu create_fence: {e}");
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
        let mut f = (1 << VIRTIO_F_VERSION_1)
            | (1 << VIRTIO_F_NOTIFY_ON_EMPTY)
            | (1 << VIRTIO_RING_F_EVENT_IDX)
            | (1u64 << VIRTIO_GPU_F_VIRGL)
            | (1u64 << VIRTIO_GPU_F_CONTEXT_INIT);
        // Blob resources need somewhere to be placed, which only exists when
        // the monitor publishes a window and agrees to serve map requests.
        f |= 1u64 << VIRTIO_GPU_F_RESOURCE_BLOB;
        f
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
            num_capsets: capset_count(self.config),
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
        Ok(())
    }

    fn set_backend_req_fd(&mut self, backend: Backend) {
        // Without this the device has no way to ask for a blob to be placed,
        // so blob resources simply do not work.
        backend.set_shmem_flag(true);
        backend.set_reply_ack_flag(true);
        self.frontend = Some(backend);
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

        let config = self.config;
        RENDERER.with_borrow_mut(|slot| {
            let rutabaga = match slot {
                Some(r) => r,
                None => slot.insert(
                    build_rutabaga(config).map_err(|e| io::Error::other(format!("{e}")))?,
                ),
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
