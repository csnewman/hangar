# Hangar QEMU device selection, common to every target.
#
# Built with --without-default-devices, so QEMU contains nothing that is not
# named here or pulled in by a Kconfig `select`. A stock build carries several
# hundred device models -- floppy, sound, USB, SCSI, legacy network cards --
# which a Hangar guest can never address and which are where most QEMU CVEs
# live. A guest sees a fixed, known device model, so anything beyond it is
# attack surface with no compensating use.
#
# This is the same bargain as the guest kernel's tinyconfig: start from
# nothing, and let the build fail if something needed was left out. The list
# is verified against the finished binary rather than trusted, because Kconfig
# drops an unsatisfied symbol silently.

CONFIG_VIRTIO=y
CONFIG_VIRTIO_PCI=y

# The guest's entire device model.
CONFIG_VIRTIO_BLK=y
CONFIG_VIRTIO_NET=y
CONFIG_VIRTIO_SERIAL=y
CONFIG_VIRTIO_RNG=y

# Memory reclaim: virtio-mem for resize with guarantees, balloon for
# free-page reporting.
CONFIG_VIRTIO_BALLOON=y
CONFIG_VIRTIO_MEM=y

# The accelerated desktop. CONFIG_VIRTIO_GPU alone gives the unaccelerated
# device; the GL variant additionally needs virglrenderer at configure time.
CONFIG_VIRTIO_GPU=y

# The agent channel. Both backends: the kernel's vhost-vsock, and the
# vhost-user one that a userspace daemon drives over a Unix socket.
CONFIG_VHOST_VSOCK=y
CONFIG_VHOST_USER_VSOCK=y

# virtiofs, for the read-only image layer.
CONFIG_VHOST_USER_FS=y
