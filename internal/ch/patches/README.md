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

### Requires

- `virglrenderer >= 1.3.0`, built with `-Dvenus=true -Dunstable-apis=true`.
  No distribution ships one new enough; `rutabaga_gfx` refuses to build
  against anything older.
- `rutabaga_gfx` with the `virgl_renderer` cargo feature, which is not a
  default.
