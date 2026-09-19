#!/bin/sh
# Turns an unpacked rootfs at /r into the artefacts a VM needs, writing to /out.
# Shared by both builders. Runs inside a throwaway Linux container.
#
# Inputs:  /r            unpacked root filesystem
#          /scripts      this directory
#          SIZE_GB UPPER_GB DOCKER_GB
# Outputs: /out/initrd.img /out/base.ext4 /out/upper.ext4 /out/docker.ext4
#
# The output is a root filesystem and an initramfs. The guest kernel is built
# separately by `hangar kernel`, once per architecture, and shared by every
# image, so an image carries no kernel, no /lib/modules and no bootloader.
set -eu

# --- disks -------------------------------------------------------------------
rm -f /out/base.ext4 /out/upper.ext4 /out/docker.ext4
mke2fs -q -t ext4 -L hangar-base   -d /r -F /out/base.ext4   "${SIZE_GB}G"
mke2fs -q -t ext4 -L hangar-upper        -F /out/upper.ext4  "${UPPER_GB}G"
# overlay2 cannot stack on overlayfs, and the root IS an overlay, so the docker
# image store needs a real filesystem of its own. Mounted by label from fstab.
mke2fs -q -t ext4 -L hangar-docker       -F /out/docker.ext4 "${DOCKER_GB}G"

# --- initramfs ---------------------------------------------------------------
mkdir -p /i/bin /i/dev /i/proc /i/sys /i/base /i/rw /i/root
cp /bin/busybox /i/bin/busybox
cp /scripts/initramfs-init.sh /i/init
chmod +x /i/init

( cd /i && find . | cpio -H newc -o --quiet | gzip -9 ) > /out/initrd.img
echo "initramfs: $(stat -c%s /out/initrd.img) bytes" >&2
