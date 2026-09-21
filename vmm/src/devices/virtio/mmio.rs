//! The virtio-mmio transport.
//!
//! MMIO rather than PCI because everything Hangar needs is reachable over it,
//! including the shared memory registers the filesystem's DAX window is
//! advertised through (`SHM_SEL` and friends, added in virtio 1.2 and
//! implemented by Linux's `virtio_mmio.c`). That removes a PCI host bridge,
//! configuration space, a BAR allocator and MSI-X routing from this VMM, none
//! of which a fixed set of eight devices needs.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use virtio_queue::{Queue, QueueT};
use vmm_sys_util::eventfd::EventFd;

use super::{ActiveQueue, Interrupt, ShmRegion, VirtioDevice, VIRTIO_RING_F_EVENT_IDX};
use crate::devices::MmioDevice;
use crate::memory::Mem;

const MAGIC_VALUE: u64 = 0x000;
const VERSION: u64 = 0x004;
const DEVICE_ID: u64 = 0x008;
const VENDOR_ID: u64 = 0x00c;
const DEVICE_FEATURES: u64 = 0x010;
const DEVICE_FEATURES_SEL: u64 = 0x014;
const DRIVER_FEATURES: u64 = 0x020;
const DRIVER_FEATURES_SEL: u64 = 0x024;
const QUEUE_SEL: u64 = 0x030;
const QUEUE_NUM_MAX: u64 = 0x034;
const QUEUE_NUM: u64 = 0x038;
const QUEUE_READY: u64 = 0x044;
/// Where the guest writes a queue index to say it has work.
///
/// KVM is given a descriptor per queue matched against a write of that
/// index here, so the kick is handled in the kernel and the vCPU keeps
/// running.
pub const NOTIFY_OFFSET: u64 = 0x050;
const QUEUE_NOTIFY: u64 = NOTIFY_OFFSET;
const INTERRUPT_STATUS: u64 = 0x060;
const INTERRUPT_ACK: u64 = 0x064;
const STATUS: u64 = 0x070;
const QUEUE_DESC_LOW: u64 = 0x080;
const QUEUE_DESC_HIGH: u64 = 0x084;
const QUEUE_AVAIL_LOW: u64 = 0x090;
const QUEUE_AVAIL_HIGH: u64 = 0x094;
const QUEUE_USED_LOW: u64 = 0x0a0;
const QUEUE_USED_HIGH: u64 = 0x0a4;
const SHM_SEL: u64 = 0x0ac;
const SHM_LEN_LOW: u64 = 0x0b0;
const SHM_LEN_HIGH: u64 = 0x0b4;
const SHM_BASE_LOW: u64 = 0x0b8;
const SHM_BASE_HIGH: u64 = 0x0bc;
const CONFIG_GENERATION: u64 = 0x0fc;
const CONFIG: u64 = 0x100;

/// "virt", little endian, which is how a driver recognises the register block.
const MAGIC: u32 = 0x7472_6976;
/// Version 2 is the modern layout: no guest page size, no queue PFN.
const VERSION_MODERN: u32 = 2;
/// Any value identifies the implementation; nothing keys off it.
const VENDOR: u32 = 0x484e_4752;

/// Device status bits.
const STATUS_ACKNOWLEDGE: u32 = 1;
const STATUS_DRIVER: u32 = 2;
const STATUS_DRIVER_OK: u32 = 4;
const STATUS_FEATURES_OK: u32 = 8;
const STATUS_FAILED: u32 = 128;

/// A device's register block.
pub struct Transport {
    device: Arc<Mutex<dyn VirtioDevice>>,
    mem: Mem,
    interrupt: Arc<Interrupt>,

    queues: Vec<Queue>,
    kicks: Vec<EventFd>,
    queue_select: u32,

    device_features_select: u32,
    driver_features_select: u32,
    acked_features: u64,

    status: u32,
    shm_select: u32,
    config_generation: Arc<AtomicU32>,
    activated: bool,
}

