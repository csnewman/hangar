//! Guest memory, and the DAX window.
//!
//! RAM is one private anonymous mapping registered as a single KVM memory
//! slot. Private rather than shared because nothing outside this process
//! reads it: the one kernel-side backend we use, vhost-vsock, reaches guest
//! memory through this process's address space, and the filesystem that would
//! otherwise need shared memory runs in here.

use std::io;
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicU64, Ordering};

use kvm_bindings::kvm_userspace_memory_region;
use kvm_ioctls::VmFd;
use vm_memory::{
    Address, GuestAddress, GuestAddressSpace, GuestMemory, GuestMemoryAtomic, GuestMemoryMmap,
    GuestMemoryRegion,
};

use crate::layout;

pub type Mem = GuestMemoryAtomic<GuestMemoryMmap<()>>;

/// The guest's RAM.
pub struct Ram {
    pub mem: Mem,
    pub size: u64,
}

impl Ram {
    /// Allocate `size` bytes of guest RAM at the architectural RAM base.
    pub fn new(size: u64) -> io::Result<Self> {
        let base = GuestAddress(layout::RAM_BASE);
        let mmap = GuestMemoryMmap::from_ranges(&[(base, size as usize)])
            .map_err(|e| io::Error::other(format!("allocating {size} bytes of guest RAM: {e}")))?;
        Ok(Self {
            mem: GuestMemoryAtomic::new(mmap),
            size,
        })
    }

    /// Hand the RAM to KVM. Slot numbers start at zero and are dense, so the
    /// DAX window takes the slot after the last RAM region.
    pub fn register(&self, vm: &VmFd, first_slot: u32) -> io::Result<u32> {
        let guard = self.mem.memory();
        let mut slot = first_slot;
        for region in guard.iter() {
            let r = kvm_userspace_memory_region {
                slot,
                guest_phys_addr: region.start_addr().raw_value(),
                memory_size: region.len(),
                userspace_addr: region.as_ptr() as u64,
                flags: 0,
            };
            // SAFETY: the region is a live mapping owned by self.mem, which
            // outlives the VM.
            unsafe { vm.set_user_memory_region(r) }
                .map_err(|e| io::Error::other(format!("registering guest RAM with KVM: {e}")))?;
            slot += 1;
        }
        Ok(slot)
    }

    /// The first address past RAM.
    pub fn end(&self) -> u64 {
        layout::RAM_BASE + self.size
    }
}

/// A region of guest physical address space that the virtio-fs device fills
/// in with mappings of host files.
///
/// The window is reserved as one `PROT_NONE` anonymous mapping and registered
/// with KVM as a single slot. Nothing in it is readable until the guest asks
/// for a mapping, at which point `map` replaces that slice of the reservation
/// with a mapping of the host file. KVM notices through the mmu notifier and
/// drops any stage-2 translation it had cached, so the guest's next access
/// faults onto the host's page cache page.
///
/// This is what makes DAX work: the guest reads file data by touching memory,
/// with no FUSE request and no copy, and several guests sharing a base image
/// share the host's page cache rather than each caching its own.
pub struct DaxWindow {
    host_addr: *mut libc::c_void,
    guest_addr: u64,
    size: u64,
    /// Bytes currently backed by a file mapping, for reporting.
    mapped: AtomicU64,
}

// SAFETY: the window is a fixed address range. `map` and `unmap` take &self
// and are safe to call concurrently because mmap on disjoint ranges is, and
// the filesystem serialises overlapping requests for a single mapping itself.
unsafe impl Send for DaxWindow {}
unsafe impl Sync for DaxWindow {}

