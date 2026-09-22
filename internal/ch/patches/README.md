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

Not implemented: blob resources and the shared memory window they need, which
is why a guest reports `-resource_blob -host_visible`. Venus needs those, so
`venus=on` is accepted but will not give the guest Vulkan yet.

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
