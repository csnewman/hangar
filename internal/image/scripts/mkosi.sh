#!/bin/sh
# mkosi builder: build an OS image from distribution packages, then hand over to
# postprocess.sh.
#
# mkosi produces a real OS image: udev is present (on Debian/Ubuntu it is a
# separate package from systemd, and without it the guest has no network) and
# there are no container markers for systemd to misread as container mode.
set -eu
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
# mkosi drives the *target* distribution's package manager, so the builder needs
# both toolchains: apt/dpkg for Debian-family targets, dnf/rpm for RHEL-family.
apt-get install -y -qq --no-install-recommends \
        mkosi ubuntu-keyring dpkg-dev apt-utils uidmap \
        dnf rpm ca-certificates \
        e2fsprogs zstd gzip python3-minimal busybox-static cpio >/dev/null

# mkosi needs a 65536-UID range to build unprivileged inside its own namespace.
printf 'root:100000:65536\n' > /etc/subuid
printf 'root:100000:65536\n' > /etc/subgid

# Build on the container filesystem: a macOS bind mount cannot represent Linux
# ownership, and mkosi's tar extraction fails on it.
# /cfg is the whole images/ directory, not just one image, so that a config can
# reference a sibling with a relative path -- ../common/tree is shared by every
# base. IMAGE names the subdirectory to build.
: "${IMAGE:?}"
cp -a /cfg /build
cd "/build/$IMAGE"
mkosi --force build

ln -s "/build/$IMAGE/rootfs" /r
exec sh /scripts/postprocess.sh
