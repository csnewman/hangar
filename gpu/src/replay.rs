// SPDX-License-Identifier: Apache-2.0

//! What a guest has built on the GPU, in a form that can be built again.
//!
//! A renderer's state cannot be read out. A guest's GL context is compiled
//! shaders, bound samplers, framebuffers and driver allocations, and neither GL
//! nor the renderer will serialise them. What *can* be read out is the data in
//! each resource, and what can be *observed* is every command that defined the
//! rest, because they all pass through this process on their way to the
//! renderer.
//!
//! So this keeps two things. For each resource, what it was created as and
//! what backs it; its contents are read back from the renderer when the guest
//! is suspended. For each virgl context, the part of its command stream that
//! defines state -- object creates not yet destroyed, and the latest value of
//! every binding -- and none of the part that uses it. Draws, clears, blits,
//! copies and transfers are dropped: their only lasting effect is on resource
//! contents, which are carried directly. The record therefore stays the size of
//! the state, however long the guest runs.
//!
//! Replaying it into a fresh renderer, with the contents written back, gives
//! the guest's context what it had: the same objects under the same handles,
//! bound the same way, over resources holding the same data.
//!
//! Venus is different. Its commands travel through a ring in shared memory
//! that the renderer reads directly, so they never pass through here. The
//! renderer records and rebuilds a Venus context itself, and this keeps its
//! snapshot alongside the rest; see `Context::venus`.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// Command numbers from virglrenderer's `virgl_protocol.h`.
mod ccmd {
    pub const CREATE_OBJECT: u32 = 1;
    pub const BIND_OBJECT: u32 = 2;
    pub const DESTROY_OBJECT: u32 = 3;
    pub const SET_VIEWPORT_STATE: u32 = 4;
    pub const SET_FRAMEBUFFER_STATE: u32 = 5;
    pub const SET_VERTEX_BUFFERS: u32 = 6;
    pub const SET_SAMPLER_VIEWS: u32 = 10;
    pub const SET_INDEX_BUFFER: u32 = 11;
    pub const SET_CONSTANT_BUFFER: u32 = 12;
    pub const SET_STENCIL_REF: u32 = 13;
    pub const SET_BLEND_COLOR: u32 = 14;
    pub const SET_SCISSOR_STATE: u32 = 15;
    pub const BIND_SAMPLER_STATES: u32 = 18;
    pub const SET_POLYGON_STIPPLE: u32 = 22;
    pub const SET_CLIP_STATE: u32 = 23;
    pub const SET_SAMPLE_MASK: u32 = 24;
    pub const SET_STREAMOUT_TARGETS: u32 = 25;
    pub const SET_UNIFORM_BUFFER: u32 = 27;
    pub const SET_SUB_CTX: u32 = 28;
    pub const CREATE_SUB_CTX: u32 = 29;
    pub const DESTROY_SUB_CTX: u32 = 30;
    pub const BIND_SHADER: u32 = 31;
    pub const SET_TESS_STATE: u32 = 32;
    pub const SET_MIN_SAMPLES: u32 = 33;
    pub const SET_SHADER_BUFFERS: u32 = 34;
    pub const SET_SHADER_IMAGES: u32 = 35;
    pub const SET_FRAMEBUFFER_STATE_NO_ATTACH: u32 = 38;
    pub const SET_ATOMIC_BUFFERS: u32 = 40;
    pub const SET_DEBUG_FLAGS: u32 = 41;
    pub const SET_TWEAKS: u32 = 46;
    pub const PIPE_RESOURCE_CREATE: u32 = 48;
    pub const PIPE_RESOURCE_SET_TYPE: u32 = 49;
}

const OBJECT_SHADER: u32 = 4;
/// A shader longer than one command continues in further creates for the same
/// handle, marked by this bit in their offset field.
const SHADER_OFFSET_CONT: u32 = 1 << 31;

/// A command header: the command, the object type it concerns, and how many
/// payload dwords follow.
pub fn header(cmd: u32, obj: u32, len: u32) -> u32 {
    cmd | (obj << 8) | (len << 16)
}

