#!/bin/sh
# Builds the desktop app (crates/app) and runs it from an app bundle,
# out/Hangar Dev.app (dev/app-bundle.sh), as macOS needs to name it in the
# menu bar and to let it post notifications. Run from the Mac:
#
#   dev/app.sh            build (debug) and start it, quitting one running
#   dev/app.sh release    the same, optimised
#   dev/app.sh stop       quit it
#
# It is the dev channel's build (crates/app/src/channel.rs): Hangar Dev,
# with settings, places and tokens of its own, beside an installed Hangar
# Experimental or the Electron app.
#
# HANGAR_URL and HANGAR_TOKEN, when set, are passed on: a server place for
# the one run, beside the places the app keeps.
set -eu
repo=$(cd "$(dirname "$0")/.." && pwd)
app="$repo/out/Hangar Dev.app"

# The dev bundle's process alone: an installed release's has the same
# executable name.
stop() {
    for pid in $(pgrep -x hangar-app); do
        case $(ps -o comm= -p "$pid") in
        "$app"/*) kill -TERM "$pid" 2>/dev/null || true ;;
        esac
    done
    for i in $(seq 1 50); do
        running=
        for pid in $(pgrep -x hangar-app); do
            case $(ps -o comm= -p "$pid") in "$app"/*) running=1 ;; esac
        done
        [ -z "$running" ] && return 0
        sleep 0.1
    done
}

if [ "${1:-}" = stop ]; then
    stop
    exit 0
fi

profile=debug
flags=
if [ "${1:-}" = release ]; then
    profile=release
    flags=--release
fi
(cd "$repo/crates" && cargo build -q $flags -p hangar-app)

stop
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$repo/crates/app/Cargo.toml" | head -1)
"$repo/dev/app-bundle.sh" dev "$repo/crates/target/$profile/hangar-app" "$repo/out" "$version" >/dev/null
# Signed with a stable identity, so the Keychain's answer about the
# places' tokens holds across builds (dev/codesign.sh).
"$repo/dev/codesign.sh" "$app"
set --
[ -n "${HANGAR_URL:-}" ] && set -- "$@" --env "HANGAR_URL=$HANGAR_URL"
[ -n "${HANGAR_TOKEN:-}" ] && set -- "$@" --env "HANGAR_TOKEN=$HANGAR_TOKEN"
open -n "$app" "$@"
