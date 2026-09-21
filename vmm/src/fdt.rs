//! The device tree handed to the guest kernel.
//!
//! An arm64 guest discovers nothing by probing: RAM, CPUs, the interrupt
//! controller, the timer, the serial port and every virtio device are all
//! described here, and the kernel believes it. That is why this VMM needs no
//! firmware and no ACPI.

use std::ffi::CString;
use std::io;

use vm_fdt::FdtWriter;
use vm_memory::{Bytes, GuestAddress, GuestAddressSpace};

use crate::gic::Gic;
use crate::layout;
use crate::memory::Mem;

/// Phandles. Any distinct non-zero numbers will do; these are referenced by
/// `interrupt-parent` and the PL011's `clocks`.
const PHANDLE_GIC: u32 = 1;
const PHANDLE_CLOCK: u32 = 2;

/// Interrupt specifier cells, as the GIC binding defines them.
const IRQ_SPI: u32 = 0;
const IRQ_PPI: u32 = 1;
const IRQ_EDGE_RISING: u32 = 1;
const IRQ_LEVEL_HIGH: u32 = 4;

/// One virtio-mmio device, as the tree describes it.
pub struct VirtioNode {
    pub addr: u64,
    pub size: u64,
    pub spi: u32,
}

pub struct Params<'a> {
    pub cmdline: &'a str,
    pub mem: &'a Mem,
    pub ram_base: u64,
    pub ram_size: u64,
    pub cpus: u8,
    pub gic: &'a Gic,
    pub initrd: Option<(u64, u64)>,
    pub virtio: &'a [VirtioNode],
}

/// Build the tree and write it to the top of RAM, returning its address.
pub fn write(p: &Params) -> io::Result<GuestAddress> {
    let blob = build(p).map_err(|e| io::Error::other(format!("building the device tree: {e}")))?;
    if blob.len() as u64 > layout::FDT_MAX_SIZE {
        return Err(io::Error::other(format!(
            "the device tree is {} bytes, more than the {} reserved for it",
            blob.len(),
            layout::FDT_MAX_SIZE
        )));
    }

    // The kernel is told where the tree is, so it can go anywhere it will not
    // be overwritten. The top of RAM is past the kernel and past any
    // initramfs, and the arm64 boot protocol requires it be reachable in the
    // initial identity map, which RAM is.
    let addr = GuestAddress(p.ram_base + p.ram_size - layout::FDT_MAX_SIZE);
    let guard = p.mem.memory();
    guard
        .write_slice(&blob, addr)
        .map_err(|e| io::Error::other(format!("writing the device tree to guest memory: {e}")))?;
    Ok(addr)
}

fn build(p: &Params) -> Result<Vec<u8>, vm_fdt::Error> {
    let mut fdt = FdtWriter::new()?;

    let root = fdt.begin_node("")?;
    fdt.property_string("compatible", "linux,dummy-virt")?;
    fdt.property_u32("#address-cells", 2)?;
    fdt.property_u32("#size-cells", 2)?;
    fdt.property_u32("interrupt-parent", PHANDLE_GIC)?;

    cpus(&mut fdt, p.cpus)?;
    memory(&mut fdt, p.ram_base, p.ram_size)?;
    chosen(&mut fdt, p.cmdline, p.initrd)?;
    gic(&mut fdt, p.gic)?;
    timer(&mut fdt)?;
    clock(&mut fdt)?;
    psci(&mut fdt)?;
    serial(&mut fdt)?;
    for node in p.virtio {
        virtio(&mut fdt, node)?;
    }

    fdt.end_node(root)?;
    fdt.finish()
}

fn cpus(fdt: &mut FdtWriter, count: u8) -> Result<(), vm_fdt::Error> {
    let cpus = fdt.begin_node("cpus")?;
    fdt.property_u32("#address-cells", 1)?;
    fdt.property_u32("#size-cells", 0)?;
    for i in 0..count {
        let cpu = fdt.begin_node(&format!("cpu@{i:x}"))?;
        fdt.property_string("device_type", "cpu")?;
        fdt.property_string("compatible", "arm,arm-v8")?;
        // PSCI is how secondary processors are started; without it the guest
        // brings up one vCPU and leaves the rest parked.
        fdt.property_string("enable-method", "psci")?;
        fdt.property_u32("reg", u32::from(i))?;
        fdt.end_node(cpu)?;
    }
    fdt.end_node(cpus)
}

