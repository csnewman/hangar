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

# Build on the container filesystem rather than the bind mount: mkosi's tar
# extraction needs to set ownership the mount may not be able to represent.
# /cfg is the whole images/ directory, not just one image, so that a config can
# reference a sibling with a relative path -- ../common/tree is shared by every
# base. IMAGE names the subdirectory to build.
: "${IMAGE:?}"
cp -a /cfg /build
cd "/build/$IMAGE"
mkosi --force build

# --- guest programs ---------------------------------------------------------
# Native programs Hangar ships in an image are compiled here rather than in the
# image, so the image carries no toolchain. The builder is the same
# distribution and release as the Ubuntu base, so what links here runs there.
# A base without Mesa -- Rocky, today -- has nothing for these to link against
# and goes without.
if ls rootfs/usr/lib/*/libEGL.so.1 >/dev/null 2>&1; then
    apt-get install -y -qq --no-install-recommends gcc libc6-dev libegl-dev libgles-dev >/dev/null
    install -d rootfs/usr/local/bin
    gcc -O2 -Wall -Wextra -o rootfs/usr/local/bin/hangar-glcheck \
        /cfg/common/src/hangar-glcheck.c -lEGL -lGLESv2
    echo "glcheck: installed" >&2
fi

ln -s "/build/$IMAGE/rootfs" /r
exec sh /scripts/postprocess.sh
