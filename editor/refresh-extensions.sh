#!/usr/bin/env bash
# Copies editor/extensions into an existing build in out/editor and remakes
# its disk, without rebuilding VS Code: for working on Hangar's extensions,
# which are plain JavaScript and need no build of their own. Linux only.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
out=$(realpath -m "${1:-$here/../out/editor}")
case "$(uname -m)" in
x86_64) arch=x64 ;;
aarch64) arch=arm64 ;;
esac
name=vscode-reh-web-linux-$arch
for ext in "$here"/extensions/*/; do
	rm -rf "$out/$name/extensions/$(basename "$ext")"
	cp -a "$ext" "$out/$name/extensions/"
done
"$here/disk.sh" "$out/$name" "$out/editor-$arch.ext4"