impl Transport {
    /// Build a register block for `device`.
    ///
    /// `kicks` must hold one eventfd per queue, already registered with KVM
    /// as an ioeventfd matching a write of that queue's index to
    /// `QUEUE_NOTIFY`, so the guest's kick never becomes an exit.
    pub fn new(
        device: Arc<Mutex<dyn VirtioDevice>>,
        mem: Mem,
        interrupt: Arc<Interrupt>,
        kicks: Vec<EventFd>,
    ) -> Result<Self, String> {
        let max_sizes = device.lock().unwrap().queue_max_sizes();
        if kicks.len() != max_sizes.len() {
            return Err(format!(
                "the device has {} queues but {} kick descriptors were given",
                max_sizes.len(),
                kicks.len()
            ));
        }
        let mut queues = Vec::with_capacity(max_sizes.len());
        for size in max_sizes {
            queues.push(Queue::new(size).map_err(|e| format!("creating a queue: {e}"))?);
        }
        Ok(Self {
            device,
            mem,
            interrupt,
            queues,
            kicks,
            queue_select: 0,
            device_features_select: 0,
            driver_features_select: 0,
            acked_features: 0,
            status: 0,
            shm_select: 0,
            config_generation: Arc::new(AtomicU32::new(0)),
            activated: false,
        })
    }

    fn selected_queue(&mut self) -> Option<&mut Queue> {
        self.queues.get_mut(self.queue_select as usize)
    }

    fn shm(&self) -> Option<ShmRegion> {
        self.device
            .lock()
            .unwrap()
            .shm_region()
            .filter(|r| u32::from(r.id) == self.shm_select)
    }

    /// Hand the queues to the device, once the driver says it is ready.
    fn activate(&mut self) {
        if self.activated {
            return;
        }
        let ready: Vec<usize> = self
            .queues
            .iter()
            .enumerate()
            .filter(|(_, q)| q.ready())
            .map(|(i, _)| i)
            .collect();
        if ready.is_empty() {
            log::warn!("driver set DRIVER_OK with no queue ready");
            return;
        }

        // Only the queues the driver enabled are handed over: a device with
        // optional queues -- the balloon's reporting queue, for one -- is
        // told which ones exist by which ones the driver set up.
        let mut active = Vec::with_capacity(ready.len());
        for i in ready {
            // The device takes the queue away, leaving a fresh one of the
            // same shape behind. It is not ready, so nothing will use it
            // unless the driver resets and sets it up again.
            let replacement = match Queue::new(self.queues[i].max_size()) {
                Ok(q) => q,
                Err(e) => {
                    log::error!("replacing queue {i}: {e}");
                    self.status |= STATUS_FAILED;
                    return;
                }
            };
            let mut queue = std::mem::replace(&mut self.queues[i], replacement);
            queue.set_event_idx(self.acked_features & VIRTIO_RING_F_EVENT_IDX != 0);
            let kick = match self.kicks[i].try_clone() {
                Ok(k) => k,
                Err(e) => {
                    log::error!("cloning the kick descriptor for queue {i}: {e}");
                    self.status |= STATUS_FAILED;
                    return;
                }
            };
            active.push(ActiveQueue {
                index: i,
                queue,
                kick,
            });
        }

        let mut device = self.device.lock().unwrap();
        log::debug!(
            "activating device type {} with queues {:?}",
            device.device_type(),
            active.iter().map(|q| q.index).collect::<Vec<_>>()
        );
        device.ack_features(self.acked_features);
        if let Err(e) = device.activate(self.mem.clone(), self.interrupt.clone(), active) {
            log::error!("activating the device: {e}");
            self.status |= STATUS_FAILED;
            return;
        }
        self.activated = true;
    }

    fn reset(&mut self) {
        self.status = 0;
        self.acked_features = 0;
        self.interrupt.ack(u32::MAX);
        for q in &mut self.queues {
            q.set_ready(false);
        }
    }
}

