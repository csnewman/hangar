#!/usr/bin/env bash
# Makes the editor disk a worker attaches to every environment: an ext4 image,
# labelled hangar-editor, holding a built VS Code server directory.
#
#   editor/disk.sh <server-dir> <image>
set -euo pipefail
dir=$1
img=$2
# Sized to its contents with room for ext4's own structures.
kib=$(du -sk "$dir" | cut -f1)
rm -f "$img.new"
truncate -s "$(( (kib * 13 / 10 + 65536) / 1024 ))M" "$img.new"
mkfs.ext4 -q -L hangar-editor -d "$dir" "$img.new"
mv "$img.new" "$img"
echo "built $img"
