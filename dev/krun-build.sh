#!/bin/sh
# Builds what a worker on macOS needs beside itself, from source: libkrun,
# hangar-krun against it, and mkfs.ext4 (e2fsprogs) for environments'
# writable disks. Run on the Mac:
#
#   dev/krun-build.sh       out/krun/bin/{hangar-krun,libkrun.1.dylib,mkfs.ext4}
#
# libkrun is built without its guest init (Hangar's agent is init) and
# with block and network devices. hangar-krun is signed with the
# hypervisor entitlement, without which macOS refuses it a VM.
set -eu
version=v1.19.6
e2fsprogs=1.47.3
repo=$(cd "$(dirname "$0")/.." && pwd)
out=$repo/out/krun
src=$out/libkrun
mkdir -p "$out/bin"
if [ ! -d "$src/.git" ] || [ "$(git -C "$src" describe --tags 2>/dev/null)" != "$version" ]; then
    rm -rf "$src"
    # Plain HTTPS whatever the global config rewrites GitHub URLs to.
    GIT_CONFIG_GLOBAL=/dev/null git clone -q --depth 1 --branch "$version" \
        https://github.com/libkrun/libkrun.git "$src"
fi
# bindgen needs libclang, and its build scripts load it at run time.
clang=$(xcode-select -p)/Toolchains/XcodeDefault.xctoolchain/usr/lib
(cd "$src" && LIBCLANG_PATH=$clang DYLD_FALLBACK_LIBRARY_PATH=$clang \
    cargo build -q --release -p libkrun --no-default-features --features blk,net)
cp "$src/target/release/libkrun.dylib" "$out/bin/libkrun.1.dylib"
install_name_tool -id @rpath/libkrun.1.dylib "$out/bin/libkrun.1.dylib"
codesign -f -s - "$out/bin/libkrun.1.dylib" 2>/dev/null
ln -sf libkrun.1.dylib "$out/bin/libkrun.dylib"

cd "$repo"
CGO_CFLAGS="-I$src/include" CGO_LDFLAGS="-L$out/bin -Wl,-rpath,@executable_path" \
    go build -tags krun -o "$out/bin/hangar-krun" ./cmd/hangar-krun
cat > "$out/entitlements.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>com.apple.security.hypervisor</key><true/></dict></plist>
PLIST
codesign -f -s - --entitlements "$out/entitlements.plist" "$out/bin/hangar-krun" 2>/dev/null

e2=$out/e2fsprogs-$e2fsprogs
if [ ! -x "$out/bin/mkfs.ext4" ]; then
    curl -fsSL https://mirrors.edge.kernel.org/pub/linux/kernel/people/tytso/e2fsprogs/v$e2fsprogs/e2fsprogs-$e2fsprogs.tar.gz |
        tar xz -C "$out"
    (cd "$e2" && ./configure -q --disable-nls --disable-fuse2fs --disable-e2initrd-helper >/dev/null &&
        make -s -j8 libs >/dev/null && make -s -C misc -j8 mke2fs >/dev/null)
    # Named so, mke2fs makes ext4 without being told.
    cp "$e2/misc/mke2fs" "$out/bin/mkfs.ext4"
fi
echo "built $out/bin/hangar-krun and mkfs.ext4"
