#!/bin/sh
# Builds the desktop app (crates/app) and runs it from an app bundle,
# out/Hangar.app, as macOS needs to name it Hangar in the menu bar and to
# let it post notifications. Run from the Mac:
#
#   dev/app.sh            build (debug) and start it, quitting one running
#   dev/app.sh release    the same, optimised
#   dev/app.sh stop       quit it
#
# HANGAR_URL and HANGAR_TOKEN, when set, are passed on: a server place for
# the one run, beside the places the app keeps.
set -eu
repo=$(cd "$(dirname "$0")/.." && pwd)
app=$repo/out/Hangar.app

stop() {
    pkill -TERM -x hangar-app 2>/dev/null || return 0
    for i in $(seq 1 50); do pgrep -x hangar-app >/dev/null || return 0; sleep 0.1; done
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

mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Hangar</string>
  <key>CFBundleDisplayName</key><string>Hangar</string>
  <key>CFBundleIdentifier</key><string>io.github.csnewman.hangar</string>
  <key>CFBundleExecutable</key><string>hangar-app</string>
  <key>CFBundleIconFile</key><string>Hangar</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# The icon, from the web app's, made once.
icns=$app/Contents/Resources/Hangar.icns
if [ ! -f "$icns" ]; then
    set_=$repo/out/Hangar.iconset
    rm -rf "$set_" && mkdir -p "$set_"
    for s in 16 32 128 256 512; do
        sips -z $s $s "$repo/web/public/icon-512.png" --out "$set_/icon_${s}x${s}.png" >/dev/null
        d=$((s * 2))
        [ $d -le 512 ] && sips -z $d $d "$repo/web/public/icon-512.png" --out "$set_/icon_${s}x${s}@2x.png" >/dev/null
    done
    iconutil -c icns "$set_" -o "$icns"
    rm -rf "$set_"
fi

stop
# Copied, not linked: macOS reads the bundle's own executable.
cp "$repo/crates/target/$profile/hangar-app" "$app/Contents/MacOS/hangar-app"
# Signed with a stable identity, so the Keychain's answer about the
# places' tokens holds across builds (dev/codesign.sh).
"$repo/dev/codesign.sh" io.github.csnewman.hangar "$app"
set --
[ -n "${HANGAR_URL:-}" ] && set -- "$@" --env "HANGAR_URL=$HANGAR_URL"
[ -n "${HANGAR_TOKEN:-}" ] && set -- "$@" --env "HANGAR_TOKEN=$HANGAR_TOKEN"
open -n "$app" "$@"
