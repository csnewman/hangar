#!/bin/sh
# Writes crates/ghostty/src/sys.rs, the bindings to libghostty-vt's C API,
# from the header of the release crates/ghostty/build.rs pins. Run it after
# moving the pin; it needs bindgen (cargo install bindgen-cli) and libclang
# (Homebrew's llvm on a Mac).
#
#   dev/ghostty-bindings.sh [GHOSTTY_SOURCE]
#
# With no argument it fetches the pinned release's source into out/ghostty.
set -eu
repo=$(cd "$(dirname "$0")/.." && pwd)
tag=$(sed -n 's/^const TAG: &str = "\(.*\)";/\1/p' "$repo/crates/ghostty/build.rs")
src=${1:-}
if [ -z "$src" ]; then
    src="$repo/out/ghostty/ghostty-$tag"
    if [ ! -f "$src/include/ghostty/vt.h" ]; then
        mkdir -p "$repo/out/ghostty"
        # GitHub's archive of tag T unpacks as ghostty-T.
        curl -sSfL "https://github.com/ghostty-org/ghostty/archive/refs/tags/$tag.tar.gz" |
            tar xz -C "$repo/out/ghostty"
    fi
fi
if [ -z "${LIBCLANG_PATH:-}" ] && command -v brew >/dev/null; then
    LIBCLANG_PATH="$(brew --prefix llvm)/lib"
    export LIBCLANG_PATH
fi
bindgen "$src/include/ghostty/vt.h" \
    --allowlist-function 'ghostty_.*' \
    --allowlist-type 'Ghostty.*' \
    --allowlist-var 'GHOSTTY_.*' \
    --default-enum-style moduleconsts \
    --with-derive-default \
    --no-layout-tests \
    --no-doc-comments \
    --use-core \
    -o "$repo/crates/ghostty/src/sys.rs" \
    -- -I"$src/include"
echo "wrote crates/ghostty/src/sys.rs for $tag"
