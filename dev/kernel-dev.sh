#!/bin/sh
# Builds the guest kernel incrementally in the Lima VM, for working on
# Hangar's kernel code (internal/kernel/tree, patches): the first build takes
# as long as `hangar kernel`, later ones a minute or two. Run from the Mac:
#
#   dev/kernel-dev.sh       build; the kernel is out/kernel-dev/vmlinuz
#   dev/kernel-dev.sh 16k   16 KiB pages, for a worker on a Mac (libkrun);
#                           the kernel is out/kernel-dev-16k/vmlinuz
#
# Point a worker config's vm.kernel at it to boot environments on it. The
# tree is ~/kbuild/linux-<version> in the VM, with the version, config and
# patches `hangar kernel` uses; delete it to start again.
set -eu
repo=/Users/csnewman/Projects/hangar
# Each variant has a tree of its own, so building one leaves the other's
# objects as they are.
variant=${1:-}
case "$variant" in
'') extra= out=kernel-dev suffix= ;;
16k) extra=$repo/internal/kernel/hangar-arm64-16k.config out=kernel-dev-16k suffix=-16k ;;
*) echo "usage: dev/kernel-dev.sh [16k]" >&2; exit 2 ;;
esac
limactl shell hangar bash -s <<SCRIPT
set -eu
v=\$(cat $repo/internal/kernel/version)
k=~/kbuild/linux-\$v$suffix
if [ ! -d \$k ]; then
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq build-essential flex bison \
        libssl-dev libelf-dev bc dwarves cpio rsync zstd >/dev/null
    mkdir -p ~/kbuild
    mkdir -p \$k
    curl -fsSL https://cdn.kernel.org/pub/linux/kernel/v\${v%%.*}.x/linux-\$v.tar.xz | tar xJ -C \$k --strip-components=1
    cd \$k
    git init -q && git add -A && git -c user.name=b -c user.email=b@localhost commit -qm upstream
    for p in $repo/internal/kernel/patches/*.patch; do [ -e "\$p" ] && patch -p1 --forward --fuzz=0 < "\$p" >/dev/null; done
fi
cd \$k
rsync -a $repo/internal/kernel/tree/ ./
# The config is made afresh from the fragments each time, so a change to
# them is built; what it leaves the same is not rebuilt.
make ARCH=arm64 tinyconfig >/dev/null
./scripts/kconfig/merge_config.sh -m -O . .config $repo/internal/kernel/hangar.config \
    $repo/internal/kernel/hangar-arm64.config $extra >/dev/null 2>&1 || true
make ARCH=arm64 olddefconfig >/dev/null
for o in \$(grep -h '^CONFIG_.*=y' $repo/internal/kernel/hangar.config $repo/internal/kernel/hangar-arm64.config $extra); do
    grep -qx "\$o" .config || echo "not built in: \$o"
done
s=\$(date +%s)
make ARCH=arm64 -j\$(nproc) Image > /var/tmp/kernel-dev.log 2>&1 || { grep -E "error|Error" /var/tmp/kernel-dev.log | head -20; exit 1; }
mkdir -p $repo/out/$out && cp arch/arm64/boot/Image $repo/out/$out/vmlinuz
echo "built in \$(( \$(date +%s) - s ))s: out/$out/vmlinuz"
SCRIPT
