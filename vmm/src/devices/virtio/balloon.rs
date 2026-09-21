//! virtio-balloon, used for reclaim rather than for resizing.
//!
//! Free page reporting is the part that matters: the guest tells the host
//! which of its pages are free as it frees them, and the host returns them,
//! continuously and without anyone deciding a target size. That is the
//! density lever for a node full of environments that are each using far less
//! memory than they were given.
//!
//! Inflate and deflate are implemented because the driver requires the queues
//! to exist, and because an explicit target is occasionally the right tool.
//! Both reclaim the same way: the pages are returned to the host, and the
//! guest faults them back in as zeroes if it ever touches them again.

use std::io;
use std::ops::Deref;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use virtio_queue::QueueT;
use vm_memory::{Bytes, GuestAddress, GuestAddressSpace, GuestMemory};

use super::worker::{self, Stop};
use super::{ActiveQueue, Interrupt, VirtioDevice, TYPE_BALLOON};
use crate::memory::Mem;

const QUEUE_SIZE: u16 = 128;

/// The driver asks for a fixed number of queues and identifies them by
/// position, so every slot exists whether or not its feature was offered.
const QUEUE_INFLATE: usize = 0;
const QUEUE_DEFLATE: usize = 1;
const QUEUE_REPORTING: usize = 4;
const NUM_QUEUES: usize = 5;

/// Feature bits.
const F_DEFLATE_ON_OOM: u64 = 1 << 2;
const F_REPORTING: u64 = 1 << 5;

/// The balloon counts in 4 KiB pages regardless of the guest's page size.
const BALLOON_PAGE_SIZE: u64 = 4096;

pub struct Balloon {
    free_page_reporting: bool,
    /// Pages the host is asking the guest to give up.
    num_pages: Arc<AtomicU32>,
    /// Pages the guest has actually given up.
    actual: Arc<AtomicU32>,
    stop: Stop,
}

impl Balloon {
    pub fn new(free_page_reporting: bool, stop: Stop) -> Self {
        Self {
            free_page_reporting,
            num_pages: Arc::new(AtomicU32::new(0)),
            actual: Arc::new(AtomicU32::new(0)),
            stop,
        }
    }
}

impl VirtioDevice for Balloon {
    fn device_type(&self) -> u32 {
        TYPE_BALLOON
    }

    fn queue_max_sizes(&self) -> Vec<u16> {
        vec![QUEUE_SIZE; NUM_QUEUES]
    }

    fn features(&self) -> u64 {
        let mut f = F_DEFLATE_ON_OOM;
        if self.free_page_reporting {
            f |= F_REPORTING;
        }
        f
    }

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        let mut config = [0u8; 8];
        config[..4].copy_from_slice(&self.num_pages.load(Ordering::SeqCst).to_le_bytes());
        config[4..].copy_from_slice(&self.actual.load(Ordering::SeqCst).to_le_bytes());
        for (i, byte) in data.iter_mut().enumerate() {
            *byte = config.get(offset as usize + i).copied().unwrap_or(0);
        }
    }

    fn write_config(&mut self, offset: u64, data: &[u8]) {
        // The guest only writes `actual`, to say how far it has got.
        if offset == 4 && data.len() >= 4 {
            let v = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
            self.actual.store(v, Ordering::SeqCst);
        }
    }

    fn activate(
        &mut self,
        mem: Mem,
        interrupt: Arc<Interrupt>,
        queues: Vec<ActiveQueue>,
    ) -> io::Result<()> {
        for ActiveQueue { index, queue, kick } in queues {
            let mut worker = Worker {
                queue,
                mem: mem.clone(),
                interrupt: interrupt.clone(),
                kind: match index {
                    QUEUE_INFLATE => Kind::Inflate,
                    QUEUE_DEFLATE => Kind::Deflate,
                    QUEUE_REPORTING => Kind::Reporting,
                    _ => Kind::Drain,
                },
            };
            worker::spawn(
                format!("hangar-balloon-{index}"),
                vec![kick],
                self.stop.clone(),
                move |_| worker.process(),
            )?;
        }
        Ok(())
    }
}

enum Kind {
    /// Buffers hold arrays of page frame numbers the guest has given up.
    Inflate,
    /// Buffers hold page frame numbers the guest is taking back. Nothing has
    /// to be done: the pages fault back in on first touch.
    Deflate,
    /// Descriptors themselves address the free guest memory.
    Reporting,
    /// A queue whose feature was never offered. Emptied so it cannot fill.
    Drain,
}

struct Worker {
    queue: virtio_queue::Queue,
    mem: Mem,
    interrupt: Arc<Interrupt>,
    kind: Kind,
}

impl Worker {
    fn process(&mut self) {
        let guard = self.mem.memory();
        let mut used_any = false;
        while let Some(chain) = self.queue.pop_descriptor_chain(guard.clone()) {
            let head = chain.head_index();
            match self.kind {
                Kind::Inflate => {
                    for desc in chain.clone().readable() {
                        release_pfn_array(&guard, desc.addr(), desc.len());
                    }
                }
                Kind::Reporting => {
                    // The descriptor is the free memory, not a description of
                    // it, so there is nothing to read first.
                    for desc in chain.clone() {
                        release_range(&guard, desc.addr(), u64::from(desc.len()));
                    }
                }
                Kind::Deflate | Kind::Drain => {}
            }
            if let Err(e) = self.queue.add_used(guard.deref(), head, 0) {
                log::error!("balloon: returning a descriptor chain: {e}");
                break;
            }
            used_any = true;
        }
        if used_any {
            if let Err(e) = self.interrupt.signal_queue() {
                log::error!("balloon: raising the interrupt: {e}");
            }
        }
    }
}

/// Return the pages named by an array of 32-bit page frame numbers.
fn release_pfn_array(
    guard: &vm_memory::GuestMemoryLoadGuard<vm_memory::GuestMemoryMmap<()>>,
    addr: GuestAddress,
    len: u32,
) {
    let count = (len / 4) as usize;
    let mut buf = vec![0u8; count * 4];
    if guard.read_slice(&mut buf, addr).is_err() {
        return;
    }
    for pfn in buf.chunks_exact(4) {
        let pfn = u32::from_le_bytes([pfn[0], pfn[1], pfn[2], pfn[3]]);
        release_range(
            guard,
            GuestAddress(u64::from(pfn) * BALLOON_PAGE_SIZE),
            BALLOON_PAGE_SIZE,
        );
    }
}

/// Hand a range of guest memory back to the host.
///
/// `MADV_DONTNEED` on a private anonymous mapping drops the pages; the guest
/// reading the range again gets zeroes, which is what the balloon protocol
/// promises for a page the guest said it was finished with.
fn release_range(
    guard: &vm_memory::GuestMemoryLoadGuard<vm_memory::GuestMemoryMmap<()>>,
    addr: GuestAddress,
    len: u64,
) {
    let host = match guard.get_host_address(addr) {
        Ok(p) => p,
        Err(_) => return,
    };
    // Partial pages cannot be dropped, and rounding outwards would discard a
    // page the guest is still using.
    let start = host as u64;
    let aligned = crate::layout::align_up(start, BALLOON_PAGE_SIZE);
    let trimmed = len.saturating_sub(aligned - start) & !(BALLOON_PAGE_SIZE - 1);
    if trimmed == 0 {
        return;
    }
    // SAFETY: the range is inside the guest memory mapping this process owns,
    // and MADV_DONTNEED on it only discards contents the guest has declared
    // free.
    unsafe {
        libc::madvise(
            aligned as *mut libc::c_void,
            trimmed as usize,
            libc::MADV_DONTNEED,
        );
    }
}
