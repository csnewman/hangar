# Proposed upstream changes

Unlike `../patches/`, these are meant to go *away*: each is written to be
submitted to Cloud Hypervisor, and carried locally only until it lands.

Developed on a branch of upstream `main`, not of the pinned release, so it
applies where a maintainer would look at it.

## 0001-vhost-user-shmem-map.patch

Serves `SHMEM_MAP` and `SHMEM_UNMAP` backend requests, so a vhost-user device
can publish a shared memory window and let its backend place memory in it.

**239 insertions, 7 deletions, across 8 files.**

Two consumers exist the moment it lands: virtio-fs gets a DAX cache, and a
vhost-user virtio-gpu backend gets somewhere to put host-visible blob
resources. Neither works today.

It is deliberately generic and mentions no GPU. The maintainers are on record
as uninterested in GPU support, and this needs none of it to be worth having.

It also carries the one genuine bug fix in this whole effort: the shared
memory PCI capability's id is a property of the device, not of the region's
position in a list. Deriving it from the index is correct only because
virtio-fs uses 0 and is the sole consumer today.

### Why it is small

The scaffolding was already upstream and simply unfinished. Both virtio-fs and
the generic vhost-user device already carry a `cache` field, expose it through
`get_shm_regions`, and the generic device already negotiates `BACKEND_REQ`.
What was missing was a handler for the map requests, and anything to configure
a window with. The message types have been in `vhost` since 0.16.0 -- the
version Cloud Hypervisor already depends on -- and nothing consumed them.

### What it does not do

Nothing about virtio-gpu. That device stays in `../patches/`, and will until
the backend in `../../../gpu/` works.
