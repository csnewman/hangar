#!/usr/bin/env bash
# Builds the editor every environment runs: VS Code's own server (Code - OSS,
# MIT) at a pinned release, with the patches in patches/ applied, the
# extensions in extensions/ built in, and product.json merged over upstream's;
# and beside it the Code tab's service (code/).
#
#   editor/build.sh [out-dir]
#
# Linux only, for the architecture it runs on. The result is
#
#   <out>/vscode-reh-web-linux-<arch>/   the server, with its own Node
#   <out>/editor-<arch>.ext4             the same, as the read-only disk a
#                                        worker attaches to every environment
#
# The source tree is kept in HANGAR_EDITOR_WORK (default
# ~/.cache/hangar-editor) between builds, since installing its dependencies
# takes longer than building it.
set -euo pipefail

VSCODE_TAG=1.139.0
VSCODE_COMMIT=2242ebbb54efeeb0129e08e919e7e8d43033cd83

here=$(cd "$(dirname "$0")" && pwd)
out=$(realpath -m "${1:-$here/../out/editor}")
work=${HANGAR_EDITOR_WORK:-$HOME/.cache/hangar-editor}
src=$work/vscode

case "$(uname -m)" in
x86_64) arch=x64 ;;
aarch64) arch=arm64 ;;
*) echo "unsupported architecture $(uname -m)" >&2; exit 1 ;;
esac

mkdir -p "$work" "$out"

if [ ! -d "$src/.git" ]; then
	git -c advice.detachedHead=false clone --depth 1 --branch "$VSCODE_TAG" \
		https://github.com/microsoft/vscode.git "$src"
fi
cd "$src"
if [ "$(git rev-parse HEAD)" != "$VSCODE_COMMIT" ]; then
	echo "$src is not VS Code $VSCODE_TAG ($VSCODE_COMMIT); remove it to fetch again" >&2
	exit 1
fi

node_want=$(cat .nvmrc)
node_have=$(node --version 2>/dev/null || true)
if [ "$node_have" != "v$node_want" ]; then
	echo "VS Code $VSCODE_TAG builds with Node $node_want; found ${node_have:-none}" >&2
	exit 1
fi

# Back to upstream, then the patches in order. Ignored files -- installed
# dependencies and earlier build output -- are kept.
git checkout -q -- .
git clean -q -fd
while read -r patch; do
	[ -n "$patch" ] || continue
	git apply "$here/patches/$patch"
done < "$here/patches/series"

# Hangar's own extensions are built in, as VS Code's are.
cp -a "$here"/extensions/. extensions/

node -e '
	const fs = require("fs");
	const [upstream, overlay] = process.argv.slice(1).map(f => JSON.parse(fs.readFileSync(f, "utf8")));
	fs.writeFileSync(process.argv[1], JSON.stringify({ ...upstream, ...overlay }, null, "\t") + "\n");
' product.json "$here/product.json"

# Dependencies are reinstalled only when the lock file changes.
stamp=node_modules/.hangar-lock-sha
lock_sha=$(sha256sum package-lock.json | cut -d' ' -f1)
if [ "$(cat "$stamp" 2>/dev/null)" != "$lock_sha" ]; then
	ELECTRON_SKIP_BINARY_DOWNLOAD=1 PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 npm ci
	echo "$lock_sha" > "$stamp"
fi

# The built-in Copilot extension is Microsoft's service, reached only with a
# GitHub Copilot subscription, and the largest built-in by far. Hangar's build
# leaves it out; no-builtin-copilot.diff lets the packaging do without it. It
# goes after installing, whose postinstall visits every extension's directory.
rm -rf extensions/copilot

NODE_OPTIONS=--max-old-space-size=8192 npm run gulp "vscode-reh-web-linux-$arch-min"

name=vscode-reh-web-linux-$arch
rm -rf "${out:?}/$name"
cp -a "$work/$name" "$out/$name"
"$here/code-build.sh" "$out/$name" "$work/hangar-code"
# What Hangar adds to upstream, as one hash: the patches, the product overlay
# and the extensions.
hangar_sha=$(cd "$here" && find patches product.json extensions code/src code/package-lock.json -type f | LC_ALL=C sort | xargs cat | sha256sum | cut -d' ' -f1)
printf 'vscode %s %s\nhangar %s\n' "$VSCODE_TAG" "$VSCODE_COMMIT" "$hangar_sha" > "$out/$name/HANGAR_VERSION"

"$here/disk.sh" "$out/$name" "$out/editor-$arch.ext4"
