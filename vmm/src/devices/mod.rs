//! Devices the guest reaches through memory-mapped I/O.

pub mod serial;
pub mod virtio;

use std::sync::{Arc, Mutex};

/// A device occupying a range of guest physical addresses.
///
/// Offsets are relative to the device's base, so a device does not know where
/// it was placed.
pub trait MmioDevice: Send {
    fn read(&mut self, offset: u64, data: &mut [u8]);
    fn write(&mut self, offset: u64, data: &[u8]);
}

struct Entry {
    base: u64,
    len: u64,
    device: Arc<Mutex<dyn MmioDevice>>,
}

/// Routes a guest MMIO access to the device that owns the address.
///
/// vCPU threads share one of these, so lookup takes `&self` and the device
/// itself carries the lock. Ranges are kept sorted, which makes a lookup a
/// binary search rather than a scan -- it happens on every MMIO exit.
#[derive(Default)]
pub struct Bus {
    entries: Vec<Entry>,
}

impl Bus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Place a device at `base`, covering `len` bytes.
    pub fn insert(
        &mut self,
        base: u64,
        len: u64,
        device: Arc<Mutex<dyn MmioDevice>>,
    ) -> Result<(), String> {
        if len == 0 {
            return Err("a device cannot occupy zero bytes".into());
        }
        if let Some(e) = self
            .entries
            .iter()
            .find(|e| base < e.base + e.len && e.base < base + len)
        {
            return Err(format!(
                "a device at {:#x} would overlap the one at {:#x}",
                base, e.base
            ));
        }
        self.entries.push(Entry { base, len, device });
        self.entries.sort_by_key(|e| e.base);
        Ok(())
    }

    fn find(&self, addr: u64) -> Option<(u64, &Arc<Mutex<dyn MmioDevice>>)> {
        let i = match self.entries.binary_search_by(|e| e.base.cmp(&addr)) {
            Ok(i) => i,
            Err(0) => return None,
            Err(i) => i - 1,
        };
        let e = &self.entries[i];
        (addr < e.base + e.len).then(|| (addr - e.base, &e.device))
    }

    /// Serve a read. An access to an address no device claims reads as zero,
    /// which is what a guest touching a hole would see on real hardware.
    pub fn read(&self, addr: u64, data: &mut [u8]) {
        match self.find(addr) {
            Some((offset, dev)) => dev.lock().unwrap().read(offset, data),
            None => data.fill(0),
        }
    }

    /// Serve a write. A write to a hole is dropped.
    pub fn write(&self, addr: u64, data: &[u8]) {
        if let Some((offset, dev)) = self.find(addr) {
            dev.lock().unwrap().write(offset, data);
        }
    }
}
