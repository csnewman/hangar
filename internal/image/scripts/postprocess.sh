#!/bin/sh
# Turns an unpacked rootfs at /r into the artefacts a VM needs, writing to /out.
# Shared by both builders. Runs inside a throwaway Linux container.
#
# Inputs:  /r            unpacked root filesystem
#          /scripts      this directory
#          /agent        optional, holds hangar-agent
#          SIZE_GB UPPER_GB DOCKER_GB
#          OUT_UID OUT_GID  who should own the results
# Outputs: /out/initrd.img /out/base.ext4 /out/upper.ext4 /out/docker.ext4
#
# The output is a root filesystem and an initramfs. The guest kernel is built
# separately by `hangar kernel`, once per architecture, and shared by every
# image, so an image carries no kernel, no /lib/modules and no bootloader.
set -eu

# --- agent ---------------------------------------------------------------
# hangar-agent is built by the host and copied in, rather than installed from
# a package. The agent belongs to the node, not to the image: an environment
# built from an arbitrary Dockerfile gets the same agent as a Hangar base, and
# upgrading it does not mean rebuilding every image.
if [ -x /agent/hangar-agent ]; then
    install -D -m 0755 /agent/hangar-agent /r/usr/local/bin/hangar-agent
    echo "agent: installed $(stat -c%s /agent/hangar-agent) bytes" >&2
else
    echo "agent: none supplied, environment will have no control channel" >&2
fi

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

# --- ownership -------------------------------------------------------------
# This script runs as root in the builder, so everything it writes to the bind
# mount lands root-owned. The caller then cannot open the writable layers, and
# QEMU fails with a bare "Permission denied" that says nothing about why.
if [ -n "${OUT_UID:-}" ] && [ -n "${OUT_GID:-}" ]; then
    chown "$OUT_UID:$OUT_GID" /out/base.ext4 /out/upper.ext4 /out/docker.ext4 /out/initrd.img
fi
