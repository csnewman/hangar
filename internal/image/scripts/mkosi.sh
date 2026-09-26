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
        zstd gzip python3-minimal >/dev/null

# mkosi needs a 65536-UID range to build unprivileged inside its own namespace.
printf 'root:100000:65536\n' > /etc/subuid
printf 'root:100000:65536\n' > /etc/subgid

# Build in /work, a volume on the Docker host's own filesystem, rather than
# the bind mount or the container's overlay: mkosi's tar extraction needs to
# set ownership the mount may not be able to represent, and the overlay it
# stacks for build packages cannot have its upper layer on another overlay.
# /cfg is the whole images/ directory, not just one distribution, so that a
# config can reference a sibling with a relative path -- ../../common is
# shared by every tier. IMAGE names the distribution to build.
#
# Its tiers are built in order, each on the tree of the one before:
# base/mkosi.conf names ../minimal/rootfs as its BaseTrees.
: "${IMAGE:?}"
cp -a /cfg /work/src
mkdir -p /work/workspace
for tier in minimal base; do
	(cd "/work/src/$IMAGE/$tier" && mkosi --force --workspace-directory=/work/workspace build)
done
cd "/work/src/$IMAGE/base"

# --- guest programs ---------------------------------------------------------
# Native programs Hangar ships in an image are compiled here rather than in the
# image, so the image carries no toolchain. The builder is the same
# distribution and release as the Ubuntu base, so what links here runs there.
# They go into base, the tier with Mesa; a base whose libraries live elsewhere
# -- Rocky's, in /usr/lib64 -- goes without.
if ls rootfs/usr/lib/*/libEGL.so.1 >/dev/null 2>&1; then
    apt-get install -y -qq --no-install-recommends gcc libc6-dev libegl-dev libgles-dev >/dev/null
    install -d rootfs/usr/local/bin
    gcc -O2 -Wall -Wextra -o rootfs/usr/local/bin/hangar-glcheck \
        /cfg/common/src/hangar-glcheck.c -lEGL -lGLESv2
    echo "glcheck: installed" >&2
fi
if ls rootfs/usr/lib/*/libvulkan.so.1 >/dev/null 2>&1; then
    apt-get install -y -qq --no-install-recommends gcc libc6-dev libvulkan-dev glslang-tools >/dev/null
    install -d rootfs/usr/local/bin /tmp/vkcheck
    for s in vert frag; do
        glslangValidator -V --vn "vkcheck_$s" -o "/tmp/vkcheck/vkcheck_$s.h" "/cfg/common/src/vkcheck.$s" >/dev/null
    done
    gcc -O2 -Wall -Wextra -I/tmp/vkcheck -o rootfs/usr/local/bin/hangar-vkcheck \
        /cfg/common/src/hangar-vkcheck.c -lvulkan
    echo "vkcheck: installed" >&2
fi

for tier in minimal base; do
	sh /scripts/postprocess.sh "/work/src/$IMAGE/$tier/rootfs" "/out/$tier"
done
