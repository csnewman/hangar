# virglrenderer patches

Carried against virglrenderer `7d4eb14`, which `hangar-gpu` links and whose
render server runs Venus contexts.

`build.sh [prefix]` fetches that commit, applies every patch here in order, and
installs the result; CI builds `hangar-gpu` against it the same way.

## 0001-venus-snapshot.patch

Snapshots a Venus (Vulkan) context and restores it in another process -- the
half of GPU suspend that only the renderer can do, because Venus commands
travel through a ring in shared memory that the renderer reads directly and
`hangar-gpu` never sees.

**1007 insertions, 11 deletions, across 21 files**, most of it the new
`src/venus/vkr_snapshot.c`.

The renderer keeps the raw bytes of every command whose effect lasts, against
the objects it concerns, and drops them when those objects are destroyed.
Replaying the survivors in order through the same decoder recreates every
object under the id the guest already holds. A snapshot also carries every
host-visible allocation's contents and the state of fences and events; a ring
created during a restore resumes at the head its shared memory holds.

`hangar-gpu` drives it through `virgl_renderer_context_snapshot` and
`virgl_renderer_context_restore`, and restores the context's shared-memory
blobs first, since the rings live in them.

### Off unless asked for

Recording costs a copy of every lasting command for the life of a context, and
the replay is proven only on lavapipe, so nothing is recorded unless
`VKR_RECORD` is set in the environment the render server inherits. Without it
every command is dispatched untouched and a snapshot is refused.

`hangar-gpu --venus-restore` sets it, and `hangar run -gpu-venus-restore`
passes that flag. Without it a suspended Venus context is not carried: on
resume `hangar-gpu` marks the context's rings fatal and the guest's Vulkan
driver ends the program, which is the device-lost behaviour a real GPU gives.

Measured with `hangar-vkcheck` -- command buffers recorded once and resubmitted
every frame, descriptor sets written once, a uniform buffer kept mapped -- a
Vulkan program mid-render carries on across a suspend and a host restart:
86 of 86 commands replayed, 8 of 8 allocations written back, no GPU errors.

### Not covered

- Memory that is not host-visible is not read back. Every lavapipe memory type
  is; a discrete GPU's device-local memory is not, and needs a staging copy
  through the queue.
- Descriptor updates are kept whole rather than per binding, so a program that
  rewrites the same set every frame grows the record until the set is freed.
- Objects created together and freed separately -- descriptor sets from one
  allocation, say -- all come back, including the freed ones.
- Binary semaphores signalled but not yet waited on, and timeline semaphore
  values, are not carried.
- One program, one context, on lavapipe.

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
