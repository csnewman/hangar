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
if [ $# -ne 3 ]; then
    echo "usage: dev/app-release.sh CHANNEL VERSION OUT" >&2
    exit 2
fi
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

# Runs a step, its errors kept: in CI a failed one leaves them as an
# annotation, which can be read where the job's log cannot.
run() {
    step=$1
    shift
    if "$@" 2> "$work/errors"; then
        cat "$work/errors" >&2
        return 0
    fi
    cat "$work/errors" >&2
    if [ -n "${GITHUB_ACTIONS:-}" ]; then
        printf '::error title=app-release.sh failed %s::%s\n' "$step" \
            "$(tail -n 25 "$work/errors" | tr '\n' ' ' | cut -c1-2000)"
    fi
    exit 1
}

set --
for t in $targets; do
    run "building for $t" sh -c 'cd "$1/crates" && HANGAR_CHANNEL=$2 cargo build -q --release --locked -p hangar-app --target "$3"' \
        _ "$repo" "$channel" "$t"
    set -- "$@" "$repo/crates/target/$t/release/hangar-app"
done
run "joining the architectures" lipo -create -output "$work/hangar-app" "$@"

app=$(run "bundling" "$repo/dev/app-bundle.sh" "$channel" "$work/hangar-app" "$work" "$version")
run "signing" "$repo/dev/codesign.sh" "$app"
run "checking the signature" codesign --verify --strict "$app"
mkdir -p "$out"
zip=$(cd "$out" && pwd)/$(basename "$app" .app | tr ' ' -)-$version-mac.zip
rm -f "$zip"
run "zipping" ditto -c -k --keepParent "$app" "$zip"
echo "$zip"
