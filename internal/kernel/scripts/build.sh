#!/bin/sh
# Builds the Hangar guest kernel inside a throwaway container.
#
# Inputs:  /cfg/hangar.config          the committed config fragment
#          /cfg/tree                    source files added to the kernel tree
#          /cfg/patches                 patches applied to it
#          /cfg/hangar-$KARCH.config   its architecture-specific half
#          /cfg/version                 the kernel version, unless
#                                       KERNEL_VERSION names another
#          KARCH JOBS
# Outputs: /out/vmlinuz         raw Image (arm64) or bzImage (x86_64)
#          /out/config          the resolved .config, for the record
#
# There is no modules directory: the kernel is monolithic (see hangar.config),
# which is asserted below rather than assumed.
#
# The kernel source is downloaded here and never committed; only the fragment
# is source.
set -eu

export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends \
        build-essential flex bison libssl-dev libelf-dev bc kmod cpio rsync \
        zstd xz-utils curl ca-certificates python3-minimal patch dwarves >/dev/null

KERNEL_VERSION="${KERNEL_VERSION:-$(cat /cfg/version)}"
: "${KARCH:?}"
JOBS="${JOBS:-$(nproc)}"

series="v$(echo "$KERNEL_VERSION" | cut -d. -f1).x"
url="https://cdn.kernel.org/pub/linux/kernel/${series}/linux-${KERNEL_VERSION}.tar.xz"

mkdir -p /src && cd /src
tarball="linux-${KERNEL_VERSION}.tar.xz"
if [ -f "/cache/$tarball" ]; then
    echo "kernel: using cached $tarball" >&2
    cp "/cache/$tarball" linux.tar.xz
else
    echo "kernel: fetching $url" >&2
    curl -fsSL "$url" -o linux.tar.xz
    if [ -d /cache ]; then cp linux.tar.xz "/cache/$tarball"; fi
fi
tar -xf linux.tar.xz
cd "linux-${KERNEL_VERSION}"

# Hangar's own source files (fs/hangar and its headers), at their paths in
# the tree; the patches that follow hook them in.
if [ -d /cfg/tree ]; then
    cp -a /cfg/tree/. .
fi

