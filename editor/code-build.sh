#!/usr/bin/env bash
# Builds the Code tab's service (code/) into one file, <server-dir>/hangar/code.mjs,
# on the editor disk beside VS Code's server. It is built in a copy, so the
# dependencies it installs are for the machine building, whatever code/
# holds.
#
#   editor/code-build.sh <server-dir> [work-dir]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
dest=$1
work=${2:-$(mktemp -d)}
mkdir -p "$work/code"
tar -C "$here/code" --exclude=node_modules --exclude=dist -cf - . | tar -C "$work/code" -xf -
(cd "$work/code" && npm ci --no-audit --no-fund --loglevel=error && npm run -s build)
install -D -m 0644 "$work/code/dist/code.mjs" "$dest/hangar/code.mjs"
