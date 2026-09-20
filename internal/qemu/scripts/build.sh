#!/bin/sh
# Builds the Hangar QEMU inside a throwaway container.
#
# Inputs:  /cfg/devices.mak          the committed device selection
#          /cfg/devices-$QARCH.mak   its architecture-specific half
#          QEMU_VERSION QARCH JOBS
# Outputs: /out/qemu-system-$QARCH   the binary
#          /out/share/               firmware, keymaps: QEMU's datadir
#          /out/devices              the device list the binary actually has
#          /out/deps                 its shared library dependencies
#
# The source is downloaded here and never committed; only the device
# selection is source.
set -eu

export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends \
        build-essential ninja-build meson pkg-config python3 python3-venv \
        libglib2.0-dev libpixman-1-dev zlib1g-dev \
        libfdt-dev \
        libvirglrenderer-dev libepoxy-dev libgbm-dev libdrm-dev \
        libseccomp-dev libslirp-dev libcap-ng-dev \
        curl ca-certificates xz-utils bison flex >/dev/null
# libfdt-dev matters specifically: the arm `virt` machine needs device-tree
# support, and without the system library meson falls back to fetching the
# dtc subproject over git, which fails in a container with no git and would
# be a network dependency at configure time even where it works.

: "${QEMU_VERSION:?}"
: "${QARCH:?}"
JOBS="${JOBS:-$(nproc)}"

url="https://download.qemu.org/qemu-${QEMU_VERSION}.tar.xz"
mkdir -p /src && cd /src
tarball="qemu-${QEMU_VERSION}.tar.xz"
if [ -f "/cache/$tarball" ]; then
    echo "qemu: using cached $tarball" >&2
    cp "/cache/$tarball" qemu.tar.xz
else
    echo "qemu: fetching $url" >&2
    curl -fsSL "$url" -o qemu.tar.xz
    if [ -d /cache ]; then cp qemu.tar.xz "/cache/$tarball"; fi
fi
tar -xf qemu.tar.xz
cd "qemu-${QEMU_VERSION}"

target="${QARCH}-softmmu"

# Replace the target's device selection outright. The stock file pulls in
# every board QEMU supports for the architecture; ours names only what a
# Hangar guest can address.
mkdir -p "configs/devices/${target}"
cat /cfg/devices.mak "/cfg/devices-${QARCH}.mak" \
    > "configs/devices/${target}/default.mak"
echo "qemu: device selection" >&2
grep -c '=y$' "configs/devices/${target}/default.mak" | sed 's/^/  symbols: /' >&2

# --without-default-devices is what makes the file above authoritative.
# --disable-tcg drops the emulator: Hangar requires KVM, and an interpreter
#   for every guest instruction is a large piece of attack surface that can
#   never run.
# --enable-virglrenderer is the reason this build exists at all; a distro
#   QEMU is typically not linked against it, so virtio-gpu has no GL variant.
# --disable-nitro drops AWS Nitro Enclave support, which Hangar has no use
#   for. It also has to go: its accelerator is compiled whenever the
#   accelerator is enabled, but the device it calls into is `default y`, and
#   --without-default-devices suppresses exactly those defaults. The result
#   links against a symbol that was never built.
# --enable-seccomp confines QEMU itself, which matters when the thing on the
#   other side of the device model is hostile.
./configure \
    --target-list="${target}" \
    --without-default-devices \
    --disable-tcg \
    --disable-nitro \
    --enable-kvm \
    --enable-virglrenderer \
    --enable-vhost-user \
    --enable-seccomp \
    --enable-slirp \
    --disable-docs \
    --disable-guest-agent \
    --disable-tools \
    --prefix=/usr/local \
    >/tmp/configure.log 2>&1 || {
        echo "qemu: configure failed" >&2
        tail -30 /tmp/configure.log >&2
        exit 1
    }

