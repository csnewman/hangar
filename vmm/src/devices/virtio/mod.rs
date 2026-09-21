//! The virtio device model.
//!
//! A device here implements [`VirtioDevice`] and knows nothing about how it
//! is addressed. [`mmio::Transport`] is the only thing that does, which is
//! what keeps a PCI transport possible later without touching any device.

pub mod balloon;
pub mod blk;
pub mod fs;
pub mod fsopts;
pub mod mmio;
pub mod rng;
pub mod vsock;
pub mod worker;

use std::io;
use std::ops::Deref;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use virtio_queue::{Queue, QueueT};
use vm_memory::GuestAddressSpace;
use vmm_sys_util::eventfd::EventFd;

use crate::memory::Mem;

/// Device type numbers from the virtio specification.
pub const TYPE_BLOCK: u32 = 2;
pub const TYPE_RNG: u32 = 4;
pub const TYPE_BALLOON: u32 = 5;
pub const TYPE_VSOCK: u32 = 19;
pub const TYPE_FS: u32 = 26;

/// Feature bits every device here sets, because none of them speak the
/// pre-1.0 layout.
pub const VIRTIO_F_VERSION_1: u64 = 1 << 32;

/// A descriptor may point at a table of further descriptors, so a long
/// request costs one entry in the queue rather than one per segment.
pub const VIRTIO_RING_F_INDIRECT_DESC: u64 = 1 << 28;

/// Each side publishes the point at which it wants to be told, so a guest
/// that is already draining a queue is not interrupted to be told there is
/// more in it.
pub const VIRTIO_RING_F_EVENT_IDX: u64 = 1 << 29;

/// What the transport offers on every device's behalf.
///
/// These describe how a queue is laid out and when it is signalled, not what
/// the device does with it, and every device here is served by the same queue
/// handling -- so they are offered once rather than repeated per device.
pub const TRANSPORT_FEATURES: u64 =
    VIRTIO_F_VERSION_1 | VIRTIO_RING_F_INDIRECT_DESC | VIRTIO_RING_F_EVENT_IDX;

/// Interrupt status bits, as the virtio-mmio transport defines them.
pub const INT_VRING: u32 = 1 << 0;

/// A region of guest physical address space a device owns outside its
/// registers.
///
/// virtio calls these shared memory regions. The only one here is the
/// filesystem's DAX window, which the guest finds by reading the transport's
/// shared memory registers and then maps as device memory.
#[derive(Clone, Copy, Debug)]
pub struct ShmRegion {
    pub id: u8,
    pub addr: u64,
    pub size: u64,
}

/// The line a device raises to tell the guest something happened.
///
/// Raising it is a write to an eventfd that KVM has bound to a GIC interrupt,
/// so a device thread signals the guest without an ioctl and without waking
/// this process's main thread.
pub struct Interrupt {
    status: AtomicU32,
    evt: EventFd,
    /// Whether a reader of the status always sees a queue interrupt.
    ///
    /// A device whose queues are served by the host kernel hands the
    /// interrupt descriptor straight to it, so the kernel raises the guest's
    /// interrupt without passing through this process and cannot set the
    /// status word. A guest that then reads zero concludes the interrupt was
    /// not for this device and ignores it. Reporting a queue interrupt
    /// unconditionally makes the driver check its queues, which is what the
    /// kernel signalled for; a check that finds nothing costs one pass over
    /// the used rings.
    always_queue: bool,
}

impl Interrupt {
    pub fn new(evt: EventFd) -> Self {
        Self {
            status: AtomicU32::new(0),
            evt,
            always_queue: false,
        }
    }

    /// An interrupt whose descriptor is written by something other than this
    /// process -- see `always_queue`.
    pub fn external(evt: EventFd) -> Self {
        Self {
            status: AtomicU32::new(0),
            evt,
            always_queue: true,
        }
    }

    /// The descriptor an external signaller writes to raise this interrupt.
    pub fn raw_eventfd(&self) -> &EventFd {
        &self.evt
    }

    /// Tell the guest a queue has used buffers in it.
    pub fn signal_queue(&self) -> io::Result<()> {
        self.signal(INT_VRING)
    }

    /// Tell the guest about `queue`, if it has asked to be told.
    ///
    /// With the event index in use the guest publishes how far it has got, so
    /// a queue it is already draining needs no interrupt at all.
    pub fn signal_if_wanted(&self, queue: &mut Queue, mem: &Mem) -> io::Result<()> {
        let guard = mem.memory();
        match queue.needs_notification(guard.deref()) {
            Ok(false) => Ok(()),
            Ok(true) => self.signal_queue(),
            Err(e) => {
                log::warn!("reading the guest's notification point: {e}");
                self.signal_queue()
            }
        }
    }

    fn signal(&self, bits: u32) -> io::Result<()> {
        // The status has to be visible before the interrupt: the guest reads
        // it from the handler to find out why it was interrupted, and a
        // handler that reads zero concludes the interrupt was not ours.
        self.status.fetch_or(bits, Ordering::SeqCst);
        self.evt.write(1)
    }

    pub fn status(&self) -> u32 {
        let status = self.status.load(Ordering::SeqCst);
        if self.always_queue {
            status | INT_VRING
        } else {
            status
        }
    }

    pub fn ack(&self, bits: u32) {
        self.status.fetch_and(!bits, Ordering::SeqCst);
    }
}

/// A queue handed to a device once the driver has set it up.
pub struct ActiveQueue {
    /// Which of the device's queues this is. Devices whose queue layout has
    /// optional members -- the balloon, whose queues exist whether or not
    /// their feature was offered -- identify a queue by this rather than by
    /// its position in the list.
    pub index: usize,
    pub queue: Queue,
    /// Signalled when the guest kicks this queue. KVM writes it directly from
    /// the vCPU's MMIO exit, so the guest's notification never reaches this
    /// process as an exit.
    pub kick: EventFd,
}

/// What every virtio device implements.
pub trait VirtioDevice: Send {
    fn device_type(&self) -> u32;

    /// Maximum size of each queue, which also fixes how many there are.
    fn queue_max_sizes(&self) -> Vec<u16>;

    /// Features this device offers.
    fn features(&self) -> u64;

    /// Which of the transport's own features this device can have added.
    ///
    /// A device whose queues this process parses can have all of them. A
    /// device whose queues are parsed by something else -- vsock's, by the
    /// host kernel -- can only have the ones that parser understands, or the
    /// guest and the parser would disagree about the ring layout.
    fn transport_features(&self) -> u64 {
        TRANSPORT_FEATURES
    }

    /// Features the driver accepted. A device that cares records them here.
    fn ack_features(&mut self, _value: u64) {}

    fn read_config(&self, _offset: u64, data: &mut [u8]) {
        data.fill(0);
    }

    fn write_config(&mut self, _offset: u64, _data: &[u8]) {}

    /// The shared memory region the guest can map, if there is one.
    fn shm_region(&self) -> Option<ShmRegion> {
        None
    }

    /// Start serving. Called once, when the driver sets DRIVER_OK.
    ///
    /// The device takes ownership of its queues here and is expected to do
    /// its work on its own thread; the caller is a vCPU finishing an MMIO
    /// write and must not be kept waiting.
    fn activate(
        &mut self,
        mem: Mem,
        interrupt: Arc<Interrupt>,
        queues: Vec<ActiveQueue>,
    ) -> io::Result<()>;
}
