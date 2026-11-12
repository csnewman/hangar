#!/bin/sh
# Builds a release of the desktop app for macOS, as the release workflow
# does (.github/workflows/app.yml):
#
#   dev/app-release.sh CHANNEL VERSION OUT
#
# builds hangar-app for CHANNEL (crates/app/src/channel.rs) for Apple
# silicon and Intel, joins them into one universal binary, bundles it
# (dev/app-bundle.sh), signs it with the release certificate
# (dev/codesign.sh; HANGAR_SIGN_IDENTITY names it, HANGAR_SIGN_KEYCHAIN the
# keychain it is in) and zips it into OUT as <Name>-VERSION-mac.zip,
# printing the zip's path. HANGAR_RELEASE_TARGETS names other targets.
set -eu
channel=$1
version=$2
out=$3
repo=$(cd "$(dirname "$0")/.." && pwd)
if [ -z "${HANGAR_SIGN_IDENTITY:-}" ]; then
    echo "HANGAR_SIGN_IDENTITY names the release certificate (dev/release-cert.sh)" >&2
    exit 2
fi
targets=${HANGAR_RELEASE_TARGETS:-aarch64-apple-darwin x86_64-apple-darwin}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

set --
for t in $targets; do
    (cd "$repo/crates" && HANGAR_CHANNEL=$channel cargo build -q --release --locked -p hangar-app --target "$t")
    set -- "$@" "$repo/crates/target/$t/release/hangar-app"
done
lipo -create -output "$work/hangar-app" "$@"

app=$("$repo/dev/app-bundle.sh" "$channel" "$work/hangar-app" "$work" "$version")
"$repo/dev/codesign.sh" "$app" >&2
codesign --verify --strict "$app"
mkdir -p "$out"
zip=$(cd "$out" && pwd)/$(basename "$app" .app | tr ' ' -)-$version-mac.zip
rm -f "$zip"
ditto -c -k --keepParent "$app" "$zip"
echo "$zip"