# Fixes carried against the upstream tree. Each one is expected to apply
# cleanly; a rejected hunk means the fix has landed upstream or the code has
# moved under it, and either way the build should stop rather than produce a
# kernel whose contents nobody can predict.
for p in /cfg/patches/*.patch; do
    [ -e "$p" ] || break
    echo "kernel: applying $(basename "$p")" >&2
    patch -p1 --forward --fuzz=0 <"$p" >/dev/null
done

case "$KARCH" in
  arm64)  image_path=arch/arm64/boot/Image ;;
  x86_64) image_path=arch/x86/boot/bzImage ;;
  *) echo "unsupported KARCH=$KARCH" >&2; exit 1 ;;
esac

# Every option at its default, with the fragments over it, for the check
# below. Made out of tree while the source tree is still clean.
mkdir -p /tmp/kdef
make ARCH="$KARCH" O=/tmp/kdef alldefconfig >/dev/null
./scripts/kconfig/merge_config.sh -m -O /tmp/kdef /tmp/kdef/.config \
        /cfg/hangar.config "/cfg/hangar-${KARCH}.config" >/dev/null 2>&1 || true
make ARCH="$KARCH" O=/tmp/kdef olddefconfig >/dev/null

echo "kernel: configuring ($KARCH, base=${KBASE:-tinyconfig})" >&2
make ARCH="$KARCH" "${KBASE:-tinyconfig}" >/dev/null

# hangar.config and its architecture's half go on top of the base.
# merge_config.sh warns but does not fail when an option cannot be satisfied,
# so the result is verified below rather than trusted.
./scripts/kconfig/merge_config.sh -m -O . .config \
        /cfg/hangar.config "/cfg/hangar-${KARCH}.config" >/dev/null 2>&1 || true
make ARCH="$KARCH" olddefconfig >/dev/null

# Every option the fragments ask for must survive olddefconfig.
#
# merge_config.sh warns but does not fail when a symbol is unknown or its
# dependencies are unmet, and olddefconfig then omits it entirely. A dropped
# option therefore leaves no trace in .config, and the symptom arrives much
# later and somewhere else: a kernel that panics mounting root, or a userspace
# daemon that fails for reasons that look nothing like a kernel config problem.
#
# The check compares the fragments against the resolved .config, so it covers
# every option asked for rather than the subset someone remembered to list.
cat /cfg/hangar.config "/cfg/hangar-${KARCH}.config" > /tmp/wanted.config
missing=""
unwanted=""
while IFS= read -r line; do
    case "$line" in
      CONFIG_*=y)
        opt=${line%=y}
        grep -q "^${opt}=y\$" .config || missing="$missing $opt"
        ;;
      "# CONFIG_"*" is not set")
        opt=${line#\# }
        opt=${opt% is not set}
        if grep -q "^${opt}=" .config; then unwanted="$unwanted $opt"; fi
        ;;
    esac
done < /tmp/wanted.config

if [ -n "$missing" ] || [ -n "$unwanted" ]; then
    [ -z "$missing" ] || {
        echo "kernel: requested but not built in:" >&2
        for opt in $missing; do
            state=$(grep -E "^(# )?${opt}[= ]" .config || true)
            echo "  ${opt}: ${state:-absent (unknown symbol, or its dependencies are unmet)}" >&2
        done
    }
    [ -z "$unwanted" ] || {
        echo "kernel: disabled in the fragment but enabled in the result:" >&2
        for opt in $unwanted; do grep -E "^${opt}=" .config >&2; done
    }
    exit 1
fi
echo "kernel: all $(grep -c "^CONFIG_.*=y\$" /tmp/wanted.config) requested options built in" >&2

# tinyconfig turns off every option that has a prompt, whatever its default,
# so an option on by default is off here unless a fragment turns it on. Each
# that stays off must be named off in a fragment: a default nobody looked at
# (a stack guard page, a CPU's errata) is otherwise lost without a trace, and
# a new kernel brings new ones.
lost=""
for opt in $(sed -n 's/=y$//p' /tmp/kdef/.config); do
    grep -q "^${opt}=y\$" .config && continue
    grep -q "^# ${opt} is not set\$" /tmp/wanted.config && continue
    lost="$lost $opt"
done
if [ -n "$lost" ]; then
    echo "kernel: on by default, off here, and not named off in a fragment:" >&2
    for opt in $lost; do echo "  $opt" >&2; done
    exit 1
fi

# Nothing may be a module: the guest has a fixed, known device model, so a
# module is either for hardware that cannot exist or for a feature we should
# have built in. CONFIG_MODULES=n also removes loading kernel code as a
# privilege-escalation path inside the guest.
if grep -q '^CONFIG_MODULES=y$' .config; then
    echo "kernel: CONFIG_MODULES is enabled; this kernel is meant to be monolithic" >&2
    exit 1
fi
mods=$(grep -c '=m$' .config || true)
if [ "$mods" != "0" ]; then
    echo "kernel: $mods options are still modules:" >&2
    grep '=m$' .config | head -20 >&2
    exit 1
fi
echo "kernel: monolithic, no modules" >&2

if [ -n "${CONFIG_ONLY:-}" ]; then
    cp .config /out/config
    echo "kernel: config-only, $(grep -c '=y$' .config) options built in" >&2
    exit 0
fi

echo "kernel: building with $JOBS jobs" >&2
make ARCH="$KARCH" -j"$JOBS" "$(basename $image_path)" >/dev/null

cp "$image_path" /out/vmlinuz
cp .config /out/config

kver=$(make ARCH="$KARCH" -s kernelrelease)
echo "$kver" > /out/kernelrelease
echo "kernel: built $kver, $(stat -c%s /out/vmlinuz) bytes" >&2
