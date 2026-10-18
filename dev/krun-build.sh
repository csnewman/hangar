#!/bin/sh
# Builds what a worker on macOS needs beside itself, from source: libkrun,
# with virglrenderer for a Venus GPU, hangar-krun against it, and mkfs.ext4
# (e2fsprogs) for environments' writable disks. Run on the Mac:
#
#   dev/krun-build.sh       everything in out/krun/bin
#
# The GPU needs the Vulkan SDK, found at $VULKAN_SDK or the newest under
# ~/VulkanSDK, and libepoxy and meson (brew install libepoxy meson); without
# the SDK libkrun is built without a GPU. virglrenderer reaches Vulkan
# through the SDK's loader, and the drivers it can use -- KosmicKrisp and
# MoltenVK -- are bundled with a manifest each in bin/vulkan, which
# hangar-krun picks between. The libraries are copied beside hangar-krun and
# loaded from there. libkrun is built without
# its guest init (Hangar's agent is init); hangar-krun is signed with the
# hypervisor entitlement, without which macOS refuses it a VM.
set -eu
version=v1.19.6
virgl=0.10.4e-krunkit
e2fsprogs=1.47.3
repo=$(cd "$(dirname "$0")/.." && pwd)
out=$repo/out/krun
bin=$out/bin
src=$out/libkrun
mkdir -p "$bin"

