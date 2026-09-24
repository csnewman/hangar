#!/bin/sh
# Turns an unpacked rootfs at /r into an image, writing to /out. Runs inside a
# throwaway Linux container.
#
# Inputs:  /r            unpacked root filesystem
# Outputs: /out/rootfs   the image: its root filesystem, as a directory
#
# An image is a root filesystem and nothing else. A worker exports it to its
# environments over virtio-fs; the kernel, the initramfs and the agent are the
# node's, so an image carries no kernel, no /lib/modules, no bootloader, no
# initramfs and no agent.
#
# Owners, modes, links and extended attributes are the image, and virtio-fs
# serves them as they are: /out must be a Linux filesystem, and the tree stays
# root-owned. A bind mount that maps owners -- a macOS host's, say -- loses
# them, and sudo in the guest with them.
set -eu

rm -rf /out/rootfs /out/rootfs.new
mkdir /out/rootfs.new
cp -a /r/. /out/rootfs.new/
mv /out/rootfs.new /out/rootfs
echo "rootfs: $(du -sh /out/rootfs | cut -f1)" >&2
