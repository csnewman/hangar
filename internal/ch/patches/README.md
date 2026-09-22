# Cloud Hypervisor patches

Applied to a checkout of the version named in `Version` (see `../build.go`),
in order, before building.

Hangar runs its own build of Cloud Hypervisor, so these are carried rather
than upstreamed.

## 0001-virtio-gpu.patch

Adds an in-tree virtio-gpu device backed by `rutabaga_gfx`, and a `--gpu`
option to turn it on.

The device is in-tree rather than a vhost-user backend because virtio-gpu
cannot be carried over vhost-user to this monitor. Upstream's own
`docs/generic-vhost-user.md` says so: the version QEMU implements needs
`VHOST_USER_GPU_SET_SOCKET`, "which is standard but will never be implemented
by Cloud Hypervisor", and the other versions use unstandardised messages. A
device compiled into the monitor does not use vhost-user at all, so none of
that applies -- it reads its own virtqueues and will own its shared memory
window directly.

Upstream will not take this: the maintainers are on record as uninterested in
GPU support. It is written to rebase cleanly instead -- one new module plus
small additions at the registration points, with no changes to existing
behaviour.

### What works

A guest renders through it. With Mesa in the image and the device enabled:

```
OpenGL core profile renderer: virgl (LLVMPIPE (LLVM 21.1.8, 128 bits))
OpenGL core profile version:  4.3 (Core Profile) Mesa 26.0.8
OpenGL ES profile renderer:   virgl (LLVMPIPE (LLVM 21.1.8, 128 bits))
OpenGL ES profile version:    OpenGL ES 3.2
```

That is the guest's Mesa virgl driver talking to this device, which hands the
command stream to virglrenderer on the host. `eglinfo` needs a real context,
so the context, resource, transfer and submit paths are all exercised by it.

Blob resources work too. The device declares a shared memory window, the
transport puts it in a dedicated PCI BAR, and the renderer maps each blob over
a slice of it, so the guest reads the renderer's own pages:

```
[drm] Host memory window: 0x200000000 +0x20000000
[drm] features: +virgl -edid +resource_blob +host_visible
```

With the window present the guest's OpenGL goes from 4.3 to 4.5, and Vulkan
works through venus:

```
deviceName = Virtio-GPU Venus (llvmpipe (LLVM 21.1.8, 128 bits))
driverName = venus
apiVersion = 1.4.334
```

Venus needs virglrenderer's render server. Without it the venus capset is
advertised but reads back as 160 zero bytes, and a guest Mesa correctly
refuses to use it. The render server is a second process the device spawns,
which is why `venus=on` widens the syscall filter further -- including
`execve`.

The capability carries an id the driver looks the region up by, and it is a
property of the region rather than its position in the list: virtio-fs uses 0,
virtio-gpu uses 1 for its host-visible window. The transport derived it from
the list index, which is why the guest found nothing at first, so
`VirtioSharedMemory` now carries the id and the transport uses it.

### Seccomp

Device threads inherit the monitor thread's filter, and seccomp filters stack
-- the most restrictive wins -- so the renderer's syscalls have to be allowed
in *both* the device filter and the monitor's. The monitor's is only relaxed
when `--gpu` is passed.

Both sets were measured rather than guessed: `strace -f -c` over a full
rutabaga initialisation on this host. The monitor's relaxation includes an
unconditional `ioctl`, because the renderer issues DRM ioctls that the
hypervisor's ioctl allowlist does not cover. That is a real widening of the
monitor's syscall surface, and it is the price of rendering in-process.

When extending either set, run with `--seccomp log`: the kernel then records
every refused syscall in the audit log instead of killing the VM on the first
one.

### Requires

- `virglrenderer >= 1.3.0`, built with `-Dvenus=true -Dunstable-apis=true`.
  No distribution ships one new enough; `rutabaga_gfx` refuses to build
  against anything older.
- `rutabaga_gfx` with the `virgl_renderer` cargo feature, which is not a
  default.