/// Everything recorded about a guest's GPU session.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Session {
    pub resources: BTreeMap<u32, Resource>,
    pub contexts: BTreeMap<u32, Context>,
    /// scanout -> resource shown on it.
    pub scanouts: BTreeMap<u32, u32>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Create3D {
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Blob {
    pub ctx_id: u32,
    pub blob_mem: u32,
    pub blob_flags: u32,
    pub blob_id: u64,
    pub size: u64,
    /// Guest memory backing a guest-backed blob, as guest-physical ranges.
    pub entries: Vec<(u64, u32)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Kind {
    ThreeD(Create3D),
    Blob(Blob),
}

/// A region of a resource's contents, saved alongside the session.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Chunk {
    pub level: u32,
    pub x: u32,
    pub y: u32,
    pub z: u32,
    pub w: u32,
    pub h: u32,
    pub d: u32,
    pub stride: u32,
    pub layer_stride: u32,
    /// Where the bytes are in the contents file, and how many.
    pub offset: u64,
    pub len: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Resource {
    pub kind: Kind,
    /// Guest memory attached as backing, as guest-physical ranges. Physical
    /// rather than host addresses, because a restored guest's memory is
    /// mapped somewhere else in the process that restores it.
    pub backing: Option<Vec<(u64, u32)>>,
    /// Where in the shared window the blob was placed, if it was.
    pub mapped_at: Option<u64>,
    /// Filled in when the session is saved.
    #[serde(default)]
    pub contents: Vec<Chunk>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Context {
    pub init: u32,
    pub name: Option<String>,
    pub attached: BTreeSet<u32>,
    /// The context's virgl state. None for a context whose protocol is not
    /// understood here, which cannot be rebuilt.
    pub virgl: Option<Virgl>,
    /// Where a Venus context's own snapshot is in the contents file, as
    /// offset and length. Venus is rebuilt by the renderer itself, which is
    /// the only place its commands are seen.
    #[serde(default)]
    pub venus: Option<(u64, u64)>,
    /// A Venus context's rings, so that a context which cannot be rebuilt can
    /// be marked failed where its guest driver looks.
    #[serde(default)]
    pub rings: Vec<Ring>,
}

/// Where a Venus ring keeps its status word.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Ring {
    pub id: u64,
    pub resource: u32,
    /// Byte offset of the status word within the resource.
    pub status: u64,
}

const VN_COMMAND_CREATE_RING: u32 = 188;
const VN_COMMAND_DESTROY_RING: u32 = 189;

/// The ring a submission creates or destroys, if it does.
///
/// Venus commands carry no length, so a stream cannot be walked without
/// decoding every command in it. It does not need to be: the guest driver
/// sends each ring's creation and destruction as a submission of its own, so
/// only the first command is looked at.
pub enum RingChange {
    Created(Ring),
    Destroyed(u64),
}

pub fn ring_change(bytes: &[u8]) -> Option<RingChange> {
    let u32_at = |q: usize| {
        bytes
            .get(q..q + 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
    };
    let u64_at = |q: usize| {
        bytes
            .get(q..q + 8)
            .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
    };
    match u32_at(0)? {
        VN_COMMAND_DESTROY_RING => Some(RingChange::Destroyed(u64_at(8)?)),
        VN_COMMAND_CREATE_RING => {
            // type, flags, ring id, pCreateInfo marker, sType
            let id = u64_at(8)?;
            let mut q = 8 + 8;
            if u64_at(q)? == 0 {
                return None;
            }
            q += 8 + 4;
            // The pNext chain: each entry is a marker, its sType, the rest of
            // the chain, then its own single 32-bit field (the monitor's
            // reporting period, or a priority).
            let mut depth = 0;
            while u64_at(q)? != 0 {
                q += 8 + 4;
                depth += 1;
                if depth > 8 {
                    return None;
                }
            }
            q += 8 + 4 * depth;
            // flags, resourceId, then offset, size, idleTimeout, headOffset,
            // tailOffset, statusOffset, ...
            let resource = u32_at(q + 4)?;
            let offset = u64_at(q + 8)?;
            let status_offset = u64_at(q + 8 + 8 * 5)?;
            Some(RingChange::Created(Ring {
                id,
                resource,
                status: offset + status_offset,
            }))
        }
        _ => None,
    }
}

/// The capset whose command stream is Venus.
pub fn is_venus(context_init: u32) -> bool {
    context_init & 0xff == 4
}

/// The capsets whose command stream is virgl.
pub fn is_virgl(context_init: u32) -> bool {
    matches!(context_init & 0xff, 0..=2)
}

impl Context {
    pub fn new(init: u32, name: Option<String>) -> Self {
        Self {
            init,
            name,
            attached: BTreeSet::new(),
            virgl: is_virgl(init).then(Virgl::new),
            venus: None,
            rings: Vec::new(),
        }
    }
}

/// One virgl object and the commands that created it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Obj {
    pub seq: u64,
    pub kind: u32,
    pub cmds: Vec<Vec<u32>>,
}

/// One virgl sub-context: the objects in it and what is bound.
///
/// Bindings that name slots are kept slot by slot, following the renderer's
/// own semantics, rather than as the commands that set them. A program that
/// sets a constant buffer every frame would otherwise grow the record every
/// frame; slot by slot it is only as large as the state.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Sub {
    pub next_seq: u64,
    pub objects: BTreeMap<u32, Obj>,
    /// Bindings with one value per key, as the latest whole command.
    pub singles: BTreeMap<String, Vec<u32>>,
    /// shader -> sampler view handles. Setting views also unbinds every slot
    /// above the ones set, so this is a prefix, not a sparse map.
    pub views: BTreeMap<u32, Vec<u32>>,
    /// shader -> slot -> sampler state handle.
    pub samplers: BTreeMap<u32, BTreeMap<u32, u32>>,
    /// "shader:slot" -> offset, length, resource.
    pub ssbos: BTreeMap<String, Vec<u32>>,
    /// "shader:slot" -> format, access, layer offset, level size, resource.
    pub images: BTreeMap<String, Vec<u32>>,
    /// slot -> offset, length, resource.
    pub abos: BTreeMap<u32, Vec<u32>>,
    /// slot -> scale and translate.
    pub viewports: BTreeMap<u32, Vec<u32>>,
    /// slot -> min and max corners.
    pub scissors: BTreeMap<u32, Vec<u32>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Virgl {
    pub current: u32,
    pub subs: BTreeMap<u32, Sub>,
    /// blob id -> the PIPE_RESOURCE_CREATE that a blob of that id is made
    /// from. Replayed before the blob is created again.
    pub pipe_resources: BTreeMap<u32, Vec<u32>>,
    /// resource -> the latest PIPE_RESOURCE_SET_TYPE for it.
    pub set_types: BTreeMap<u32, Vec<u32>>,
}

impl Virgl {
    pub fn new() -> Self {
        let mut subs = BTreeMap::new();
        subs.insert(0, Sub::default());
        Self {
            current: 0,
            subs,
            pipe_resources: BTreeMap::new(),
            set_types: BTreeMap::new(),
        }
    }

    /// Records what a submitted command stream defines.
    pub fn observe(&mut self, bytes: &[u8]) {
        let dw: Vec<u32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes(*c))
            .collect();
        let mut i = 0;
        while i < dw.len() {
            let h = dw[i];
            let len = (h >> 16) as usize;
            let cmd = h & 0xff;
            let obj = (h >> 8) & 0xff;
            if i + 1 + len > dw.len() {
                // A truncated command is the renderer's to reject, and
                // nothing after it can be parsed.
                break;
            }
            let full = &dw[i..i + 1 + len];
            let p = &dw[i + 1..i + 1 + len];
            self.observe_one(cmd, obj, p, full);
            i += 1 + len;
        }
    }

    fn observe_one(&mut self, cmd: u32, obj: u32, p: &[u32], full: &[u32]) {
        use ccmd::*;
        match cmd {
            CREATE_SUB_CTX if !p.is_empty() => {
                self.subs.insert(p[0], Sub::default());
                return;
            }
            DESTROY_SUB_CTX if !p.is_empty() => {
                if p[0] != 0 {
                    self.subs.remove(&p[0]);
                }
                return;
            }
            SET_SUB_CTX if !p.is_empty() => {
                self.current = p[0];
                self.subs.entry(p[0]).or_default();
                return;
            }
            PIPE_RESOURCE_CREATE if p.len() >= 11 => {
                self.pipe_resources.insert(p[10], full.to_vec());
                return;
            }
            PIPE_RESOURCE_SET_TYPE if !p.is_empty() => {
                self.set_types.insert(p[0], full.to_vec());
                return;
            }
            _ => {}
        }

        let sub = self.subs.entry(self.current).or_default();
        let key = |parts: &[u32]| -> String {
            parts
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(":")
        };
        match cmd {
            CREATE_OBJECT if !p.is_empty() => {
                let handle = p[0];
                let continues =
                    obj == OBJECT_SHADER && p.len() > 2 && p[2] & SHADER_OFFSET_CONT != 0;
                if continues {
                    if let Some(o) = sub.objects.get_mut(&handle) {
                        o.cmds.push(full.to_vec());
                    }
                } else {
                    let seq = sub.next_seq;
                    sub.next_seq += 1;
                    sub.objects.insert(
                        handle,
                        Obj {
                            seq,
                            kind: obj,
                            cmds: vec![full.to_vec()],
                        },
                    );
                }
            }
            DESTROY_OBJECT if !p.is_empty() => {
                sub.objects.remove(&p[0]);
            }
            BIND_OBJECT => {
                sub.singles.insert(key(&[BIND_OBJECT, obj]), full.to_vec());
            }
            BIND_SHADER if p.len() >= 2 => {
                sub.singles.insert(key(&[BIND_SHADER, p[1]]), full.to_vec());
            }
            SET_FRAMEBUFFER_STATE => {
                sub.singles.insert("fb".into(), full.to_vec());
            }
            SET_FRAMEBUFFER_STATE_NO_ATTACH => {
                // Not a framebuffer of its own: the default size of the one
                // bound, which Mesa sends after every framebuffer it sets. It
                // is kept apart so it cannot stand in for the attachments,
                // and its key sorts after "fb" so it is replayed after them.
                sub.singles.insert("fb:size".into(), full.to_vec());
            }
            SET_CONSTANT_BUFFER | SET_UNIFORM_BUFFER if p.len() >= 2 => {
                sub.singles.insert(key(&[cmd, p[0], p[1]]), full.to_vec());
            }
            SET_TWEAKS if !p.is_empty() => {
                sub.singles.insert(key(&[cmd, p[0]]), full.to_vec());
            }
            SET_VERTEX_BUFFERS
            | SET_INDEX_BUFFER
            | SET_STENCIL_REF
            | SET_BLEND_COLOR
            | SET_POLYGON_STIPPLE
            | SET_CLIP_STATE
            | SET_SAMPLE_MASK
            | SET_STREAMOUT_TARGETS
            | SET_TESS_STATE
            | SET_MIN_SAMPLES
            | SET_DEBUG_FLAGS => {
                sub.singles.insert(key(&[cmd]), full.to_vec());
            }
            SET_SAMPLER_VIEWS if p.len() >= 2 => {
                let (shader, start) = (p[0], p[1] as usize);
                let handles = &p[2..];
                let views = sub.views.entry(shader).or_default();
                let end = start + handles.len();
                views.resize(end.max(views.len()), 0);
                views[start..end].copy_from_slice(handles);
                views.truncate(end);
            }
            BIND_SAMPLER_STATES if p.len() >= 2 => {
                let (shader, start) = (p[0], p[1]);
                let slots = sub.samplers.entry(shader).or_default();
                for (i, &h) in p[2..].iter().enumerate() {
                    slots.insert(start + i as u32, h);
                }
            }
            SET_SHADER_BUFFERS if p.len() >= 2 => {
                for (i, e) in p[2..].as_chunks::<3>().0.iter().enumerate() {
                    sub.ssbos.insert(key(&[p[0], p[1] + i as u32]), e.to_vec());
                }
            }
            SET_SHADER_IMAGES if p.len() >= 2 => {
                for (i, e) in p[2..].as_chunks::<5>().0.iter().enumerate() {
                    sub.images.insert(key(&[p[0], p[1] + i as u32]), e.to_vec());
                }
            }
            SET_ATOMIC_BUFFERS if !p.is_empty() => {
                for (i, e) in p[1..].as_chunks::<3>().0.iter().enumerate() {
                    sub.abos.insert(p[0] + i as u32, e.to_vec());
                }
            }
            SET_VIEWPORT_STATE if !p.is_empty() => {
                for (i, e) in p[1..].as_chunks::<6>().0.iter().enumerate() {
                    sub.viewports.insert(p[0] + i as u32, e.to_vec());
                }
            }
            SET_SCISSOR_STATE if !p.is_empty() => {
                for (i, e) in p[1..].as_chunks::<2>().0.iter().enumerate() {
                    sub.scissors.insert(p[0] + i as u32, e.to_vec());
                }
            }
            // Everything else either uses state rather than defining it --
            // draws, clears, blits, copies, transfers, queries, barriers,
            // markers -- or leaves its lasting effect in resource contents,
            // which are carried separately.
            _ => {}
        }
    }

    /// The PIPE_RESOURCE_CREATE commands the given blobs were made from.
    pub fn pipe_resource_commands(&self, blob_ids: &BTreeSet<u32>) -> Vec<u32> {
        blob_ids
            .iter()
            .filter_map(|id| self.pipe_resources.get(id))
            .flatten()
            .copied()
            .collect()
    }

    /// Commands that rebuild this context's state in a fresh renderer.
    ///
    /// `live` is the set of resources that exist again. A binding that names
    /// something which does not -- an object destroyed while still bound, a
    /// resource gone -- is replayed with nothing in its place: the renderer
    /// treats an unknown handle as an error that poisons the whole context,
    /// and an empty binding is what the guest effectively had anyway.
    pub fn replay(&self, live: &BTreeSet<u32>) -> Vec<u32> {
        use ccmd::*;
        let mut out = Vec::new();
        let res = |id: u32| if id == 0 || live.contains(&id) { id } else { 0 };

        for (&res_id, cmd) in &self.set_types {
            if live.contains(&res_id) {
                out.extend_from_slice(cmd);
            }
        }

        for (&id, sub) in &self.subs {
            if id != 0 {
                out.extend_from_slice(&[header(CREATE_SUB_CTX, 0, 1), id]);
            }
            out.extend_from_slice(&[header(SET_SUB_CTX, 0, 1), id]);
            let obj = |h: u32| {
                if h == 0 || sub.objects.contains_key(&h) {
                    h
                } else {
                    0
                }
            };

            let mut objects: Vec<&Obj> = sub.objects.values().collect();
            objects.sort_by_key(|o| o.seq);
            for o in objects {
                for c in &o.cmds {
                    out.extend_from_slice(c);
                }
            }

            for (k, c) in &sub.singles {
                let mut c = c.clone();
                let cmd = c[0] & 0xff;
                let p = &mut c[1..];
                match cmd {
                    BIND_OBJECT | BIND_SHADER => {
                        if !p.is_empty() && obj(p[0]) != p[0] {
                            continue;
                        }
                    }
                    SET_FRAMEBUFFER_STATE => {
                        for h in p.iter_mut().skip(1) {
                            *h = obj(*h);
                        }
                    }
                    SET_VERTEX_BUFFERS => {
                        for e in p.as_chunks_mut::<3>().0 {
                            e[2] = res(e[2]);
                        }
                    }
                    SET_INDEX_BUFFER => {
                        if !p.is_empty() {
                            p[0] = res(p[0]);
                        }
                    }
                    SET_UNIFORM_BUFFER => {
                        if p.len() >= 5 {
                            p[4] = res(p[4]);
                        }
                    }
                    SET_STREAMOUT_TARGETS => {
                        for h in p.iter_mut().skip(1) {
                            *h = obj(*h);
                        }
                    }
                    _ => {}
                }
                let _ = k;
                out.extend_from_slice(&c);
            }

            for (&shader, views) in &sub.views {
                out.push(header(SET_SAMPLER_VIEWS, 0, 2 + views.len() as u32));
                out.extend_from_slice(&[shader, 0]);
                out.extend(views.iter().map(|&h| obj(h)));
            }
            for (&shader, slots) in &sub.samplers {
                let n = slots.keys().max().map_or(0, |m| m + 1);
                out.push(header(BIND_SAMPLER_STATES, 0, 2 + n));
                out.extend_from_slice(&[shader, 0]);
                out.extend((0..n).map(|s| obj(slots.get(&s).copied().unwrap_or(0))));
            }
            for (k, e) in &sub.ssbos {
                let (shader, slot) = split2(k);
                out.extend_from_slice(&[
                    header(SET_SHADER_BUFFERS, 0, 5),
                    shader,
                    slot,
                    e[0],
                    e[1],
                    res(e[2]),
                ]);
            }
            for (k, e) in &sub.images {
                let (shader, slot) = split2(k);
                out.extend_from_slice(&[
                    header(SET_SHADER_IMAGES, 0, 7),
                    shader,
                    slot,
                    e[0],
                    e[1],
                    e[2],
                    e[3],
                    res(e[4]),
                ]);
            }
            for (&slot, e) in &sub.abos {
                out.extend_from_slice(&[
                    header(SET_ATOMIC_BUFFERS, 0, 4),
                    slot,
                    e[0],
                    e[1],
                    res(e[2]),
                ]);
            }
            for (&slot, e) in &sub.viewports {
                out.push(header(SET_VIEWPORT_STATE, 0, 7));
                out.push(slot);
                out.extend_from_slice(e);
            }
            for (&slot, e) in &sub.scissors {
                out.push(header(SET_SCISSOR_STATE, 0, 3));
                out.push(slot);
                out.extend_from_slice(e);
            }
        }

        out.extend_from_slice(&[header(SET_SUB_CTX, 0, 1), self.current]);
        out
    }
}

/// The commands in a virgl command stream, as `command/object` pairs, for
/// tracing.
pub fn describe(bytes: &[u8]) -> String {
    let dw: Vec<u32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_le_bytes(*c))
        .collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < dw.len() {
        let h = dw[i];
        let len = (h >> 16) as usize;
        let mut s = format!("{}/{}", h & 0xff, (h >> 8) & 0xff);
        if len >= 1 && i + 1 < dw.len() {
            s += &format!("({})", dw[i + 1]);
        }
        out.push(s);
        i += 1 + len;
    }
    out.join(" ")
}

