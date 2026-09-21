//! A virtual processor and the thread that runs it.
//!
//! The loop is small because KVM does nearly all of it: the only exits this
//! VMM has to answer are memory-mapped I/O to a device, and the two ways a
//! guest asks to be switched off.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use kvm_bindings::{
    kvm_vcpu_init, KVM_ARM_VCPU_POWER_OFF, KVM_ARM_VCPU_PSCI_0_2, KVM_SYSTEM_EVENT_CRASH,
    KVM_SYSTEM_EVENT_RESET, KVM_SYSTEM_EVENT_SHUTDOWN,
};
use kvm_ioctls::{VcpuExit, VcpuFd, VmFd};

use crate::devices::Bus;

/// Register ids, encoded the way `KVM_SET_ONE_REG` wants them: an
/// architecture, a size, a register file, and the register's offset within
/// `struct kvm_regs` counted in 32-bit words.
///
/// Only two are set. The arm64 boot protocol says the kernel is entered at
/// its base address with the device tree's address in x0 and the other
/// argument registers zero, and KVM resets everything else for us.
const KVM_REG_ARM64: u64 = 0x6000_0000_0000_0000;
const KVM_REG_SIZE_U64: u64 = 0x0030_0000_0000_0000;
/// The core register file, `0x0010` shifted into the coprocessor field.
const KVM_REG_ARM_CORE: u64 = 0x0010 << 16;

/// `struct kvm_regs` starts with `struct user_pt_regs`, which is 31 general
/// registers, then the stack pointer, then the program counter.
const OFFSET_X0: u64 = 0;
const OFFSET_PC: u64 = 32 * 8;

/// Turn a byte offset into `struct kvm_regs` into a register id.
fn core_reg(offset: u64) -> u64 {
    KVM_REG_ARM64 | KVM_REG_SIZE_U64 | KVM_REG_ARM_CORE | (offset / 4)
}

pub struct Vcpu {
    fd: VcpuFd,
    index: u8,
}

impl Vcpu {
    /// Create vCPU `index` and put it in the state the kernel expects.
    ///
    /// Every processor but the first is created powered off: the guest starts
    /// one processor and brings the rest up itself through PSCI, which is how
    /// an arm64 kernel expects to find a multiprocessor machine.
    pub fn new(vm: &VmFd, index: u8, preferred: &kvm_vcpu_init) -> io::Result<Self> {
        let fd = vm
            .create_vcpu(u64::from(index))
            .map_err(|e| io::Error::other(format!("creating vCPU {index}: {e}")))?;

        let mut init = *preferred;
        init.features[0] |= 1 << KVM_ARM_VCPU_PSCI_0_2;
        if index != 0 {
            init.features[0] |= 1 << KVM_ARM_VCPU_POWER_OFF;
        }
        fd.vcpu_init(&init)
            .map_err(|e| io::Error::other(format!("initialising vCPU {index}: {e}")))?;

        Ok(Self { fd, index })
    }

    /// Point the first processor at the kernel, with the device tree's
    /// address in its first argument register.
    pub fn set_entry(&self, entry: u64, fdt_addr: u64) -> io::Result<()> {
        self.fd
            .set_one_reg(core_reg(OFFSET_PC), &entry.to_le_bytes())
            .map_err(|e| io::Error::other(format!("setting the entry point: {e}")))?;
        self.fd
            .set_one_reg(core_reg(OFFSET_X0), &fdt_addr.to_le_bytes())
            .map_err(|e| io::Error::other(format!("passing the device tree address: {e}")))?;
        Ok(())
    }

    /// Run until the guest stops or something goes wrong.
    pub fn start(self, bus: Arc<Bus>, running: Arc<AtomicBool>) -> io::Result<JoinHandle<()>> {
        let index = self.index;
        thread::Builder::new()
            .name(format!("hangar-vcpu-{index}"))
            .spawn(move || {
                let mut vcpu = self;
                loop {
                    if !running.load(Ordering::Relaxed) {
                        return;
                    }
                    match vcpu.fd.run() {
                        Ok(VcpuExit::MmioRead(addr, data)) => bus.read(addr, data),
                        Ok(VcpuExit::MmioWrite(addr, data)) => bus.write(addr, data),
                        Ok(VcpuExit::SystemEvent(event, _)) => {
                            match event {
                                KVM_SYSTEM_EVENT_SHUTDOWN => {
                                    log::info!("guest powered off");
                                }
                                KVM_SYSTEM_EVENT_RESET => {
                                    // Nothing here can reboot a guest in
                                    // place, and an environment that wants a
                                    // fresh kernel is a fresh environment.
                                    log::info!("guest asked to reset; stopping instead");
                                }
                                KVM_SYSTEM_EVENT_CRASH => {
                                    log::error!("guest crashed");
                                }
                                other => log::info!("guest raised system event {other}"),
                            }
                            running.store(false, Ordering::Relaxed);
                            return;
                        }
                        Ok(VcpuExit::Hlt) => {
                            running.store(false, Ordering::Relaxed);
                            return;
                        }
                        Ok(other) => {
                            log::error!("vCPU {index}: unhandled exit {other:?}");
                            running.store(false, Ordering::Relaxed);
                            return;
                        }
                        // A signal delivered to stop the thread lands here.
                        Err(e) if e.errno() == libc::EINTR => continue,
                        Err(e) => {
                            log::error!("vCPU {index}: {e}");
                            running.store(false, Ordering::Relaxed);
                            return;
                        }
                    }
                }
            })
    }
}
