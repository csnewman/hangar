# hangar-fs

A virtio-fs device served over vhost-user, backed by a host directory, with a
DAX window.

**Status: working.** A guest mounts its root over this backend and the DAX
window carries file contents directly, with no copy into the guest page
cache:

```
[    0.513191] virtiofs virtio3: discovered new tag: hangar-base
[    0.519152] virtiofs virtio3: Cache len: 0x40000000 @ 0x200000000

setupmapping fd_offset=0x0 shm_offset=0x17800000 len=0x200000 writable=false -> Ok(())
```

189 mappings over a boot, no VCPU faults, guest reaches `graphical.target`.

Roughly 590 lines on top of `fuse-backend-rs`, which supplies the passthrough
filesystem and the FUSE decoding; what is here is the vhost-user daemon, the
mount options, and the `FsCacheReqHandler` that turns `setupmapping` into a
`SHMEM_MAP` to the monitor.

## What DAX needs that a plain virtio-fs backend does not

`setupmapping` hands the guest a range of a host file at an offset in a shared
window. The window belongs to the monitor, so this process cannot place
anything in it; it sends the descriptor with `SHMEM_MAP` and the monitor maps
it. That request is what
`../internal/ch/upstream/0001-vhost-user-shmem-map.patch` adds, and without it
this backend can serve a filesystem but not a window.

## Depends on

- `../internal/ch/upstream/0001-vhost-user-shmem-map.patch` — the monitor side
  of `SHMEM_MAP` / `SHMEM_UNMAP`.
- `../internal/kernel/patches/0001-dax-guard-folio.patch` — without it the
  guest panics in `dax_disassociate_entry` on the first invalidation of an
  entry that was never mapped.

## Two things that cost time

The monitor's memory slot for the window is writable, so KVM lets the guest
write anywhere in it and only the host mapping can refuse. A refusal there
does not fault the guest: it returns EFAULT from `KVM_RUN` and the VM dies. So
a mapping is always `PROT_READ | PROT_WRITE`, and a range the backend marked
read-only is mapped `MAP_PRIVATE` — a guest that writes gets its own copy, and
readers still share the backend's pages.

A file shorter than the requested length cannot be mapped for the whole range,
and mapping past its end gives SIGBUS rather than zeroes. The file-backed part
is clamped to the file's length rounded up to a page, and the remainder is
anonymous.

## Performance

A straight trade of boot latency for memory, measured in `../docs/plan.md`:
at a 64 KiB threshold a guest costs 44 MiB less RAM and boots 0.47 s slower.
The saving comes of sharing rather than repeating -- two guests on one base
save 107 MiB where two private savings would be 80 -- so it improves with
density.

`--dax-min-file-size` is the knob, and 64 KiB is the floor worth using: below
it, a mapping costs a FUSE round trip, a `SHMEM_MAP` and two `mmap`s, plus
window space, to save copying a few kilobytes.
