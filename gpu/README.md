# hangar-gpu

A virtio-gpu device served over vhost-user and rendered by `rutabaga_gfx`.

**Status: working.** A guest gets OpenGL 4.5 and Vulkan 1.4 rendered by this
process, with blob resources mapped into a window the monitor publishes:

```
gpu         rendered by hangar-gpu, 512 MiB window
OpenGL core profile version: 4.5 (Core Profile) Mesa 26.0.8
deviceName = Virtio-GPU Venus (llvmpipe (LLVM 21.1.8, 128 bits))
driverName = venus
apiVersion = 1.4.334

[drm] Host memory window: 0x200000000 +0x20000000
[drm] features: +virgl -edid +resource_blob +host_visible
[drm] number of cap sets: 3
```

The monitor running that guest is upstream `main` plus one generic patch, and
contains no GPU code at all.

The in-tree device in `../internal/ch/patches/` is kept until this has been
exercised more widely.

## Why a separate process

The monitor carries the guest's virtqueues here and publishes a shared memory
window on this process's behalf; everything about rendering happens here.

That buys three things the in-tree device cannot have. The renderer's syscall
surface belongs to a process that holds no guest memory it did not ask for,
rather than forcing the monitor's own filter open -- the in-tree device needs
an unconditional `ioctl` and, for Venus, `execve` in the *monitor*. A crash in
native graphics code takes down a backend rather than the VM. And the monitor
goes back to being stock upstream plus one generic patch.

## How a blob gets to the guest

The window belongs to the monitor, so this process cannot map into it. On
`RESOURCE_MAP_BLOB` the resource is exported as a descriptor and sent to the
monitor with `SHMEM_MAP`, naming an offset in the window; the monitor places
it there. `Rutabaga::export_blob` returns a `RutabagaHandle` enum, whose
`MagmaGpuHandle` variant carries the descriptor. It carries no length, so the
size is kept from the creation request.

## Two things that cost time

`features()` has to include `VHOST_USER_F_PROTOCOL_FEATURES`. Without it
nothing else is negotiated, the monitor cannot even read the config space, and
a guest sees a device with no capsets and no features.

The renderer is neither `Send` nor `Sync`, and the daemon shares the backend
between threads. It lives in thread-local storage on the worker thread and is
built on first use, which is the approach `vhost-device-gpu` takes for the
same reason.

## Depends on

`../internal/ch/upstream/0001-vhost-user-shmem-map.patch`, without which the
monitor cannot serve the map requests this backend sends.