fn split2(k: &str) -> (u32, u32) {
    let mut it = k.split(':').map(|s| s.parse::<u32>().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0))
}

/// Bytes per texel of a virgl format, where that is a whole number and the
/// format is one a guest renders into or samples from.
///
/// Compressed, subsampled and bitmap formats return None; their contents are
/// not carried across a restore.
pub fn bytes_per_texel(format: u32) -> Option<u32> {
    Some(match format {
        9 | 10 | 11 | 23 | 64 | 69 | 74 | 82 | 95 | 139 | 147 | 148 | 150 | 168 | 169 => 1,
        5 | 6 | 7 | 12 | 13 | 16 | 48 | 52 | 56 | 60 | 65 | 70 | 75 | 83 | 91 | 96 | 120 | 122
        | 133 | 135 | 141 | 142 | 149 | 151 | 152 | 154 | 155 | 156 | 158 | 170 | 171 => 2,
        66 | 71 | 76 | 84 | 97 => 3,
        1 | 2 | 3 | 4 | 8 | 17 | 18 | 19 | 20 | 21 | 22 | 28 | 32 | 36 | 40 | 44 | 49 | 53 | 57
        | 61 | 67 | 68 | 72 | 77 | 85 | 87 | 92 | 98 | 99 | 100 | 101 | 103 | 104 | 119 | 121
        | 123 | 124 | 125 | 131 | 132 | 134 | 136 | 137 | 140 | 153 | 157 | 159 | 160 | 162
        | 172 | 173 | 174 | 175 | 176 => 4,
        50 | 54 | 58 | 62 | 93 => 6,
        24 | 29 | 33 | 37 | 41 | 45 | 51 | 55 | 59 | 63 | 88 | 94 | 126 | 161 => 8,
        30 | 34 | 38 | 42 | 46 | 89 => 12,
        25 | 31 | 35 | 39 | 43 | 47 | 90 => 16,
        26 => 24,
        27 => 32,
        _ => return None,
    })
}