# Build the system binary specifically, rather than `make`, which also builds
# QEMU's unit tests. They are not shipped, they roughly double the build, and
# a trimmed build can fail to link one of them for reasons that say nothing
# about the binary we want.
#
# Keep the output rather than discarding it: a compile failure in a trimmed
# build is usually a device selected without its dependency, and the message
# naming it is the whole diagnosis.
ninja -C build "qemu-system-${QARCH}" >/tmp/build.log 2>&1 || {
    echo "qemu: build failed" >&2
    grep -aE "error:|undefined reference|collect2|^FAILED:" /tmp/build.log | tail -20 >&2
    exit 1
}

# --no-rebuild installs what was just built instead of insisting on `all`,
# which would drag the tests back in.
meson install -C build --no-rebuild --destdir /stage >/dev/null 2>&1 || {
    echo "qemu: meson install failed, copying the binary and firmware directly" >&2
    mkdir -p /stage/usr/local/bin /stage/usr/local/share/qemu
    cp "build/qemu-system-${QARCH}" /stage/usr/local/bin/
    cp -a pc-bios/. /stage/usr/local/share/qemu/ 2>/dev/null || true
}

bin="/stage/usr/local/bin/qemu-system-${QARCH}"
[ -x "$bin" ] || { echo "qemu: no binary at $bin" >&2; exit 1; }

# Everything below is verification. Kconfig drops a symbol whose dependencies
# are unmet without failing, and configure warns rather than errors when an
# optional feature cannot be enabled, so neither the device list nor
# virglrenderer can be assumed from the flags above.
"$bin" -device help > /out/devices 2>&1 || true

required="
virtio-blk-pci
virtio-net-pci
virtio-serial-pci
virtio-rng-pci
virtio-balloon-pci
virtio-mem-pci
virtio-gpu-pci
virtio-gpu-gl-pci
vhost-vsock-pci
vhost-user-vsock-pci
vhost-user-fs-pci
"
missing=""
for dev in $required; do
    grep -q "\"${dev}\"" /out/devices || missing="$missing $dev"
done
if [ -n "$missing" ]; then
    echo "qemu: these devices were asked for but are not in the binary:$missing" >&2
    echo "qemu: built devices were:" >&2
    grep -oE '^name "[^"]+"' /out/devices | sed 's/^/  /' >&2
    exit 1
fi
echo "qemu: all required devices present" >&2

# virtio-gpu-gl only exists when virglrenderer was found, so its presence in
# the list above already proves the link -- but say so explicitly, because a
# silently unaccelerated build is the exact failure this build exists to stop.
if ! ldd "$bin" | grep -q virglrenderer; then
    echo "qemu: not linked against virglrenderer" >&2
    exit 1
fi
echo "qemu: linked against virglrenderer" >&2

# The shared-memory backend vhost-user needs, and the machine type.
case "$QARCH" in
  aarch64) machine=virt ;;
  x86_64)  machine=microvm ;;
esac
"$bin" -machine help 2>&1 | grep -q "^${machine} " || {
    echo "qemu: machine ${machine} is missing" >&2; exit 1; }
"$bin" -object help 2>&1 | grep -q memory-backend-memfd || {
    echo "qemu: memory-backend-memfd is missing" >&2; exit 1; }
echo "qemu: machine ${machine} and memory-backend-memfd present" >&2

mkdir -p /out/share
cp "$bin" "/out/qemu-system-${QARCH}"
if [ -d /stage/usr/local/share/qemu ]; then
    cp -a /stage/usr/local/share/qemu/. /out/share/
fi
ldd "$bin" > /out/deps 2>&1 || true

if [ -n "${OUT_UID:-}" ] && [ -n "${OUT_GID:-}" ]; then
    chown -R "$OUT_UID:$OUT_GID" /out
fi

echo "qemu: built $("$bin" --version | head -1)" >&2
echo "qemu: $(stat -c%s "/out/qemu-system-${QARCH}") bytes, $(grep -c '^name ' /out/devices) devices" >&2
