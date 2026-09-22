# virglrenderer patches

Carried against virglrenderer `7d4eb14`, which `hangar-gpu` links and whose
render server runs Venus contexts.

## 0001-venus-snapshot.patch

Snapshots a Venus (Vulkan) context and restores it in another process -- the
half of GPU suspend that only the renderer can do, because Venus commands
travel through a ring in shared memory that the renderer reads directly and
`hangar-gpu` never sees.

**982 insertions, 11 deletions, across 21 files**, most of it the new
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