/// pipe_texture_target values.
const TARGET_BUFFER: u32 = 0;
const TARGET_1D: u32 = 1;
const TARGET_3D: u32 = 3;
const TARGET_1D_ARRAY: u32 = 6;

/// The regions a resource's contents are read and written in: one per mip
/// level, each covering every layer. Empty if the contents cannot be carried.
pub fn chunks_for(c: &Create3D) -> Result<Vec<Chunk>, String> {
    if c.nr_samples > 1 {
        return Err(format!(
            "{} samples per texel cannot be read back",
            c.nr_samples
        ));
    }
    if c.target == TARGET_BUFFER {
        return Ok(vec![Chunk {
            level: 0,
            x: 0,
            y: 0,
            z: 0,
            w: c.width,
            h: 1,
            d: 1,
            stride: 0,
            layer_stride: 0,
            offset: 0,
            len: c.width as u64,
        }]);
    }
    let bpp = bytes_per_texel(c.format)
        .ok_or_else(|| format!("format {} has no fixed texel size", c.format))?;
    let mut v = Vec::new();
    for level in 0..=c.last_level {
        let w = (c.width >> level).max(1);
        let (h, d) = match c.target {
            TARGET_1D => (1, 1),
            // A 1D array keeps its layers in the second dimension.
            TARGET_1D_ARRAY => (c.array_size.max(1), 1),
            TARGET_3D => ((c.height >> level).max(1), (c.depth >> level).max(1)),
            _ => ((c.height >> level).max(1), c.array_size.max(1)),
        };
        let stride = w * bpp;
        let layer_stride = stride * h;
        v.push(Chunk {
            level,
            x: 0,
            y: 0,
            z: 0,
            w,
            h,
            d,
            stride,
            layer_stride,
            offset: 0,
            len: layer_stride as u64 * d as u64,
        });
    }
    Ok(v)
}