impl MmioDevice for Transport {
    fn read(&mut self, offset: u64, data: &mut [u8]) {
        if offset >= CONFIG {
            self.device
                .lock()
                .unwrap()
                .read_config(offset - CONFIG, data);
            return;
        }

        let value: u32 = match offset {
            MAGIC_VALUE => MAGIC,
            VERSION => VERSION_MODERN,
            DEVICE_ID => self.device.lock().unwrap().device_type(),
            VENDOR_ID => VENDOR,
            DEVICE_FEATURES => {
                let device = self.device.lock().unwrap();
                let features = device.features() | device.transport_features();
                drop(device);
                match self.device_features_select {
                    0 => features as u32,
                    1 => (features >> 32) as u32,
                    _ => 0,
                }
            }
            QUEUE_NUM_MAX => self
                .queues
                .get(self.queue_select as usize)
                .map_or(0, |q| u32::from(q.max_size())),
            QUEUE_READY => self.selected_queue().map_or(0, |q| u32::from(q.ready())),
            INTERRUPT_STATUS => self.interrupt.status(),
            STATUS => self.status,
            // A length of all ones means "no such region", which is how the
            // driver stops looking.
            SHM_LEN_LOW => self.shm().map_or(u32::MAX, |r| r.size as u32),
            SHM_LEN_HIGH => self.shm().map_or(u32::MAX, |r| (r.size >> 32) as u32),
            SHM_BASE_LOW => self.shm().map_or(u32::MAX, |r| r.addr as u32),
            SHM_BASE_HIGH => self.shm().map_or(u32::MAX, |r| (r.addr >> 32) as u32),
            CONFIG_GENERATION => self.config_generation.load(Ordering::SeqCst),
            _ => 0,
        };
        write_le(data, value);
    }

    fn write(&mut self, offset: u64, data: &[u8]) {
        if offset >= CONFIG {
            self.device
                .lock()
                .unwrap()
                .write_config(offset - CONFIG, data);
            return;
        }

        let value = read_le(data);
        match offset {
            DEVICE_FEATURES_SEL => self.device_features_select = value,
            DRIVER_FEATURES => {
                let shift = if self.driver_features_select == 1 {
                    32
                } else {
                    0
                };
                self.acked_features |= u64::from(value) << shift;
            }
            DRIVER_FEATURES_SEL => self.driver_features_select = value,
            QUEUE_SEL => self.queue_select = value,
            QUEUE_NUM => {
                if let Some(q) = self.selected_queue() {
                    q.set_size(value as u16);
                }
            }
            QUEUE_READY => {
                let ready = value == 1;
                if let Some(q) = self.selected_queue() {
                    q.set_ready(ready);
                }
            }
            QUEUE_DESC_LOW => {
                self.set_queue_addr(|q, v| q.set_desc_table_address(Some(v), None), value)
            }
            QUEUE_DESC_HIGH => {
                self.set_queue_addr(|q, v| q.set_desc_table_address(None, Some(v)), value)
            }
            QUEUE_AVAIL_LOW => {
                self.set_queue_addr(|q, v| q.set_avail_ring_address(Some(v), None), value)
            }
            QUEUE_AVAIL_HIGH => {
                self.set_queue_addr(|q, v| q.set_avail_ring_address(None, Some(v)), value)
            }
            QUEUE_USED_LOW => {
                self.set_queue_addr(|q, v| q.set_used_ring_address(Some(v), None), value)
            }
            QUEUE_USED_HIGH => {
                self.set_queue_addr(|q, v| q.set_used_ring_address(None, Some(v)), value)
            }
            // Reached only if KVM has no ioeventfd for this queue, which is
            // the case for a queue index the driver invented.
            QUEUE_NOTIFY => {
                if let Some(kick) = self.kicks.get(value as usize) {
                    let _ = kick.write(1);
                }
            }
            INTERRUPT_ACK => self.interrupt.ack(value),
            SHM_SEL => self.shm_select = value,
            STATUS => {
                if value == 0 {
                    self.reset();
                    return;
                }
                self.status = value;
                if value & STATUS_DRIVER_OK != 0
                    && value & STATUS_FEATURES_OK != 0
                    && value & STATUS_DRIVER != 0
                    && value & STATUS_ACKNOWLEDGE != 0
                {
                    self.activate();
                }
            }
            _ => {}
        }
    }
}

impl Transport {
    fn set_queue_addr<F: Fn(&mut Queue, u32)>(&mut self, f: F, value: u32) {
        if let Some(q) = self.selected_queue() {
            f(q, value);
        }
    }
}

fn read_le(data: &[u8]) -> u32 {
    let mut buf = [0u8; 4];
    let n = data.len().min(4);
    buf[..n].copy_from_slice(&data[..n]);
    u32::from_le_bytes(buf)
}

