#!/usr/bin/env bash
# Rebuilds what Hangar adds to the editor disk -- its extensions and the Code
# tab's service -- into an existing build in out/editor, and remakes the disk,
# without rebuilding VS Code. For working on those; Linux only.
#
#   editor/refresh.sh [out-dir]
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
"$here/code-build.sh" "$out/$name" "${HANGAR_EDITOR_WORK:-$HOME/.cache/hangar-editor}/hangar-code"
"$here/disk.sh" "$out/$name" "$out/editor-$arch.ext4"
