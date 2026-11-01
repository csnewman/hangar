#!/bin/sh
# Builds what a worker running libkrun needs beside itself, from source:
# libkrun, with virglrenderer for a GPU, and hangar-krun against it. Run on
# the Mac:
#
#   dev/krun-build.sh        the Mac's, in out/krun/bin, with mkfs.ext4
#                            (e2fsprogs) for environments' writable disks
#   dev/krun-build.sh mac BRANCH
#                            the same, from a branch of the fork's clone in
#                            libkrun/ beside this checkout
#   dev/krun-build.sh linux  the Lima VM's, in out/krun-linux/bin
#   dev/krun-build.sh linux BRANCH
#                            the same, from a branch of the fork's clone in
#                            libkrun/ beside this checkout
#
# Both are libkrun 2.0 to be, from the hangar branch of Hangar's fork
# (krunRepo, pinned at krunHangar): libkrun's main with Hangar's changes merged
# in while upstream reviews them, each on a branch of its own in the fork.
# They are huge pages for guest RAM, surfaceless EGL, disks served in parallel,
# on a queue per vCPU and through io_uring for raw images, virtio-pci on arm64
# and x86_64 with queue notifications by ioeventfd and MSI-X, snapshots,
# virtio-mem, and on the Mac shared memory blobs mapped into the guest. Both
# link Hangar's virglrenderer (gpu/virglrenderer/build.sh), as hangar-gpu
# does: the Lima VM's own, and on the Mac one built here, Venus alone.
#
# The Mac's GPU needs the Vulkan SDK, found at $VULKAN_SDK or the newest under
# ~/VulkanSDK, and meson (brew install meson); without the SDK libkrun is
# built without a GPU. virglrenderer's render server reaches Vulkan through
# the SDK's loader, and the drivers it can use -- KosmicKrisp and MoltenVK --
# are bundled with a manifest each in bin/vulkan, which hangar-krun picks
# between. The libraries and the render server are copied beside hangar-krun
# and found there.
#
# KosmicKrisp is patched (dev/krun/kosmickrisp-drm-modifier.patch) so a
# guest can share GPU buffers between its programs, which OpenGL on Vulkan
# (zink) needs: images with a linear DRM format modifier, which virglrenderer
# places in the shared memory it offers the guest as dma-bufs. It is built
# from Mesa when its compilers are installed (brew install llvm libclc
# spirv-llvm-translator), and the SDK's own copy, without modifiers, is
# bundled otherwise. libkrun is built without its guest init (Hangar's agent
# is init); hangar-krun is signed with the hypervisor entitlement, without
# which macOS refuses it a VM.
set -eu
krunRepo=https://github.com/csnewman/libkrun.git
krunHangar=360706da8bad657ff456e71f07ed4070a57e1e5c
repo=$(cd "$(dirname "$0")/.." && pwd)
if [ "${1:-}" = linux ]; then
    exec limactl shell hangar sh "$repo/dev/krun-build.sh" in-vm "${2:-}"
fi
if [ "${1:-}" = in-vm ]; then
    . "$HOME/.cargo/env"
    src=$HOME/.cache/hangar-krun/libkrun
    bin=$repo/out/krun-linux/bin
    arch=$(uname -m)
    # bindgen loads libclang, and libkrun's build makes its own guest init,
    # which it links against musl, whether or not it is used.
    dpkg -s libclang-dev >/dev/null 2>&1 || sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq libclang-dev >/dev/null
    rustup target list --installed | grep -q "$arch-unknown-linux-musl" || rustup target add "$arch-unknown-linux-musl" >/dev/null
    if [ ! -d "$src/.git" ]; then
        mkdir -p "$(dirname "$src")"
        GIT_CONFIG_GLOBAL=/dev/null git clone -q "$krunRepo" "$src"
    fi
    from=$krunRepo branch=hangar want=$krunHangar
    if [ -n "${2:-}" ]; then
        # A branch of the fork's clone beside this checkout (libkrun/), to
        # try before it is pushed.
        from=$repo/libkrun branch=$2 want=$(git -C "$from" rev-parse "$2")
    fi
    if [ "$(git -C "$src" rev-parse HEAD)" != "$want" ]; then
        # Files left from another tree go too: the build's own are ignored.
        git -C "$src" checkout -q -- .
        git -C "$src" clean -fdq
        GIT_CONFIG_GLOBAL=/dev/null git -C "$src" fetch -q "$from" "$branch"
        git -C "$src" checkout -q "$want"
    fi
    # Built on the VM's own disk: the Mac's mount is slow for cargo.
    (cd "$src" && PKG_CONFIG_PATH=/usr/local/lib/$arch-linux-gnu/pkgconfig \
        make -s INIT_BLOB=0 BLK=1 NET=1 GPU=1 >/dev/null)
    mkdir -p "$bin"
    # Renamed into place: a machine running keeps the library it mapped,
    # which writing over it would crash.
    cp -f "$src/target/release/libkrun.so.2.0.0" "$bin/libkrun.so.2.new"
    mv -f "$bin/libkrun.so.2.new" "$bin/libkrun.so.2"
    ln -sf libkrun.so.2 "$bin/libkrun.so"
    cd "$repo"
    CGO_CFLAGS="-I$src/include" CGO_LDFLAGS="-L$bin -Wl,-rpath,\$ORIGIN" \
        /usr/local/go/bin/go build -tags krun -o "$bin/hangar-krun" ./cmd/hangar-krun
    echo "built $bin: hangar-krun (libkrun $branch $(echo $want | cut -c1-8))"
    exit
