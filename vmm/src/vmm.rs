//! Assembling and running one guest.

use std::io;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use kvm_bindings::KVM_ARM_VCPU_PSCI_0_2;
use kvm_ioctls::{Kvm, VmFd};
use vm_memory::Address;
use vmm_sys_util::eventfd::EventFd;

use crate::config::Config;
use crate::devices::serial::Pl011;
use crate::devices::virtio::worker::Stop;
use crate::devices::virtio::{balloon, blk, fs, mmio, vsock, Interrupt, VirtioDevice};
use crate::devices::{Bus, MmioDevice};
use crate::memory::{DaxWindow, Ram};
use crate::{boot, fdt, gic, layout};

pub struct Vm {
    _kvm: Kvm,
    /// Held for the life of the guest: closing it destroys the VM, its
    /// memory slots and every descriptor bound to it.
    _vm: VmFd,
    ram: Ram,
    bus: Arc<Bus>,
    running: Arc<AtomicBool>,
    stop: Stop,
    vcpus: Vec<crate::vcpu::Vcpu>,
    /// Kept so the window outlives every mapping made into it.
    _dax: Option<Arc<DaxWindow>>,
    /// Kept so the devices outlive the threads serving them.
    _devices: Vec<Arc<Mutex<dyn VirtioDevice>>>,
}

impl Vm {
    pub fn new(cfg: &Config) -> io::Result<Self> {
        let kvm = Kvm::new().map_err(|e| io::Error::other(format!("opening /dev/kvm: {e}")))?;
        let vm = kvm
            .create_vm()
            .map_err(|e| io::Error::other(format!("creating the VM: {e}")))?;

        let ram_size = cfg.memory_mib * 1024 * 1024;
        let ram = Ram::new(ram_size)?;
        let next_slot = ram.register(&vm, 0)?;

        // vCPUs exist before the interrupt controller, because KVM sizes the
        // redistributor region from the processors it can see.
        let mut preferred = kvm_bindings::kvm_vcpu_init::default();
        vm.get_preferred_target(&mut preferred)
            .map_err(|e| io::Error::other(format!("asking KVM for a vCPU target: {e}")))?;
        preferred.features[0] |= 1 << KVM_ARM_VCPU_PSCI_0_2;

        let mut vcpus = Vec::with_capacity(cfg.cpus as usize);
        for i in 0..cfg.cpus {
            vcpus.push(crate::vcpu::Vcpu::new(&vm, i, &preferred)?);
        }
        let gic = gic::Gic::new(&vm, cfg.cpus)?;

        let stop = Stop::new()?;
        let mut bus = Bus::new();
        let mut devices: Vec<Arc<Mutex<dyn VirtioDevice>>> = Vec::new();
        let mut nodes = Vec::new();

        // The console, first so its output covers everything after it.
        let serial_irq = Arc::new(register_irq(&vm, layout::SERIAL_SPI)?);
        let serial = Arc::new(Mutex::new(Pl011::new(
            crate::devices::serial::output(cfg.console.as_deref())?,
            serial_irq,
        )));
        if cfg.console.is_none() {
            crate::devices::serial::forward_stdin(serial.clone());
        }
        bus.insert(layout::SERIAL_BASE, layout::SERIAL_SIZE, serial)
            .map_err(io::Error::other)?;

        // The DAX window sits above RAM, so it is allocated before any device
        // that needs to advertise it.
        let dax = match &cfg.fs {
            Some(f) if f.dax_mib > 0 => {
                let base = crate::memory::dax_window_base(ram.end());
                let window = Arc::new(DaxWindow::new(base, f.dax_mib * 1024 * 1024)?);
                window.register(&vm, next_slot)?;
                log::info!("dax window {} MiB at {:#x}", f.dax_mib, window.guest_addr());
                Some(window)
            }
            _ => None,
        };

        let mut slot = 0usize;
        for (i, disk) in cfg.disks.iter().enumerate() {
            let device = blk::Block::new(
                &disk.path,
                disk.read_only,
                format!("hangar-disk-{i}"),
                stop.clone(),
            )?;
            attach(
                &vm,
                &mut bus,
                &mut nodes,
                &mut devices,
                &ram,
                &mut slot,
                Arc::new(Mutex::new(device)),
                false,
            )?;
        }

        if let Some(f) = &cfg.fs {
            let device = fs::Fs::new(
                &f.shared_dir,
                f.tag.clone(),
                f.queues,
                dax.clone(),
                stop.clone(),
            )?;
            attach(
                &vm,
                &mut bus,
                &mut nodes,
                &mut devices,
                &ram,
                &mut slot,
                Arc::new(Mutex::new(device)),
                false,
            )?;
        }

        if let Some(cid) = cfg.vsock_cid {
            let device = vsock::Vsock::new(cid, ram.mem.clone(), stop.clone())?;
            // The host kernel raises this device's interrupt itself.
            attach(
                &vm,
                &mut bus,
                &mut nodes,
                &mut devices,
                &ram,
                &mut slot,
                Arc::new(Mutex::new(device)),
                true,
            )?;
        }

        if let Some(b) = &cfg.balloon {
            let device = balloon::Balloon::new(b.free_page_reporting, stop.clone());
            attach(
                &vm,
                &mut bus,
                &mut nodes,
                &mut devices,
                &ram,
                &mut slot,
                Arc::new(Mutex::new(device)),
                false,
            )?;
        }

        // The tree is written last, because it describes everything above it.
        let loaded = boot::load_kernel(&ram.mem, &cfg.kernel, ram_size)?;
        let initrd = match &cfg.initrd {
            Some(p) => Some(boot::load_initrd(&ram.mem, p, ram_size)?),
            None => None,
        };
        let fdt_addr = fdt::write(&fdt::Params {
            cmdline: &cfg.cmdline,
            mem: &ram.mem,
            ram_base: layout::RAM_BASE,
            ram_size,
            cpus: cfg.cpus,
            gic: &gic,
            initrd,
            virtio: &nodes,
        })?;

        vcpus[0].set_entry(loaded.entry, fdt_addr.raw_value())?;

        Ok(Self {
            _kvm: kvm,
            _vm: vm,
            ram,
            bus: Arc::new(bus),
            running: Arc::new(AtomicBool::new(true)),
            stop,
            vcpus,
            _dax: dax,
            _devices: devices,
        })
    }

