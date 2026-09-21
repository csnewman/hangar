//! The guest's interrupt controller.
//!
//! GICv3 is created inside KVM, so this is configuration rather than
//! emulation: the addresses the distributor and redistributors answer at, how
//! many interrupts exist, and then a request to finalise. Interrupts
//! themselves are raised with irqfds, which never enter this process.

use std::io;

use kvm_bindings::{
    kvm_create_device, kvm_device_attr, kvm_device_type_KVM_DEV_TYPE_ARM_VGIC_V3,
    KVM_DEV_ARM_VGIC_CTRL_INIT, KVM_DEV_ARM_VGIC_GRP_ADDR, KVM_DEV_ARM_VGIC_GRP_CTRL,
    KVM_DEV_ARM_VGIC_GRP_NR_IRQS, KVM_VGIC_V3_ADDR_TYPE_DIST, KVM_VGIC_V3_ADDR_TYPE_REDIST,
};
use kvm_ioctls::{DeviceFd, VmFd};

use crate::layout;

pub struct Gic {
    _fd: DeviceFd,
    redist_size: u64,
}

impl Gic {
    /// Create and finalise a GICv3 for `vcpus` processors.
    ///
    /// The order matters: addresses and the interrupt count are attributes
    /// that KVM only accepts before `CTRL_INIT`, and vCPUs must exist before
    /// the redistributor addresses are set, because KVM sizes the
    /// redistributor region from the vCPUs it can see.
    pub fn new(vm: &VmFd, vcpus: u8) -> io::Result<Self> {
        let mut dev = kvm_create_device {
            type_: kvm_device_type_KVM_DEV_TYPE_ARM_VGIC_V3,
            fd: 0,
            flags: 0,
        };
        let fd = vm
            .create_device(&mut dev)
            .map_err(|e| io::Error::other(format!("creating the GICv3: {e}")))?;

        let dist = layout::GIC_DIST_BASE;
        set_attr(
            &fd,
            KVM_DEV_ARM_VGIC_GRP_ADDR,
            KVM_VGIC_V3_ADDR_TYPE_DIST as u64,
            &dist,
        )?;

        let redist = layout::GIC_REDIST_BASE;
        set_attr(
            &fd,
            KVM_DEV_ARM_VGIC_GRP_ADDR,
            KVM_VGIC_V3_ADDR_TYPE_REDIST as u64,
            &redist,
        )?;

        let nr_irqs: u32 = layout::NUM_IRQS;
        set_attr(&fd, KVM_DEV_ARM_VGIC_GRP_NR_IRQS, 0, &nr_irqs)?;

        set_attr_noaddr(
            &fd,
            KVM_DEV_ARM_VGIC_GRP_CTRL,
            KVM_DEV_ARM_VGIC_CTRL_INIT as u64,
        )?;

        Ok(Self {
            _fd: fd,
            redist_size: layout::GIC_REDIST_SIZE_PER_CPU * u64::from(vcpus),
        })
    }

    /// The `reg` property of the device tree's interrupt controller node:
    /// distributor then redistributors, each as address and size.
    pub fn fdt_reg(&self) -> [u64; 4] {
        [
            layout::GIC_DIST_BASE,
            layout::GIC_DIST_SIZE,
            layout::GIC_REDIST_BASE,
            self.redist_size,
        ]
    }
}

fn set_attr<T>(fd: &DeviceFd, group: u32, attr: u64, value: &T) -> io::Result<()> {
    let a = kvm_device_attr {
        group,
        attr,
        addr: value as *const T as u64,
        flags: 0,
    };
    fd.set_device_attr(&a)
        .map_err(|e| io::Error::other(format!("GIC attribute {group}/{attr}: {e}")))
}

fn set_attr_noaddr(fd: &DeviceFd, group: u32, attr: u64) -> io::Result<()> {
    let a = kvm_device_attr {
        group,
        attr,
        addr: 0,
        flags: 0,
    };
    fd.set_device_attr(&a)
        .map_err(|e| io::Error::other(format!("GIC control {attr}: {e}")))
}
