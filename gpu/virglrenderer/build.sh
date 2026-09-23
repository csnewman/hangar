#!/bin/sh
# Builds the virglrenderer hangar-gpu links against: upstream at the commit the
# patches here are carried against, with those patches applied.
#
#   build.sh [prefix]    (default /usr/local)
#
# Installs the library, its pkg-config file and the Venus render server into
# the prefix. hangar-gpu then builds against it with
# PKG_CONFIG_PATH=<prefix>/lib/<multiarch>/pkgconfig, and runs against it with
# LD_LIBRARY_PATH pointing at the same directory.
set -eu

BASE=7d4eb14cc20be87d6c4eae984352d56f1ac24ba6
REPO=https://gitlab.freedesktop.org/virgl/virglrenderer.git

prefix=${1:-/usr/local}
here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

git -C "$work" init -q src
git -C "$work/src" fetch -q --depth 1 "$REPO" "$BASE"
git -C "$work/src" checkout -q FETCH_HEAD
git -C "$work/src" -c user.name=build -c user.email=build@localhost \
	am -q "$here"/0*.patch

# Venus runs in a render server of its own process, and the snapshot API the
# patches add is an unstable one. GL errors are checked after every command,
# which is what reports a failed context rather than leaving it drawing
# nothing.
meson setup "$work/build" "$work/src" \
	--prefix "$prefix" \
	--buildtype release \
	-Dplatforms=egl \
	-Dvenus=true \
	-Drender-server-mode=process \
	-Dcheck-gl-errors=true \
	-Dunstable-apis=true \
	-Dvideo=false \
	-Dtests=false
ninja -C "$work/build"
ninja -C "$work/build" install
