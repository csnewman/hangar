# hangar-vmm

A KVM virtual machine monitor that runs one Hangar environment.

It is not a general VMM and is not meant to become one. Everything it can do,
an environment needs; everything an environment needs, it can do. Nothing
outside `internal/vmm` invokes it, so it has no API, no machine types, no
device discovery and no command line to keep stable — it reads a JSON
description of the machine on standard input and runs until the guest stops.

Build it with `hangar vmm`. Run a guest under it with `hangar run -vmm hangar`.

## Why it exists

virtio-fs DAX. A guest that maps the host's page cache reads file data by
touching memory: no FUSE round trip, no second copy, and one copy of a base
image shared by every environment running it.

Nobody has DAX, and the reason is that it needs both halves of a split. The
filesystem daemon has to answer `setupmapping` by handing out a file
descriptor, and the VMM has to map that descriptor into a window it exposes to
the guest. virtiofsd answers `setupmapping` with `ENOSYS`. Upstream QEMU has
no window. Cloud Hypervisor has the window and says it is waiting on the
daemon.

Owning the VMM collapses the split: the filesystem runs in this process, so
`setupmapping` is an `mmap` into a KVM memory slot and there is no protocol
between the two halves to be half-finished.

`docs/vmm-rust.md` has the full argument and the measurements.

## Shape

```
src/
  main.rs        read the machine description, run it
  config.rs      that description
  memory.rs      guest RAM, and the DAX window
  boot.rs        the arm64 Image and its initramfs
  vcpu.rs        one thread per processor
  gic.rs         GICv3, created inside KVM
  fdt.rs         the device tree the guest is told everything through
  layout.rs      the guest's physical address map
  devices/
    bus.rs       MMIO routing            (in mod.rs)
    serial.rs    PL011
    virtio/
      mmio.rs    the virtio-mmio transport, including shared memory regions
      fs.rs      virtio-fs, in process, with DAX
      fsopts.rs  the FUSE options the transport negotiates
      blk.rs     virtio-blk
      vsock.rs   virtio-vsock, on the host kernel's vhost backend
      balloon.rs virtio-balloon with free page reporting
      worker.rs  the loop a device's thread runs
```

Devices are on virtio-mmio rather than PCI. Shared memory regions — the thing
DAX needs — are in the MMIO register block, so there is no PCI host bridge, no
configuration space, no BAR allocator and no MSI-X routing. That changes when
the CUDA tier needs VFIO; the transport is a trait, not an assumption.

## What it does not do

No x86-64 yet, no networking, no virtio-gpu, no VFIO, no migration, no
snapshot, no hotplug, no seccomp. QEMU remains the working path.