sdk=${VULKAN_SDK:-$(ls -d "$HOME"/VulkanSDK/*/macOS 2>/dev/null | sort -V | tail -1)}
features=blk,net
if [ -n "$sdk" ] && [ -f "$sdk/lib/libvulkan.1.dylib" ]; then
    features=blk,net,gpu
    vdir=$out/virglrenderer-$virgl
    # Built again whenever the SDK it was built against changes.
    if [ ! -f "$out/virgl/lib/libvirglrenderer.1.dylib" ] || [ "$(cat "$out/virgl/sdk" 2>/dev/null)" != "$sdk" ]; then
        rm -rf "$vdir" "$out/virgl"
        curl -fsSL "https://gitlab.freedesktop.org/slp/virglrenderer/-/archive/$virgl/virglrenderer-$virgl.tar.gz" |
            tar xz -C "$out"
        # This branch links MoltenVK itself: it is linked to the loader,
        # so the driver is chosen when hangar-krun starts.
        sed -i '' -e "s|'-lMoltenVK'|'-lvulkan'|" \
            -e "s|-I/opt/homebrew/opt/molten-vk/libexec/include|-I$sdk/include|" "$vdir/meson.build"
        (cd "$vdir" && LIBRARY_PATH=$sdk/lib PKG_CONFIG_PATH=$sdk/lib/pkgconfig:/opt/homebrew/lib/pkgconfig \
            meson setup build --prefix="$out/virgl" -Dvenus=true -Drender-server=false --buildtype=release >/dev/null &&
            LIBRARY_PATH=$sdk/lib meson compile -C build >/dev/null && meson install -C build >/dev/null)
        echo "$sdk" > "$out/virgl/sdk"
    fi
else
    echo "no Vulkan SDK: building libkrun without a GPU" >&2
fi

if [ ! -d "$src/.git" ] || [ "$(git -C "$src" describe --tags 2>/dev/null)" != "$version" ]; then
    rm -rf "$src"
    # Plain HTTPS whatever the global config rewrites GitHub URLs to.
    GIT_CONFIG_GLOBAL=/dev/null git clone -q --depth 1 --branch "$version" \
        https://github.com/libkrun/libkrun.git "$src"
fi
# bindgen needs libclang, and its build scripts load it at run time.
clang=$(xcode-select -p)/Toolchains/XcodeDefault.xctoolchain/usr/lib
(cd "$src" && LIBRARY_PATH=$out/virgl/lib PKG_CONFIG_PATH=$out/virgl/lib/pkgconfig \
    LIBCLANG_PATH=$clang DYLD_FALLBACK_LIBRARY_PATH=$clang \
    cargo build -q --release -p libkrun --no-default-features --features "$features")

# Each library beside hangar-krun, found there by @rpath, and what it
# links to rewritten to match.
install_lib() { # source, name
    cp -f "$1" "$bin/$2"
    chmod u+w "$bin/$2"
    install_name_tool -id "@rpath/$2" "$bin/$2" 2>/dev/null
}
install_lib "$src/target/release/libkrun.dylib" libkrun.1.dylib
ln -sf libkrun.1.dylib "$bin/libkrun.dylib"
if [ "$features" = blk,net,gpu ]; then
    install_lib "$out/virgl/lib/libvirglrenderer.1.dylib" libvirglrenderer.1.dylib
    install_lib "$sdk/lib/libvulkan.1.dylib" libvulkan.1.dylib
    install_lib "$sdk/lib/libMoltenVK.dylib" libMoltenVK.dylib
    # The drivers' manifests, MoltenVK's as an ordinary driver: Venus does
    # not ask the loader for portability drivers.
    mkdir -p "$bin/vulkan"
    printf '{"file_format_version": "1.0.0", "ICD": {"library_path": "../libMoltenVK.dylib", "api_version": "1.4.0"}}\n' \
        > "$bin/vulkan/moltenvk.json"
    if [ -f "$sdk/lib/libvulkan_kosmickrisp.dylib" ]; then
        install_lib "$sdk/lib/libvulkan_kosmickrisp.dylib" libvulkan_kosmickrisp.dylib
        install_name_tool -add_rpath @loader_path "$bin/libvulkan_kosmickrisp.dylib" 2>/dev/null || true
        printf '{"file_format_version": "1.0.0", "ICD": {"library_path": "../libvulkan_kosmickrisp.dylib", "api_version": "1.4.0"}}\n' \
            > "$bin/vulkan/kosmickrisp.json"
    fi
    epoxy=$(otool -L "$bin/libvirglrenderer.1.dylib" | awk '/libepoxy/ {print $1}')
    install_lib "$epoxy" libepoxy.0.dylib
    install_name_tool -change "$out/virgl/lib/libvirglrenderer.1.dylib" @rpath/libvirglrenderer.1.dylib \
        "$bin/libkrun.1.dylib"
    install_name_tool -change "$epoxy" @rpath/libepoxy.0.dylib "$bin/libvirglrenderer.1.dylib"
    for l in libkrun.1.dylib libvirglrenderer.1.dylib; do
        install_name_tool -add_rpath @loader_path "$bin/$l" 2>/dev/null || true
    done
fi
for l in "$bin"/*.dylib; do
    [ -L "$l" ] || codesign -f -s - "$l" 2>/dev/null
done

cd "$repo"
CGO_CFLAGS="-I$src/include" CGO_LDFLAGS="-L$bin -Wl,-rpath,@executable_path" \
    go build -tags krun -o "$bin/hangar-krun" ./cmd/hangar-krun 2>&1 | grep -v 'duplicate -rpath\|^# ' || true
cat > "$out/entitlements.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>com.apple.security.hypervisor</key><true/></dict></plist>
PLIST
codesign -f -s - --entitlements "$out/entitlements.plist" "$bin/hangar-krun" 2>/dev/null

e2=$out/e2fsprogs-$e2fsprogs
if [ ! -x "$bin/mkfs.ext4" ]; then
    curl -fsSL https://mirrors.edge.kernel.org/pub/linux/kernel/people/tytso/e2fsprogs/v$e2fsprogs/e2fsprogs-$e2fsprogs.tar.gz |
        tar xz -C "$out"
    (cd "$e2" && ./configure -q --disable-nls --disable-fuse2fs --disable-e2initrd-helper >/dev/null &&
        make -s -j8 libs >/dev/null && make -s -C misc -j8 mke2fs >/dev/null)
    # Named so, mke2fs makes ext4 without being told.
    cp "$e2/misc/mke2fs" "$bin/mkfs.ext4"
fi
echo "built $bin: hangar-krun (libkrun $features) and mkfs.ext4"
