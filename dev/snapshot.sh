#!/bin/sh
# Renders the desktop app's window to a PNG without the screen
# (crates/app/examples/snapshot.rs; its SNAPSHOT_* variables are passed on):
#
#   dev/snapshot.sh OUT.png
#
# Signed as the app is (dev/codesign.sh), so the Keychain's answer about the
# places' tokens holds across builds of it.
set -eu
repo=$(cd "$(dirname "$0")/.." && pwd)
(cd "$repo/crates" && cargo build -q -p hangar-app --example snapshot)
bin=$repo/crates/target/debug/examples/snapshot
"$repo/dev/codesign.sh" io.github.csnewman.hangar.snapshot "$bin"
exec "$bin" "$@"
