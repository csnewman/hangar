// SPDX-License-Identifier: Apache-2.0

//! The virtio-gpu wire protocol.
//!
//! Layouts and numbering come from the virtio specification. Nothing here
//! knows how the queues are carried, so it is the same whether the device
//! lives in the monitor or in a process of its own.

use vm_memory::ByteValued;

pub const QUEUE_SIZE: u16 = 256;
pub const NUM_QUEUES: usize = 2;
pub const QUEUE_SIZES: &[u16] = &[QUEUE_SIZE; NUM_QUEUES];

/// The queue the guest submits rendering commands on.
pub const CONTROL_QUEUE: u16 = 0;
/// The queue carrying cursor updates, kept separate so a busy renderer never
/// delays the pointer.
pub const CURSOR_QUEUE: u16 = 1;


// Feature bits from the virtio specification.
pub const VIRTIO_GPU_F_VIRGL: u32 = 0;
pub const VIRTIO_GPU_F_RESOURCE_BLOB: u32 = 3;
pub const VIRTIO_GPU_F_CONTEXT_INIT: u32 = 4;

// Commands. 2D and capset queries are answered here; the 3D commands arrive
// once the guest has negotiated VIRGL.
pub const VIRTIO_GPU_CMD_GET_DISPLAY_INFO: u32 = 0x0100;
pub const VIRTIO_GPU_CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
pub const VIRTIO_GPU_CMD_RESOURCE_UNREF: u32 = 0x0102;
pub const VIRTIO_GPU_CMD_SET_SCANOUT: u32 = 0x0103;
pub const VIRTIO_GPU_CMD_RESOURCE_FLUSH: u32 = 0x0104;
pub const VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
pub const VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;
pub const VIRTIO_GPU_CMD_RESOURCE_DETACH_BACKING: u32 = 0x0107;
pub const VIRTIO_GPU_CMD_GET_CAPSET_INFO: u32 = 0x0108;
pub const VIRTIO_GPU_CMD_GET_CAPSET: u32 = 0x0109;

pub const VIRTIO_GPU_CMD_CTX_CREATE: u32 = 0x0200;
pub const VIRTIO_GPU_CMD_CTX_DESTROY: u32 = 0x0201;
pub const VIRTIO_GPU_CMD_CTX_ATTACH_RESOURCE: u32 = 0x0202;
pub const VIRTIO_GPU_CMD_CTX_DETACH_RESOURCE: u32 = 0x0203;
pub const VIRTIO_GPU_CMD_RESOURCE_CREATE_3D: u32 = 0x0204;
pub const VIRTIO_GPU_CMD_TRANSFER_TO_HOST_3D: u32 = 0x0205;
pub const VIRTIO_GPU_CMD_TRANSFER_FROM_HOST_3D: u32 = 0x0206;
pub const VIRTIO_GPU_CMD_SUBMIT_3D: u32 = 0x0207;
pub const VIRTIO_GPU_CMD_RESOURCE_CREATE_BLOB: u32 = 0x010c;
pub const VIRTIO_GPU_CMD_RESOURCE_MAP_BLOB: u32 = 0x0208;
pub const VIRTIO_GPU_CMD_RESOURCE_UNMAP_BLOB: u32 = 0x0209;

pub const VIRTIO_GPU_CMD_UPDATE_CURSOR: u32 = 0x0300;
pub const VIRTIO_GPU_CMD_MOVE_CURSOR: u32 = 0x0301;

/// Set in a command header when the guest wants the command fenced.
pub const VIRTIO_GPU_FLAG_FENCE: u32 = 1 << 0;

// Responses.
pub const VIRTIO_GPU_RESP_OK_NODATA: u32 = 0x1100;
pub const VIRTIO_GPU_RESP_OK_DISPLAY_INFO: u32 = 0x1101;
pub const VIRTIO_GPU_RESP_OK_CAPSET_INFO: u32 = 0x1102;
pub const VIRTIO_GPU_RESP_OK_CAPSET: u32 = 0x1103;
pub const VIRTIO_GPU_RESP_OK_MAP_INFO: u32 = 0x1106;
pub const VIRTIO_GPU_RESP_ERR_UNSPEC: u32 = 0x1200;
pub const VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER: u32 = 0x1205;
pub const VIRTIO_GPU_RESP_ERR_INVALID_RESOURCE_ID: u32 = 0x1203;

/// The specification fixes the number of scanouts a device may expose.
pub const VIRTIO_GPU_MAX_SCANOUTS: usize = 16;

pub const DEFAULT_WIDTH: u32 = 1280;
pub const DEFAULT_HEIGHT: u32 = 800;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CtrlHeader {
    pub type_: u32,
    pub flags: u32,
    pub fence_id: u64,
    pub ctx_id: u32,
    pub ring_idx: u8,
    pub padding: [u8; 3],
}
// SAFETY: plain data, no padding beyond the explicit field, no invalid values.
unsafe impl ByteValued for CtrlHeader {}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
// SAFETY: plain data.
unsafe impl ByteValued for Rect {}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct DisplayOne {
    pub r: Rect,
    pub enabled: u32,
    pub flags: u32,
}
// SAFETY: plain data.
unsafe impl ByteValued for DisplayOne {}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RespDisplayInfo {
    pub hdr: CtrlHeader,
    pub pmodes: [DisplayOne; VIRTIO_GPU_MAX_SCANOUTS],
}
// SAFETY: plain data.
unsafe impl ByteValued for RespDisplayInfo {}

