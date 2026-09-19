#!/bin/busybox sh
# PID 1 in the initramfs. Assembles the overlay root and hands over to systemd.
#
# The Hangar kernel is monolithic: overlayfs, ext4 and virtio-blk are built in,
# so this script mounts and pivots with no module loading anywhere.
#
#   /dev/vda  read-only base image   -> lowerdir
#   /dev/vdb  per-environment layer  -> upperdir + workdir
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

mount -o ro /dev/vda /base || fail "cannot mount base (/dev/vda)"
mount        /dev/vdb /rw  || fail "cannot mount upper (/dev/vdb)"

mkdir -p /rw/upper /rw/work
mount -t overlay overlay \
      -o lowerdir=/base,upperdir=/rw/upper,workdir=/rw/work \
      /root || fail "cannot stack overlayfs"

# Keep the layers reachable in the new root rather than orphaning the mounts.
mkdir -p /root/run/hangar/base /root/run/hangar/rw
mount --move /base /root/run/hangar/base
mount --move /rw   /root/run/hangar/rw

exec switch_root /root /sbin/init
