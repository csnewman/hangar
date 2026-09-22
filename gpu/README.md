# hangar-gpu

A virtio-gpu device served over vhost-user and rendered by `rutabaga_gfx`.

**Status: does not compile yet.** The in-tree device in
`../internal/ch/patches/` is what works today and stays until this replaces
it.

## Why a separate process

The monitor carries the guest's virtqueues here and publishes a shared memory
window on this process's behalf; everything about rendering happens here.

That buys three things the in-tree device cannot have. The renderer's syscall
surface belongs to a process that holds no guest memory it did not ask for,
rather than forcing the monitor's own filter open -- the in-tree device needs
an unconditional `ioctl` and, for Venus, `execve` in the *monitor*. A crash in
native graphics code takes down a backend rather than the VM. And the monitor
goes back to being stock upstream plus one generic patch.

## What remains

The command layer is ported and the transport is written. Blob resources are
the unfinished part: placing one means sending the monitor a descriptor for
the resource's memory, and `Rutabaga::export_blob` in 0.1.85 returns a
`RutabagaHandle` enum whose payload differs from what the reference backend
expects. Extracting a descriptor and a length from it is the open question.

Everything else -- resource creation, backing, transfers, contexts, 3D
submission, fences -- is the same code that already renders OpenGL 4.5 and
Vulkan 1.4 in the in-tree device, with only the queue plumbing replaced.

## Depends on

`../internal/ch/upstream/0001-vhost-user-shmem-map.patch`, without which the
monitor cannot serve the map requests this backend sends.