fi
e2fsprogs=1.47.3
mesa=c823a83e8f373a5c36c2b9ed1d7dc98a4f3360ee
out=$repo/out/krun
bin=$out/bin
src=$out/libkrun-hangar
mkdir -p "$bin"

# Mesa's generators and virglrenderer's need mako, and Mesa's PyYAML.
venv() {
    [ -x "$out/venv/bin/python3" ] || python3 -m venv "$out/venv"
    "$out/venv/bin/pip" -q install mako pyyaml packaging
}

sdk=${VULKAN_SDK:-$(ls -d "$HOME"/VulkanSDK/*/macOS 2>/dev/null | sort -V | tail -1)}
features=ffi,blk,net
if [ -n "$sdk" ] && [ -f "$sdk/lib/libvulkan.1.dylib" ]; then
    features=ffi,blk,net,gpu
    # Built again whenever the SDK it was built against, or a patch, changes.
    stamp="$sdk $(cat "$repo"/gpu/virglrenderer/build.sh "$repo"/gpu/virglrenderer/0*.patch | shasum | cut -d' ' -f1)"
    if [ ! -f "$out/virgl/lib/libvirglrenderer.1.dylib" ] || [ "$(cat "$out/virgl/stamp" 2>/dev/null)" != "$stamp" ]; then
        venv
        rm -rf "$out/virgl"
        PATH=$out/venv/bin:$PATH PKG_CONFIG_PATH=$sdk/lib/pkgconfig LIBRARY_PATH=$sdk/lib \
            sh "$repo/gpu/virglrenderer/build.sh" "$out/virgl" >/dev/null
        echo "$stamp" > "$out/virgl/stamp"
    fi

    # KosmicKrisp, from Mesa, with linear DRM format modifiers.
    kk=$out/kosmickrisp/libvulkan_kosmickrisp.dylib
    kpatch=$repo/dev/krun/kosmickrisp-drm-modifier.patch
    stamp="$mesa $(shasum "$kpatch" | cut -d' ' -f1)"
    if [ "$(cat "$out/kosmickrisp/stamp" 2>/dev/null)" != "$stamp" ] || [ ! -f "$kk" ]; then
        if brew --prefix --installed llvm libclc spirv-llvm-translator >/dev/null 2>&1; then
            mdir=$out/mesa-$mesa
            rm -rf "$mdir" "$out/kosmickrisp"
            curl -fsSL "https://gitlab.freedesktop.org/mesa/mesa/-/archive/$mesa/mesa-$mesa.tar.gz" |
                tar xz -C "$out"
            patch -s -d "$mdir" -p1 < "$kpatch"
            venv
            # The build runs the tools the setup found, LLVM's among them.
            (cd "$mdir" && export PATH=$out/venv/bin:$(brew --prefix llvm)/bin:$PATH &&
                PKG_CONFIG_PATH=$sdk/lib/pkgconfig \
                meson setup build --buildtype=release -Dplatforms=macos -Dvulkan-drivers=kosmickrisp \
                    -Dgallium-drivers= -Dopengl=false -Dzstd=disabled --prefer-static >/dev/null &&
                ninja -C build src/kosmickrisp/vulkan/libvulkan_kosmickrisp.dylib >/dev/null)
            mkdir -p "$out/kosmickrisp"
            cp "$mdir/build/src/kosmickrisp/vulkan/libvulkan_kosmickrisp.dylib" "$kk"
            echo "$stamp" > "$out/kosmickrisp/stamp"
        else
            echo "no llvm, libclc or spirv-llvm-translator: bundling the SDK's KosmicKrisp, whose guests draw their desktop in software" >&2
            kk=$sdk/lib/libvulkan_kosmickrisp.dylib
        fi
    fi
else
    echo "no Vulkan SDK: building libkrun without a GPU" >&2
fi

if [ ! -d "$src/.git" ]; then
    GIT_CONFIG_GLOBAL=/dev/null git clone -q "$krunRepo" "$src"
fi
from=$krunRepo branch=hangar want=$krunHangar
if [ "${1:-}" = mac ] && [ -n "${2:-}" ]; then
    from=$repo/libkrun branch=$2 want=$(git -C "$from" rev-parse "$2")
fi
if [ "$(git -C "$src" rev-parse HEAD)" != "$want" ]; then
    git -C "$src" checkout -q -- .
    git -C "$src" clean -fdq
    GIT_CONFIG_GLOBAL=/dev/null git -C "$src" fetch -q "$from" "$branch"
    git -C "$src" checkout -q "$want"
fi
# bindgen needs libclang, and its build scripts load it at run time.
clang=$(xcode-select -p)/Toolchains/XcodeDefault.xctoolchain/usr/lib
(cd "$src" && LIBRARY_PATH=$out/virgl/lib PKG_CONFIG_PATH=$out/virgl/lib/pkgconfig \
    LIBCLANG_PATH=$clang DYLD_FALLBACK_LIBRARY_PATH=$clang \
    cargo build -q --release -p libkrun --no-default-features --features "$features")

# Each library beside hangar-krun, found there by @rpath, and what it
# links to rewritten to match. Each is renamed into place: a machine
# running keeps the library it mapped, which writing over it would crash.
install_lib() { # source, name
    cp -f "$1" "$bin/$2.new"
    chmod u+w "$bin/$2.new"
    install_name_tool -id "@rpath/$2" "$bin/$2.new" 2>/dev/null
    mv -f "$bin/$2.new" "$bin/$2"
}
rm -f "$bin/libkrun.1.dylib" "$bin/libepoxy.0.dylib"
install_lib "$src/target/release/libkrun.dylib" libkrun.2.dylib
ln -sf libkrun.2.dylib "$bin/libkrun.dylib"
if [ "$features" = ffi,blk,net,gpu ]; then
    install_lib "$out/virgl/lib/libvirglrenderer.1.dylib" libvirglrenderer.1.dylib
    cp -f "$out/virgl/libexec/virgl_render_server" "$bin/virgl_render_server.new"
    mv -f "$bin/virgl_render_server.new" "$bin/virgl_render_server"
    install_lib "$sdk/lib/libvulkan.1.dylib" libvulkan.1.dylib
    install_lib "$sdk/lib/libMoltenVK.dylib" libMoltenVK.dylib
    # The drivers' manifests, MoltenVK's as an ordinary driver: Venus does
    # not ask the loader for portability drivers.
    mkdir -p "$bin/vulkan"
    printf '{"file_format_version": "1.0.0", "ICD": {"library_path": "../libMoltenVK.dylib", "api_version": "1.4.0"}}\n' \
        > "$bin/vulkan/moltenvk.json"
    if [ -f "$kk" ]; then
        install_lib "$kk" libvulkan_kosmickrisp.dylib
        install_name_tool -add_rpath @loader_path "$bin/libvulkan_kosmickrisp.dylib" 2>/dev/null || true
        printf '{"file_format_version": "1.0.0", "ICD": {"library_path": "../libvulkan_kosmickrisp.dylib", "api_version": "1.4.0"}}\n' \
            > "$bin/vulkan/kosmickrisp.json"
    fi
    install_name_tool -change "$out/virgl/lib/libvirglrenderer.1.dylib" @rpath/libvirglrenderer.1.dylib \
        "$bin/libkrun.2.dylib"
    install_name_tool -add_rpath @loader_path "$bin/libkrun.2.dylib" 2>/dev/null || true
    codesign -f -s - "$bin/virgl_render_server" 2>/dev/null
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
echo "built $bin: hangar-krun (libkrun $branch $(echo $want | cut -c1-8), $features) and mkfs.ext4"
