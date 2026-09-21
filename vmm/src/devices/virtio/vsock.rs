//! virtio-vsock, backed by the host kernel's vhost-vsock.
//!
//! The agent's channel is the one data path that does not belong in this
//! process. `/dev/vhost-vsock` lets the host kernel move packets between the
//! guest's virtqueues and a host socket directly, so an agent request never
//! causes a VM exit into userspace and never waits on a VMM thread.
//!
//! The consequence is that this device sets up queues and then steps out of
//! the way: the receive and transmit queues are handed to the kernel, and
//! only the event queue -- which carries nothing in practice -- stays here.

use std::io;
use std::sync::Arc;

use vhost::vhost_kern::vsock::Vsock as VhostVsockKern;
use vhost::vsock::VhostVsock;
use vhost::{VhostBackend, VhostUserMemoryRegionInfo, VringConfigData};
use virtio_queue::QueueT;
use vm_memory::{Address, GuestAddressSpace, GuestMemory, GuestMemoryRegion};

use super::worker::{self, Stop};
use super::{ActiveQueue, Interrupt, VirtioDevice, TYPE_VSOCK};
use crate::memory::Mem;

const QUEUE_SIZE: u16 = 256;
/// Receive, transmit, event.
const NUM_QUEUES: usize = 3;
/// The kernel backend drives the first two; the third stays here.
const VHOST_QUEUES: usize = 2;

/// Features the guest must never see.
///
/// `LOG_ALL` turns on dirty page logging, which the kernel then insists on
/// having a log buffer for and refuses to start without. `ACCESS_PLATFORM`
/// asks for an IOMMU there is none of. Both are offered by the kernel
/// because it can do them, not because this VM wants them.
const VHOST_F_LOG_ALL: u64 = 1 << 26;
const VIRTIO_F_ACCESS_PLATFORM: u64 = 1 << 33;

pub struct Vsock {
    cid: u32,
    stop: Stop,
    /// What the host kernel can do, asked once so the guest is offered
    /// exactly what will be honoured.
    available_features: u64,
    acked_features: u64,
    /// Kept alive for the life of the VM: dropping it closes the device and
    /// tears the guest's connections down.
    backend: Option<VhostVsockKern<Mem>>,
}

impl Vsock {
    /// Claim `/dev/vhost-vsock` for a guest with context ID `cid`.
    ///
    /// The device is opened while the VM is still being built rather than at
    /// activation, so a host without vhost-vsock fails before the guest has
    /// started booting. Asking for its features here means the guest is
    /// offered exactly what will be honoured later.
    pub fn new(cid: u32, mem: Mem, stop: Stop) -> io::Result<Self> {
        let backend = VhostVsockKern::new(mem)
            .map_err(|e| io::Error::other(format!("opening /dev/vhost-vsock: {e}")))?;
        let available_features = backend
            .get_features()
            .map_err(|e| io::Error::other(format!("reading vhost-vsock features: {e}")))?
            & !(VHOST_F_LOG_ALL | VIRTIO_F_ACCESS_PLATFORM);

        Ok(Self {
            cid,
            stop,
            available_features,
            acked_features: 0,
            backend: Some(backend),
        })
    }
}

impl VirtioDevice for Vsock {
    fn device_type(&self) -> u32 {
        TYPE_VSOCK
    }

    fn queue_max_sizes(&self) -> Vec<u16> {
        vec![QUEUE_SIZE; NUM_QUEUES]
    }

    fn features(&self) -> u64 {
        self.available_features
    }

    fn ack_features(&mut self, value: u64) {
        // The kernel parses the rings itself, so it has to be told exactly
        // what the guest agreed to: claiming an event index or indirect
        // descriptors the guest did not accept makes it read the wrong
        // bytes.
        self.acked_features = value & self.available_features;
    }

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        // The configuration space is the guest's context ID, as 64 bits.
        let cid = u64::from(self.cid).to_le_bytes();
        for (i, byte) in data.iter_mut().enumerate() {
            *byte = cid.get(offset as usize + i).copied().unwrap_or(0);
        }
    }

    fn activate(
        &mut self,
        mem: Mem,
        interrupt: Arc<Interrupt>,
        queues: Vec<ActiveQueue>,
    ) -> io::Result<()> {
        if queues.len() < NUM_QUEUES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "vsock needs {NUM_QUEUES} queues, the driver readied {}",
                    queues.len()
                ),
            ));
        }

        let backend = self
            .backend
            .as_ref()
            .expect("the device is opened when the guest is built");
        backend
            .set_owner()
            .map_err(|e| io::Error::other(format!("claiming the vhost-vsock device: {e}")))?;

        backend
            .set_features(self.acked_features)
            .map_err(|e| io::Error::other(format!("setting vhost-vsock features: {e}")))?;

        let guard = mem.memory();
        let regions: Vec<VhostUserMemoryRegionInfo> = guard
            .iter()
            .map(|r| VhostUserMemoryRegionInfo {
                guest_phys_addr: r.start_addr().raw_value(),
                memory_size: r.len(),
                userspace_addr: r.as_ptr() as u64,
                mmap_offset: 0,
                mmap_handle: -1,
            })
            .collect();
        backend.set_mem_table(&regions).map_err(|e| {
            io::Error::other(format!("giving the kernel the guest memory map: {e}"))
        })?;

        // The queues arrive carrying their own index, so the kernel's two
        // are picked out by number rather than by position.
        let mut queues = queues;
        queues.sort_by_key(|q| q.index);
        let event = queues.pop().expect("the queue list was checked above");

        for q in queues.iter().take(VHOST_QUEUES) {
            let index = q.index;
            let queue = &q.queue;
            backend
                .set_vring_num(index, queue.size())
                .map_err(|e| io::Error::other(format!("sizing vsock queue {index}: {e}")))?;
            let config = VringConfigData {
                queue_max_size: queue.max_size(),
                queue_size: queue.size(),
                flags: 0,
                desc_table_addr: queue.desc_table(),
                used_ring_addr: queue.used_ring(),
                avail_ring_addr: queue.avail_ring(),
                log_addr: None,
            };
            backend
                .set_vring_addr(index, &config)
                .map_err(|e| io::Error::other(format!("addressing vsock queue {index}: {e}")))?;
            backend
                .set_vring_base(index, 0)
                .map_err(|e| io::Error::other(format!("resetting vsock queue {index}: {e}")))?;
            backend
                .set_vring_kick(index, &q.kick)
                .map_err(|e| io::Error::other(format!("kick for vsock queue {index}: {e}")))?;
            // The kernel raises the guest's interrupt itself by writing this
            // descriptor, which KVM has bound to the device's interrupt line.
            backend
                .set_vring_call(index, interrupt.raw_eventfd())
                .map_err(|e| io::Error::other(format!("call for vsock queue {index}: {e}")))?;
        }

        backend
            .set_guest_cid(u64::from(self.cid))
            .map_err(|e| io::Error::other(format!("setting the guest CID to {}: {e}", self.cid)))?;
        backend
            .start()
            .map_err(|e| io::Error::other(format!("starting vhost-vsock: {e}")))?;

        // The event queue carries transport resets, which a guest running on
        // one host never sees. Buffers the driver posts are drained so the
        // queue does not fill, and nothing is ever written back.
        let mut event_queue = event.queue;
        let event_mem = mem.clone();
        worker::spawn(
            "hangar-vsock-ev".to_string(),
            vec![event.kick],
            self.stop.clone(),
            move |_| {
                let guard = event_mem.memory();
                while event_queue.pop_descriptor_chain(guard.clone()).is_some() {}
            },
        )?;

        Ok(())
    }
}