fn write_le(data: &mut [u8], value: u32) {
    let bytes = value.to_le_bytes();
    let n = data.len().min(4);
    data[..n].copy_from_slice(&bytes[..n]);
    if data.len() > 4 {
        data[4..].fill(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::virtio::VIRTIO_F_VERSION_1;
    use crate::memory::Ram;

    /// Records what the transport hands it, and offers one shared memory
    /// region so the registers that advertise the DAX window can be read.
    struct Recorder {
        activated: Arc<Mutex<Option<Vec<usize>>>>,
        acked: Arc<Mutex<u64>>,
    }

    impl VirtioDevice for Recorder {
        fn device_type(&self) -> u32 {
            super::super::TYPE_FS
        }
        fn queue_max_sizes(&self) -> Vec<u16> {
            vec![64, 64]
        }
        fn features(&self) -> u64 {
            1 << 3
        }
        fn ack_features(&mut self, value: u64) {
            *self.acked.lock().unwrap() = value;
        }
        fn shm_region(&self) -> Option<ShmRegion> {
            Some(ShmRegion {
                id: 0,
                addr: 0xc000_0000,
                size: 0x4000_0000,
            })
        }
        fn activate(
            &mut self,
            _mem: crate::memory::Mem,
            _interrupt: Arc<Interrupt>,
            queues: Vec<ActiveQueue>,
        ) -> std::io::Result<()> {
            *self.activated.lock().unwrap() = Some(queues.iter().map(|q| q.index).collect());
            Ok(())
        }
    }

    /// What a recorder saw: the queue indices it was activated with, and
    /// the features the driver acknowledged.
    type Seen = (Transport, Arc<Mutex<Option<Vec<usize>>>>, Arc<Mutex<u64>>);

    fn transport() -> Seen {
        let activated = Arc::new(Mutex::new(None));
        let acked = Arc::new(Mutex::new(0));
        let device = Arc::new(Mutex::new(Recorder {
            activated: activated.clone(),
            acked: acked.clone(),
        }));
        let ram = Ram::new(16 * 1024 * 1024).unwrap();
        let interrupt = Arc::new(Interrupt::new(
            vmm_sys_util::eventfd::EventFd::new(libc::EFD_NONBLOCK).unwrap(),
        ));
        let kicks = (0..2)
            .map(|_| vmm_sys_util::eventfd::EventFd::new(libc::EFD_NONBLOCK).unwrap())
            .collect();
        (
            Transport::new(device, ram.mem, interrupt, kicks).unwrap(),
            activated,
            acked,
        )
    }

    fn read32(t: &mut Transport, offset: u64) -> u32 {
        let mut buf = [0u8; 4];
        t.read(offset, &mut buf);
        u32::from_le_bytes(buf)
    }

    fn write32(t: &mut Transport, offset: u64, value: u32) {
        t.write(offset, &value.to_le_bytes());
    }

    #[test]
    fn a_driver_recognises_the_register_block() {
        let (mut t, _, _) = transport();
        assert_eq!(read32(&mut t, MAGIC_VALUE), MAGIC);
        assert_eq!(read32(&mut t, VERSION), VERSION_MODERN);
        assert_eq!(read32(&mut t, DEVICE_ID), super::super::TYPE_FS);
        assert_eq!(read32(&mut t, QUEUE_NUM_MAX), 64);
    }

    #[test]
    fn the_transport_adds_its_own_features_to_the_device_s() {
        use crate::devices::virtio::TRANSPORT_FEATURES;
        let (mut t, _, _) = transport();
        write32(&mut t, DEVICE_FEATURES_SEL, 0);
        assert_eq!(
            read32(&mut t, DEVICE_FEATURES),
            (1 << 3) | TRANSPORT_FEATURES as u32
        );
        write32(&mut t, DEVICE_FEATURES_SEL, 1);
        assert_eq!(
            read32(&mut t, DEVICE_FEATURES),
            (VIRTIO_F_VERSION_1 >> 32) as u32
        );
    }

    #[test]
    fn the_shared_memory_registers_describe_the_window() {
        let (mut t, _, _) = transport();
        write32(&mut t, SHM_SEL, 0);
        assert_eq!(read32(&mut t, SHM_LEN_LOW), 0x4000_0000);
        assert_eq!(read32(&mut t, SHM_LEN_HIGH), 0);
        assert_eq!(read32(&mut t, SHM_BASE_LOW), 0xc000_0000);
        assert_eq!(read32(&mut t, SHM_BASE_HIGH), 0);

        // A region the device does not have reads as a length of all ones,
        // which is how the driver stops looking.
        write32(&mut t, SHM_SEL, 1);
        assert_eq!(read32(&mut t, SHM_LEN_LOW), u32::MAX);
        assert_eq!(read32(&mut t, SHM_LEN_HIGH), u32::MAX);
    }

    /// Set up one queue the way a driver does, leaving it ready.
    fn ready_queue(t: &mut Transport, index: u32, desc: u64) {
        write32(t, QUEUE_SEL, index);
        write32(t, QUEUE_NUM, 64);
        write32(t, QUEUE_DESC_LOW, desc as u32);
        write32(t, QUEUE_DESC_HIGH, (desc >> 32) as u32);
        write32(t, QUEUE_AVAIL_LOW, (desc + 0x1000) as u32);
        write32(t, QUEUE_AVAIL_HIGH, ((desc + 0x1000) >> 32) as u32);
        write32(t, QUEUE_USED_LOW, (desc + 0x2000) as u32);
        write32(t, QUEUE_USED_HIGH, ((desc + 0x2000) >> 32) as u32);
        write32(t, QUEUE_READY, 1);
    }

    #[test]
    fn the_device_is_activated_once_the_driver_is_ready() {
        let (mut t, activated, acked) = transport();

        write32(&mut t, STATUS, STATUS_ACKNOWLEDGE);
        write32(&mut t, STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);
        write32(&mut t, DRIVER_FEATURES_SEL, 0);
        write32(&mut t, DRIVER_FEATURES, 1 << 3);
        write32(&mut t, DRIVER_FEATURES_SEL, 1);
        write32(&mut t, DRIVER_FEATURES, (VIRTIO_F_VERSION_1 >> 32) as u32);
        write32(
            &mut t,
            STATUS,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK,
        );

        ready_queue(&mut t, 0, crate::layout::RAM_BASE + 0x10_0000);
        assert!(activated.lock().unwrap().is_none(), "activated too early");

        write32(
            &mut t,
            STATUS,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK,
        );

        // Only the queue the driver set up is handed over, and it carries the
        // index the driver used rather than its position in the list.
        assert_eq!(activated.lock().unwrap().as_deref(), Some(&[0usize][..]));
        assert_eq!(*acked.lock().unwrap(), (1 << 3) | VIRTIO_F_VERSION_1);
        // The event index was not acknowledged, so the queue was handed over
        // without it.
    }

    #[test]
    fn only_the_queues_the_driver_readied_are_handed_over() {
        let (mut t, activated, _) = transport();
        write32(
            &mut t,
            STATUS,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK,
        );
        ready_queue(&mut t, 1, crate::layout::RAM_BASE + 0x20_0000);
        write32(
            &mut t,
            STATUS,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK,
        );
        assert_eq!(activated.lock().unwrap().as_deref(), Some(&[1usize][..]));
    }

    #[test]
    fn the_interrupt_status_reflects_a_signal_and_clears_on_acknowledgement() {
        let (mut t, _, _) = transport();
        assert_eq!(read32(&mut t, INTERRUPT_STATUS), 0);
        t.interrupt.signal_queue().unwrap();
        assert_eq!(read32(&mut t, INTERRUPT_STATUS), super::super::INT_VRING);
        write32(&mut t, INTERRUPT_ACK, super::super::INT_VRING);
        assert_eq!(read32(&mut t, INTERRUPT_STATUS), 0);
    }

    #[test]
    fn a_reset_puts_every_queue_back() {
        let (mut t, _, _) = transport();
        write32(
            &mut t,
            STATUS,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK,
        );
        ready_queue(&mut t, 0, crate::layout::RAM_BASE + 0x10_0000);
        assert_eq!(read32(&mut t, QUEUE_READY), 1);

        write32(&mut t, STATUS, 0);
        assert_eq!(read32(&mut t, STATUS), 0);
        write32(&mut t, QUEUE_SEL, 0);
        assert_eq!(read32(&mut t, QUEUE_READY), 0);
    }
}