fn memory(fdt: &mut FdtWriter, base: u64, size: u64) -> Result<(), vm_fdt::Error> {
    let node = fdt.begin_node(&format!("memory@{base:x}"))?;
    fdt.property_string("device_type", "memory")?;
    fdt.property_array_u64("reg", &[base, size])?;
    fdt.end_node(node)
}

fn chosen(
    fdt: &mut FdtWriter,
    cmdline: &str,
    initrd: Option<(u64, u64)>,
) -> Result<(), vm_fdt::Error> {
    let node = fdt.begin_node("chosen")?;
    let c = CString::new(cmdline).unwrap_or_default();
    fdt.property_string("bootargs", c.to_str().unwrap_or(""))?;
    if let Some((addr, size)) = initrd {
        fdt.property_u64("linux,initrd-start", addr)?;
        fdt.property_u64("linux,initrd-end", addr + size)?;
    }
    fdt.end_node(node)
}

fn gic(fdt: &mut FdtWriter, g: &Gic) -> Result<(), vm_fdt::Error> {
    let node = fdt.begin_node("intc")?;
    fdt.property_string("compatible", "arm,gic-v3")?;
    fdt.property_null("interrupt-controller")?;
    // Three cells: type, number, flags.
    fdt.property_u32("#interrupt-cells", 3)?;
    fdt.property_array_u64("reg", &g.fdt_reg())?;
    fdt.property_u32("#address-cells", 2)?;
    fdt.property_u32("#size-cells", 2)?;
    fdt.property_null("ranges")?;
    fdt.property_u32("phandle", PHANDLE_GIC)?;
    fdt.end_node(node)
}

fn timer(fdt: &mut FdtWriter) -> Result<(), vm_fdt::Error> {
    // Secure, non-secure, virtual and hypervisor timers, in the order the
    // binding fixes.
    let mut cells = Vec::new();
    for irq in [13u32, 14, 11, 10] {
        cells.extend_from_slice(&[IRQ_PPI, irq, IRQ_LEVEL_HIGH]);
    }
    let node = fdt.begin_node("timer")?;
    fdt.property_string("compatible", "arm,armv8-timer")?;
    fdt.property_null("always-on")?;
    fdt.property_array_u32("interrupts", &cells)?;
    fdt.end_node(node)
}

fn clock(fdt: &mut FdtWriter) -> Result<(), vm_fdt::Error> {
    // The PL011 asks its clock what rate it runs at in order to program the
    // baud divisor, so it needs one even though nothing here is timed.
    let node = fdt.begin_node("apb-pclk")?;
    fdt.property_string("compatible", "fixed-clock")?;
    fdt.property_u32("#clock-cells", 0)?;
    fdt.property_u32("clock-frequency", 24_000_000)?;
    fdt.property_string("clock-output-names", "clk24mhz")?;
    fdt.property_u32("phandle", PHANDLE_CLOCK)?;
    fdt.end_node(node)
}

fn psci(fdt: &mut FdtWriter) -> Result<(), vm_fdt::Error> {
    let node = fdt.begin_node("psci")?;
    fdt.property_string("compatible", "arm,psci-0.2")?;
    // KVM implements PSCI at EL2, reached with HVC rather than SMC.
    fdt.property_string("method", "hvc")?;
    fdt.end_node(node)
}

fn serial(fdt: &mut FdtWriter) -> Result<(), vm_fdt::Error> {
    let node = fdt.begin_node(&format!("pl011@{:x}", layout::SERIAL_BASE))?;
    fdt.property_string_list(
        "compatible",
        vec!["arm,pl011".to_string(), "arm,primecell".to_string()],
    )?;
    fdt.property_array_u64("reg", &[layout::SERIAL_BASE, layout::SERIAL_SIZE])?;
    fdt.property_array_u32(
        "interrupts",
        &[IRQ_SPI, layout::SERIAL_SPI, IRQ_EDGE_RISING],
    )?;
    fdt.property_u32("clocks", PHANDLE_CLOCK)?;
    fdt.property_string("clock-names", "apb_pclk")?;
    fdt.end_node(node)
}

fn virtio(fdt: &mut FdtWriter, n: &VirtioNode) -> Result<(), vm_fdt::Error> {
    let node = fdt.begin_node(&format!("virtio_mmio@{:x}", n.addr))?;
    fdt.property_string("compatible", "virtio,mmio")?;
    fdt.property_array_u64("reg", &[n.addr, n.size])?;
    // Edge triggered, because interrupts are raised through an irqfd, which
    // asserts the line without a way to deassert it.
    fdt.property_array_u32("interrupts", &[IRQ_SPI, n.spi, IRQ_EDGE_RISING])?;
    fdt.end_node(node)
}
