#!/bin/busybox sh
# PID 1 in the initramfs. Assembles the overlay root and hands over to systemd.
#
# The Hangar kernel is monolithic: overlayfs, ext4, virtiofs and virtio-blk are
# all built in, so this script mounts and pivots with no module loading.
#
#   lowerdir   the base image, read-only
#   upperdir   the per-environment writable layer, on a virtio-blk disk
#
# The base arrives over virtiofs when the host exported one: the host already
# holds it unpacked as a directory, so nothing is converted into a disk image
# and environments sharing a base share its page cache. A base on a block
# device is the fallback for a host that exported none, and shifts the
# writable layer along one device.
#
# /var/lib/docker is NOT part of this overlay: overlay2 cannot stack on
# overlayfs, so it gets its own filesystem mounted by label from fstab.

/bin/busybox --install -s /bin 2>/dev/null
mount -t proc     none /proc
mount -t sysfs    none /sys
mount -t devtmpfs none /dev

fail() {
    echo "INITRAMFS: $*"
    echo "INITRAMFS: dropping to a shell"
    exec /bin/busybox sh
}

# hangar-base is the tag internal/vm gives the vhost-user-fs device.
if mount -t virtiofs -o ro hangar-base /base 2>/dev/null; then
    echo "INITRAMFS: base over virtiofs"
    rw=/dev/vda
else
    mount -o ro /dev/vda /base || fail "no virtiofs base, and /dev/vda will not mount either"
    echo "INITRAMFS: base on /dev/vda"
    rw=/dev/vdb
fi

mount "$rw" /rw || fail "cannot mount the writable layer ($rw)"

mkdir -p /rw/upper /rw/work
mount -t overlay overlay \
      -o lowerdir=/base,upperdir=/rw/upper,workdir=/rw/work \
      /root || fail "cannot stack overlayfs"

# Keep the layers reachable in the new root rather than orphaning the mounts.
mkdir -p /root/run/hangar/base /root/run/hangar/rw
mount --move /base /root/run/hangar/base
mount --move /rw   /root/run/hangar/rw

exec switch_root /root /sbin/init