    /// Start every processor and wait for the guest to stop.
    ///
    /// Takes `&mut self` rather than consuming the machine: the VM
    /// descriptor, the guest's memory and the devices all have to outlive the
    /// threads using them, and destructuring would drop whichever fields the
    /// threads do not themselves hold.
    pub fn run(&mut self) -> io::Result<()> {
        let mut handles = Vec::with_capacity(self.vcpus.len());
        for vcpu in self.vcpus.drain(..) {
            handles.push(vcpu.start(self.bus.clone(), self.running.clone())?);
        }
        for handle in handles {
            let _ = handle.join();
        }
        if let Some(window) = &self._dax {
            log::info!(
                "dax: {} MiB mapped at shutdown",
                window.mapped_bytes() / (1024 * 1024)
            );
        }
        // Every device thread is waiting on a queue that will never be kicked
        // again; this is what lets them finish so the process can exit.
        self.stop.signal();
        Ok(())
    }

    pub fn ram_size(&self) -> u64 {
        self.ram.size
    }
}

/// Put a virtio device on the bus, wire its interrupt and its queue
/// notifications to KVM, and describe it for the device tree.
#[allow(clippy::too_many_arguments)]
fn attach(
    vm: &VmFd,
    bus: &mut Bus,
    nodes: &mut Vec<fdt::VirtioNode>,
    devices: &mut Vec<Arc<Mutex<dyn VirtioDevice>>>,
    ram: &Ram,
    slot: &mut usize,
    device: Arc<Mutex<dyn VirtioDevice>>,
    external_interrupt: bool,
) -> io::Result<()> {
    if *slot >= layout::VIRTIO_MMIO_MAX_DEVICES {
        return Err(io::Error::other(format!(
            "no room for another virtio device: {} is the limit",
            layout::VIRTIO_MMIO_MAX_DEVICES
        )));
    }
    let (addr, spi) = layout::virtio_mmio_slot(*slot);
    *slot += 1;

    let irq = register_irq(vm, spi)?;
    let interrupt = Arc::new(if external_interrupt {
        Interrupt::external(irq)
    } else {
        Interrupt::new(irq)
    });

    // One notification descriptor per queue, bound to a write of that queue's
    // index to the transport's notify register. KVM turns the guest's kick
    // into a write on the descriptor without ever leaving the kernel.
    let queue_count = device.lock().unwrap().queue_max_sizes().len();
    let mut kicks = Vec::with_capacity(queue_count);
    for index in 0..queue_count {
        let evt = EventFd::new(libc::EFD_NONBLOCK)?;
        vm.register_ioevent(
            &evt,
            &kvm_ioctls::IoEventAddress::Mmio(addr + mmio::NOTIFY_OFFSET),
            index as u32,
        )
        .map_err(|e| io::Error::other(format!("registering a queue notification: {e}")))?;
        kicks.push(evt);
    }

    let transport = mmio::Transport::new(device.clone(), ram.mem.clone(), interrupt, kicks)
        .map_err(io::Error::other)?;
    let transport: Arc<Mutex<dyn MmioDevice>> = Arc::new(Mutex::new(transport));
    bus.insert(addr, layout::VIRTIO_MMIO_SIZE, transport)
        .map_err(io::Error::other)?;

    nodes.push(fdt::VirtioNode {
        addr,
        size: layout::VIRTIO_MMIO_SIZE,
        spi,
    });
    devices.push(device);
    Ok(())
}

/// Bind a fresh descriptor to a guest interrupt, so writing it raises the
/// line without an ioctl.
fn register_irq(vm: &VmFd, spi: u32) -> io::Result<EventFd> {
    let evt = EventFd::new(libc::EFD_NONBLOCK)?;
    vm.register_irqfd(&evt, spi)
        .map_err(|e| io::Error::other(format!("binding an interrupt for SPI {spi}: {e}")))?;
    Ok(evt)
}
