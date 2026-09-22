# Proposed upstream changes

Unlike `../patches/`, these are meant to go *away*: each is written to be
submitted to Cloud Hypervisor, and carried locally only until it lands.

Developed on a branch of upstream `main`, not of the pinned release, so it
applies where a maintainer would look at it.

## 0001-vhost-user-shmem-map.patch

Serves `SHMEM_MAP` and `SHMEM_UNMAP` backend requests, so a vhost-user device
can publish a shared memory window and let its backend place memory in it.

**533 insertions, 20 deletions, across 14 files.**

Two consumers exist the moment it lands, and both are exercised here:
virtio-fs gets a DAX cache (`../../../fs/`) and a vhost-user virtio-gpu
backend gets somewhere to put host-visible blob resources
(`../../../gpu/`).

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

### Backend requests are served while the device is paused

A device's epoll thread parks while it is paused, and that same thread is what
answers a backend's requests -- so a paused device answers none of them. For
guest traffic that is exactly right, and the comment in `epoll_helper.rs` says
so: "the device thread should not start processing anything before the device
has been resumed".

A backend request is not guest traffic. A backend asking for a region to be
placed in a shared memory window is the device being rebuilt, and a restored
guest's window has to be filled *before* its vCPUs start, because the guest is
already holding addresses that point into it. With the thread parked there is
no moment when that can happen: the window can only be filled after the guest
is already running and faulting on it.

So `EpollHelper` grows `add_event_paused`, and the vhost-user handler
registers its backend request channel with it. Those events are served while
paused; everything else still waits for the resume. They are kept in a second
epoll set rather than filtered out of the main one, because epoll is level
triggered and an event left unhandled is returned again immediately, which
would spin.

Measured on a restore with 154 regions to place: the whole set is made again
in under a second and before the guest is resumed, against five minutes and
only after resume without it.

### A restored window is created where the guest left it

The guest reprograms the window's BAR during boot, and `move_bar()` moves the
memory slot to follow. A restore rebuilds the device from its configuration,
though, and allocating the window afresh puts the slot back where the device
was first given it -- while the PCI configuration space comes back from the
snapshot holding the moved address, which is where the guest's page tables
point. The guest's first touch of the window then finds no memory at all, and
KVM answers with an external abort (the guest reports it as a machine check).

So on restore the window is allocated at the BAR 2 address recorded for the
device in the device tree, which `move_bar()` keeps current.

### Two rules a backend must follow

Both are properties of the guest's memory slot rather than of any one
backend, and both cost a VM if broken, so they are documented on the patch.

The window's slot is writable. KVM therefore lets the guest write anywhere in
it, and a host mapping that refuses does not fault the guest -- `KVM_RUN`
returns EFAULT and the VM dies. A mapping is mapped `PROT_READ | PROT_WRITE`
whatever the request asked for, and a read-only descriptor becomes
`MAP_PRIVATE`.

A descriptor shorter than the requested length cannot back the whole range,
and reading past its end raises SIGBUS rather than returning zeroes. The
file-backed part is clamped to the descriptor's length rounded up to a page,
and the rest is anonymous.

### What it does not do

Nothing about virtio-gpu. That device stays in `../patches/` until the backend
in `../../../gpu/` has been exercised more widely.

## 0002-vsock-deliver-restore-resets.patch

Delivers the connection resets a vsock device queues when it is rebuilt from a
snapshot.

**13 insertions, in one file.** Independent of 0001; either applies alone.

A restored vsock device already queues a reset for every connection the guest
had open, because the host ends of those connections did not survive. Nothing
delivered them: the RX queue is processed when the guest posts buffers or the
backend has input, and a restored guest does neither. A guest process blocked
reading a connection whose host end has gone sends nothing, and so waits on it
forever. For Hangar that process is the agent, which is how the host reaches
the environment at all, so without this a resumed environment is running but
unreachable.

The fix signals the RX queue once when the worker starts with resets pending.
The epoll loop does not run until the device is resumed, so the first pass
after resume delivers them and the guest sees ECONNRESET, which is what lets it
reconnect.
