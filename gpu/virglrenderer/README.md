# virglrenderer patches

Carried against virglrenderer `7d4eb14`, which `hangar-gpu` and libkrun link,
on Linux and macOS, and whose render server runs Venus contexts.

`build.sh [prefix]` fetches that commit, applies every patch here in order, and
installs the result; CI builds `hangar-gpu` against it the same way. On macOS
it builds Venus alone (no OpenGL renderer: the Mac has no OpenGL for it), and
needs Python's `mako` and the Vulkan SDK's `vulkan.pc` on the paths it is run
with, as `dev/krun-build.sh` gives it.

## 0001-venus-snapshot.patch

Snapshots a Venus (Vulkan) context and restores it in another process -- the
half of GPU suspend that only the renderer can do, because Venus commands
travel through a ring in shared memory that the renderer reads directly and
`hangar-gpu` never sees.

**1196 insertions, 13 deletions, across 23 files**, most of it the new
`src/venus/vkr_snapshot.c`.

The renderer keeps the raw bytes of every command whose effect lasts, against
the objects it concerns, and drops them when those objects are destroyed,
unless a later command names them: a pipeline names the shader modules a
program destroys as soon as it has one. Replaying the survivors in order
through the same decoder recreates every object under the id the guest
already holds. A snapshot also carries every host-visible allocation's
contents and the state of fences, events and semaphores, a binary
semaphore's payload as the submit and semaphore commands left it, since the
driver has no query for it. A ring created during a restore resumes at the
head its shared memory holds, and starts once the rest is back.

`hangar-gpu` drives it through `virgl_renderer_context_snapshot` and
`virgl_renderer_context_restore`, and restores the context's shared-memory
blobs first, since the rings live in them.

### Off unless asked for

Recording costs a copy of every lasting command for the life of a context, and
the replay is proven only on lavapipe, so nothing is recorded unless
`VKR_RECORD` is set in the environment the render server inherits. Without it
every command is dispatched untouched and a snapshot is refused.

`hangar-gpu --venus-restore` sets it, and a worker passes that flag when its
configuration has `vm.gpu.venus_restore`. Without it a suspended Venus context is not carried: on
resume `hangar-gpu` marks the context's rings fatal and the guest's Vulkan
driver ends the program, which is the device-lost behaviour a real GPU gives.

Measured with `hangar-vkcheck` -- command buffers recorded once and resubmitted
every frame, descriptor sets written once, a uniform buffer kept mapped -- a
Vulkan program mid-render carries on across a suspend and a host restart:
86 of 86 commands replayed, 8 of 8 allocations written back, no GPU errors.
Under libkrun, vkcube on labwc carries on rendering across two suspends in a
row, beside a second Vulkan program: 152 of 152 commands replayed each time.

### Not covered

- Memory that is not host-visible is not read back. Every lavapipe memory type
  is; a discrete GPU's device-local memory is not, and needs a staging copy
  through the queue.
- Descriptor updates are kept whole rather than per binding, so a program that
  rewrites the same set every frame grows the record until the set is freed.
- Objects created together and freed separately -- descriptor sets from one
  allocation, say -- all come back, including the freed ones.
- A destroyed object that a later command names comes back on restore and
  stays until the program exits: a shader module per pipeline, say.
- Proven on lavapipe only.

## 0002-vrend-record-context-made-current.patch

Fixes a GL context mix-up that fails a running guest context whenever another
one is created.

**8 insertions, in one file.** Independent of 0001, and meant for upstream.

Creating a context creates its first sub-context, and that makes the
sub-context's GL context current. `vrend_state` went on naming whichever
context was current before, so switching back to that one was skipped as a
no-op: its next command ran in the new context's GL context, where none of its
vertex array objects exist and no framebuffer is bound. The draw fails with
`GL_INVALID_OPERATION` (`glBindVertexArray(non-gen name)`) and the context is
marked in error.

Any guest process that opens a GL context while another is rendering
triggers it. A readback through `TRANSFER_FROM_HOST_3D` is where it shows,
because that path checks for GL errors: a compositor went dark the first
time something took a screenshot of it.

Found by recording a backtrace at every `make_current` and checking, before
each draw and transfer, that the EGL context current is the one vrend's
bookkeeping expects.

## 0003-venus-vulkan-metal-include.patch

Upstream's `aafa9bd2`, after the base: the Metal helpers include
`vulkan_metal.h` from where the build puts it, without which Venus does not
build on macOS. Drops when the base moves past it.

## 0004-proxy-fence-pipe.patch

Fences signal on macOS. **Meant for upstream.**

The render server tells the proxy a fence has signalled through an eventfd,
which macOS doesn't have, and without one thread sync was dropped: the
proxy's fences retired only if the VMM polled the renderer. rutabaga, which
libkrun drives virglrenderer through, waits for the async fence callback
instead, so no fence ever signalled. The guest's Venus driver gives up on
such a wait after two seconds, which is enough to stop zink starting.

Where there is no eventfd the proxy makes a pipe: the server writes to one
end as it did to the eventfd, and the sync thread polls the other.

## 0005-venus-macos-host-pointer-memory.patch

Venus over KosmicKrisp: memory shared with the guest and between contexts,
and zink on top of it. **Meant for upstream**, split as it goes.

Upstream's macOS Venus wraps the guest's host-visible memory, shared memory
the VMM maps, in a Metal buffer, which MoltenVK imports. KosmicKrisp imports
no Metal buffers, has no `VK_EXT_metal_objects` to make one with, and lays
tiled images out in heaps only; it imports host pointers, into which it
places buffers and linear images. So:

- Host-visible memory is imported by its host pointer where there is no
  Metal device.
- Each memory type is offered again without host access, allocated by the
  driver, and tiled images can only be bound to those copies.
- A shared memory resource keeps its fd beside its mapping (which also fixes
  freeing one made from exported memory, whose fd was unmapped as a
  pointer), so another context can import it: a compositor importing a
  client's buffer.
- The guest is offered `VK_KHR_external_memory_fd` and
  `VK_EXT_external_memory_dma_buf`, which zink's screen needs and window
  systems share buffers with, implemented as shared memory: exportable and
  importable for buffers and linear images, never asked of the driver.

With 0004 and this, a Mac guest's desktop draws through zink on the Mac's
GPU: labwc compositing vkcube, glmark2 through zink at about 3000 FPS on an
M3 Pro.

