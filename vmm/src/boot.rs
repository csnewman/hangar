//! Getting the kernel and its initramfs into guest memory.
//!
//! An arm64 `Image` is not a container format: it is the kernel's own text,
//! prefixed by a header that says where it wants to be and how much room it
//! needs. Loading it is a copy to that address.

use std::io;
use std::path::Path;

use vm_memory::{Bytes, GuestAddress, GuestAddressSpace};

use crate::layout;
use crate::memory::Mem;

/// The arm64 Image header, as `Documentation/arm64/booting.rst` defines it.
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct ImageHeader {
    /// Two instructions, so the image is also executable as a bootloader
    /// entry point on hardware that jumps straight to it.
    _code0: u32,
    _code1: u32,
    /// Where the kernel wants to be, relative to the base of RAM.
    text_offset: u64,
    /// How much room it needs once it has unpacked itself, which is more than
    /// the file's size.
    image_size: u64,
    _flags: u64,
    _res2: u64,
    _res3: u64,
    _res4: u64,
    /// "ARM\x64".
    magic: u32,
    _res5: u32,
}

const IMAGE_MAGIC: u32 = 0x644d_5241;
const HEADER_SIZE: usize = std::mem::size_of::<ImageHeader>();

/// Where the kernel was put, and where it is entered.
pub struct Loaded {
    pub entry: u64,
}

/// Load an arm64 `Image` into guest memory.
pub fn load_kernel(mem: &Mem, path: &Path, ram_size: u64) -> io::Result<Loaded> {
    let data = std::fs::read(path)
        .map_err(|e| io::Error::other(format!("reading the kernel {}: {e}", path.display())))?;
    if data.len() < HEADER_SIZE {
        return Err(io::Error::other(format!(
            "{} is too short to be an arm64 kernel image",
            path.display()
        )));
    }

    let mut header = ImageHeader::default();
    // SAFETY: the header is plain data of exactly this size, and the source
    // has been checked to be at least that long.
    unsafe {
        std::ptr::copy_nonoverlapping(
            data.as_ptr(),
            &mut header as *mut ImageHeader as *mut u8,
            HEADER_SIZE,
        );
    }
    if header.magic != IMAGE_MAGIC {
        return Err(io::Error::other(format!(
            "{} is not an arm64 kernel image: its magic is {:#x}, not {:#x}",
            path.display(),
            header.magic,
            IMAGE_MAGIC
        )));
    }

    // A kernel built since the boot protocol was relaxed asks for offset
    // zero and expects to be placed anywhere 2 MiB aligned. Honour whatever
    // it asks for, but never below the gap this layout keeps at the bottom
    // of RAM.
    let offset = layout::align_up(header.text_offset.max(layout::KERNEL_OFFSET), 0x20_0000);
    let addr = layout::RAM_BASE + offset;
    let needed = header.image_size.max(data.len() as u64);
    if offset + needed + layout::FDT_MAX_SIZE > ram_size {
        return Err(io::Error::other(format!(
            "the kernel needs {needed} bytes at offset {offset}, which does not fit \
             in {ram_size} bytes of RAM"
        )));
    }

    let guard = mem.memory();
    guard
        .write_slice(&data, GuestAddress(addr))
        .map_err(|e| io::Error::other(format!("copying the kernel into guest memory: {e}")))?;
    Ok(Loaded { entry: addr })
}

/// Copy an initramfs into guest memory, returning its address and size.
///
/// It goes as high as it will fit, below the device tree: the kernel is at
/// the bottom of RAM and grows upwards as it unpacks itself, and an initramfs
/// in its way is an unrecoverable and very confusing failure.
pub fn load_initrd(mem: &Mem, path: &Path, ram_size: u64) -> io::Result<(u64, u64)> {
    let data = std::fs::read(path)
        .map_err(|e| io::Error::other(format!("reading the initramfs {}: {e}", path.display())))?;
    let size = data.len() as u64;

    let fdt_start = layout::RAM_BASE + ram_size - layout::FDT_MAX_SIZE;
    // Page aligned, so the kernel can return the pages it occupied afterwards.
    let addr = (fdt_start - size) & !0xfff;
    if addr <= layout::RAM_BASE + layout::KERNEL_OFFSET {
        return Err(io::Error::other(format!(
            "an initramfs of {size} bytes does not fit in {ram_size} bytes of RAM \
             alongside the kernel"
        )));
    }

    let guard = mem.memory();
    guard
        .write_slice(&data, GuestAddress(addr))
        .map_err(|e| io::Error::other(format!("copying the initramfs into guest memory: {e}")))?;
    Ok((addr, size))
}