impl Default for RespDisplayInfo {
    fn default() -> Self {
        Self {
            hdr: CtrlHeader::default(),
            pmodes: [DisplayOne::default(); VIRTIO_GPU_MAX_SCANOUTS],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GetCapsetInfo {
    pub capset_index: u32,
    pub padding: u32,
}
// SAFETY: plain data.
unsafe impl ByteValued for GetCapsetInfo {}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct RespCapsetInfo {
    pub hdr: CtrlHeader,
    pub capset_id: u32,
    pub capset_max_version: u32,
    pub capset_max_size: u32,
    pub padding: u32,
}
// SAFETY: plain data.
unsafe impl ByteValued for RespCapsetInfo {}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GetCapset {
    pub capset_id: u32,
    pub capset_version: u32,
}
// SAFETY: plain data.
unsafe impl ByteValued for GetCapset {}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct ResourceCreate2D {
    pub resource_id: u32,
    pub format: u32,
    pub width: u32,
    pub height: u32,
}
// SAFETY: plain data.
unsafe impl ByteValued for ResourceCreate2D {}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct ResourceUnref {
    pub resource_id: u32,
    pub padding: u32,
}
// SAFETY: plain data.
unsafe impl ByteValued for ResourceUnref {}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SetScanout {
    pub r: Rect,
    pub scanout_id: u32,
    pub resource_id: u32,
}
// SAFETY: plain data.
unsafe impl ByteValued for SetScanout {}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct ResourceFlush {
    pub r: Rect,
    pub resource_id: u32,
    pub padding: u32,
}
// SAFETY: plain data.
unsafe impl ByteValued for ResourceFlush {}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct TransferToHost2D {
    pub r: Rect,
    pub offset: u64,
    pub resource_id: u32,
    pub padding: u32,
}
// SAFETY: plain data.
unsafe impl ByteValued for TransferToHost2D {}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AttachBacking {
    pub resource_id: u32,
    pub nr_entries: u32,
}
// SAFETY: plain data.
unsafe impl ByteValued for AttachBacking {}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MemEntry {
    pub addr: u64,
    pub length: u32,
    pub padding: u32,
}
// SAFETY: plain data.
unsafe impl ByteValued for MemEntry {}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CtxCreate {
    pub nlen: u32,
    pub context_init: u32,
    pub debug_name: [u8; 64],
}

impl Default for CtxCreate {
    fn default() -> Self {
        Self {
            nlen: 0,
            context_init: 0,
            debug_name: [0; 64],
        }
    }
}
// SAFETY: plain data.
unsafe impl ByteValued for CtxCreate {}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CtxResource {
    pub resource_id: u32,
    pub padding: u32,
}
// SAFETY: plain data.
unsafe impl ByteValued for CtxResource {}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct ResourceCreate3DReq {
    pub resource_id: u32,
    pub target: u32,
    pub format: u32,
    pub bind: u32,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub array_size: u32,
    pub last_level: u32,
    pub nr_samples: u32,
    pub flags: u32,
    pub padding: u32,
}
// SAFETY: plain data.
unsafe impl ByteValued for ResourceCreate3DReq {}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct TransferHost3D {
    pub box_: [u32; 6],
    pub offset: u64,
    pub resource_id: u32,
    pub level: u32,
    pub stride: u32,
    pub layer_stride: u32,
}
// SAFETY: plain data.
unsafe impl ByteValued for TransferHost3D {}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CmdSubmit {
    pub size: u32,
    pub num_in_fences: u32,
}
// SAFETY: plain data.
unsafe impl ByteValued for CmdSubmit {}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct ResourceCreateBlobReq {
    pub resource_id: u32,
    pub blob_mem: u32,
    pub blob_flags: u32,
    pub nr_entries: u32,
    pub blob_id: u64,
    pub size: u64,
}
// SAFETY: plain data.
unsafe impl ByteValued for ResourceCreateBlobReq {}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct ResourceMapBlob {
    pub resource_id: u32,
    pub padding: u32,
    pub offset: u64,
}
// SAFETY: plain data.
unsafe impl ByteValued for ResourceMapBlob {}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct RespMapInfo {
    pub hdr: CtrlHeader,
    pub map_info: u32,
    pub padding: u32,
}
// SAFETY: plain data.
unsafe impl ByteValued for RespMapInfo {}

/// The device configuration space the guest reads.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct VirtioGpuConfig {
    pub events_read: u32,
    pub events_clear: u32,
    pub num_scanouts: u32,
    pub num_capsets: u32,
}
// SAFETY: plain data.
unsafe impl ByteValued for VirtioGpuConfig {}