impl DaxWindow {
    /// Reserve `size` bytes of address space for a window at `guest_addr`.
    pub fn new(guest_addr: u64, size: u64) -> io::Result<Self> {
        // SAFETY: a fresh anonymous reservation; the kernel picks the address.
        let host_addr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size as usize,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };
        if host_addr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            host_addr,
            guest_addr,
            size,
            mapped: AtomicU64::new(0),
        })
    }

    pub fn guest_addr(&self) -> u64 {
        self.guest_addr
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    /// How much of the window currently maps a host file.
    ///
    /// This is the measurement the whole design turns on: bytes here are
    /// bytes the guest reads without a FUSE request and without a second
    /// copy in its own page cache.
    pub fn mapped_bytes(&self) -> u64 {
        self.mapped.load(Ordering::Relaxed)
    }

    /// Hand the reservation to KVM as one slot.
    pub fn register(&self, vm: &VmFd, slot: u32) -> io::Result<()> {
        let r = kvm_userspace_memory_region {
            slot,
            guest_phys_addr: self.guest_addr,
            memory_size: self.size,
            userspace_addr: self.host_addr as u64,
            flags: 0,
        };
        // SAFETY: the reservation lives as long as this struct, which the VM
        // holds for its lifetime.
        unsafe { vm.set_user_memory_region(r) }
            .map_err(|e| io::Error::other(format!("registering the DAX window with KVM: {e}")))
    }

    /// Map `len` bytes at `file_offset` of `fd` into the window at
    /// `window_offset`.
    pub fn map(
        &self,
        window_offset: u64,
        file_offset: u64,
        len: u64,
        writable: bool,
        fd: RawFd,
    ) -> io::Result<()> {
        self.check_range(window_offset, len)?;
        let mut prot = libc::PROT_READ;
        if writable {
            prot |= libc::PROT_WRITE;
        }
        // SAFETY: the range is inside the reservation, which this call
        // replaces rather than extends. MAP_FIXED over our own reservation
        // cannot unmap anything else.
        let addr = unsafe {
            libc::mmap(
                self.host_addr.add(window_offset as usize),
                len as usize,
                prot,
                libc::MAP_SHARED | libc::MAP_FIXED,
                fd,
                file_offset as i64,
            )
        };
        if addr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        self.mapped.fetch_add(len, Ordering::Relaxed);
        Ok(())
    }

    /// Return `len` bytes at `window_offset` to being unbacked.
    ///
    /// The range keeps its reservation, so the window never develops a hole
    /// another allocation could land in.
    pub fn unmap(&self, window_offset: u64, len: u64) -> io::Result<()> {
        self.check_range(window_offset, len)?;
        // SAFETY: as in map -- the range is inside our own reservation.
        let addr = unsafe {
            libc::mmap(
                self.host_addr.add(window_offset as usize),
                len as usize,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };
        if addr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        self.mapped.fetch_sub(
            len.min(self.mapped.load(Ordering::Relaxed)),
            Ordering::Relaxed,
        );
        Ok(())
    }

    fn check_range(&self, offset: u64, len: u64) -> io::Result<()> {
        match offset.checked_add(len) {
            Some(end) if end <= self.size => Ok(()),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "a mapping of {len} bytes at offset {offset} does not fit \
                     in a {} byte DAX window",
                    self.size
                ),
            )),
        }
    }
}

impl Drop for DaxWindow {
    fn drop(&mut self) {
        // SAFETY: the reservation was made by this struct and nothing else
        // holds a pointer into it once the VM has stopped.
        unsafe { libc::munmap(self.host_addr, self.size as usize) };
    }
}

/// Where the DAX window sits: above RAM, aligned clear of it.
pub fn dax_window_base(ram_end: u64) -> u64 {
    layout::align_up(ram_end, layout::DAX_ALIGN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_refuses_a_mapping_that_does_not_fit() {
        let w = DaxWindow::new(0x1_0000_0000, 2 * 1024 * 1024).unwrap();
        assert!(w.check_range(0, w.size()).is_ok());
        assert!(w.check_range(4096, w.size()).is_err());
        assert!(w.check_range(0, w.size() + 1).is_err());
        // An offset and length that overflow must be refused rather than
        // wrapping into a range that looks valid.
        assert!(w.check_range(u64::MAX, 4096).is_err());
    }

    #[test]
    fn mapping_a_file_makes_it_readable_through_the_window() {
        use std::io::{Seek, SeekFrom, Write};
        use std::os::unix::io::AsRawFd;

        let page = 4096u64;
        let w = DaxWindow::new(0x1_0000_0000, 2 * page).unwrap();

        let mut f = tempfile().unwrap();
        f.write_all(&[0xab; 4096]).unwrap();
        f.seek(SeekFrom::Start(0)).unwrap();

        w.map(page, 0, page, false, f.as_raw_fd()).unwrap();
        assert_eq!(w.mapped_bytes(), page);

        // SAFETY: the page was just mapped read-only from a file of at least
        // that length.
        let mapped = unsafe {
            std::slice::from_raw_parts(w.host_addr.add(page as usize) as *const u8, page as usize)
        };
        assert!(mapped.iter().all(|b| *b == 0xab));

        w.unmap(page, page).unwrap();
        assert_eq!(w.mapped_bytes(), 0);
    }

    /// An unlinked file to map, without pulling in a crate for it.
    fn tempfile() -> io::Result<std::fs::File> {
        let path = std::env::temp_dir().join(format!("hangar-vmm-test-{}", std::process::id()));
        let f = std::fs::File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)?;
        std::fs::remove_file(&path)?;
        Ok(f)
    }

    #[test]
    fn the_window_starts_above_ram_on_an_alignment_boundary() {
        let base = dax_window_base(layout::RAM_BASE + 2047 * 1024 * 1024);
        assert!(base >= layout::RAM_BASE + 2047 * 1024 * 1024);
        assert_eq!(base % layout::DAX_ALIGN, 0);
    }
}
