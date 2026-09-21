//! The guest's physical address map.
//!
//! Every address here is invented by this VMM and told to the guest through
//! the device tree, so the only hard constraints are that devices sit below
//! RAM and that the kernel's load address is 2 MiB aligned.
//!
//! The layout follows the one an arm64 guest usually sees, which keeps the
//! guest kernel on a well-trodden path even though it is being told rather
//! than discovering.

/// GIC distributor.
pub const GIC_DIST_BASE: u64 = 0x0800_0000;
pub const GIC_DIST_SIZE: u64 = 0x0001_0000;

/// GIC redistributors, one 128 KiB frame pair per vCPU.
pub const GIC_REDIST_BASE: u64 = 0x080a_0000;
pub const GIC_REDIST_SIZE_PER_CPU: u64 = 0x0002_0000;

/// PL011 serial port.
pub const SERIAL_BASE: u64 = 0x0900_0000;
pub const SERIAL_SIZE: u64 = 0x0000_1000;
/// SPI number, before the 32 SGIs and PPIs are added to reach an INTID.
pub const SERIAL_SPI: u32 = 1;

/// virtio-mmio devices, back to back.
pub const VIRTIO_MMIO_BASE: u64 = 0x0a00_0000;
pub const VIRTIO_MMIO_SIZE: u64 = 0x0000_0200;
pub const VIRTIO_MMIO_SPI_BASE: u32 = 16;
/// Enough for blk, net, fs, vsock, balloon, mem, rng and gpu.
pub const VIRTIO_MMIO_MAX_DEVICES: usize = 16;

/// Guest RAM. Devices are all below this.
pub const RAM_BASE: u64 = 0x4000_0000;

/// The kernel is loaded here: RAM base plus a 2 MiB gap, which satisfies the
/// arm64 boot protocol's alignment requirement with room to spare.
pub const KERNEL_OFFSET: u64 = 0x0020_0000;

/// Space set aside at the top of RAM for the device tree.
pub const FDT_MAX_SIZE: u64 = 0x0020_0000;

/// How many SPIs the GIC is created with. SGIs and PPIs occupy the first 32
/// INTIDs, and KVM wants a multiple of 32.
pub const NUM_IRQS: u32 = 128;

/// Alignment of the DAX window.
///
/// The guest maps it with `devm_memremap_pages`, which works in memory
/// subsections; 1 GiB is far past any subsection size and keeps the window
/// clear of RAM under any page size.
pub const DAX_ALIGN: u64 = 1 << 30;

/// Round `addr` up to the next multiple of `align`, which must be a power of
/// two.
pub const fn align_up(addr: u64, align: u64) -> u64 {
    (addr + align - 1) & !(align - 1)
}

/// The MMIO address and SPI of the nth virtio device.
pub fn virtio_mmio_slot(index: usize) -> (u64, u32) {
    (
        VIRTIO_MMIO_BASE + (index as u64) * VIRTIO_MMIO_SIZE,
        VIRTIO_MMIO_SPI_BASE + index as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn align_up_rounds_to_the_next_boundary() {
        assert_eq!(align_up(0, 0x1000), 0);
        assert_eq!(align_up(1, 0x1000), 0x1000);
        assert_eq!(align_up(0x1000, 0x1000), 0x1000);
        assert_eq!(align_up(0x1001, 0x1000), 0x2000);
    }

    #[test]
    fn virtio_slots_do_not_overlap_and_are_below_ram() {
        let mut previous_end = VIRTIO_MMIO_BASE;
        let mut previous_spi = VIRTIO_MMIO_SPI_BASE;
        for i in 0..VIRTIO_MMIO_MAX_DEVICES {
            let (addr, spi) = virtio_mmio_slot(i);
            assert!(addr >= previous_end || i == 0);
            assert!(addr + VIRTIO_MMIO_SIZE <= RAM_BASE);
            if i > 0 {
                assert!(spi > previous_spi);
            }
            // An SPI has to reach a real interrupt id once the 32 software
            // generated and private ones are counted.
            assert!(spi + 32 < NUM_IRQS);
            previous_end = addr + VIRTIO_MMIO_SIZE;
            previous_spi = spi;
        }
    }

    #[test]
    fn devices_sit_below_ram_and_do_not_collide() {
        let ranges = [
            (GIC_DIST_BASE, GIC_DIST_SIZE),
            (GIC_REDIST_BASE, GIC_REDIST_SIZE_PER_CPU * 8),
            (SERIAL_BASE, SERIAL_SIZE),
            (
                VIRTIO_MMIO_BASE,
                VIRTIO_MMIO_SIZE * VIRTIO_MMIO_MAX_DEVICES as u64,
            ),
        ];
        for (i, (base, len)) in ranges.iter().enumerate() {
            assert!(base + len <= RAM_BASE, "range {i} runs into RAM");
            for (j, (other, other_len)) in ranges.iter().enumerate() {
                if i != j {
                    assert!(
                        base + len <= *other || other + other_len <= *base,
                        "ranges {i} and {j} overlap"
                    );
                }
            }
        }
    }
}
