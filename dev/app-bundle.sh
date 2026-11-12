#!/bin/sh
# Makes the desktop app's bundle, which macOS needs to name the app in the
# menu bar and to let it post notifications:
#
#   dev/app-bundle.sh CHANNEL BINARY DIR VERSION
#
# writes DIR/<name>.app around BINARY, a hangar-app built for CHANNEL (dev,
# experimental or stable: HANGAR_CHANNEL, crates/app/src/channel.rs), and
# prints its path. Each channel is an app of its own, by name and bundle
# identifier, so they can be installed side by side.
set -eu
if [ $# -ne 4 ]; then
    echo "usage: dev/app-bundle.sh CHANNEL BINARY DIR VERSION" >&2
    exit 2
fi
channel=$1
bin=$2
dir=$3
version=$4
case $channel in
dev) name="Hangar Dev" id=io.github.csnewman.hangar.dev ;;
experimental) name="Hangar Experimental" id=io.github.csnewman.hangar.experimental ;;
stable) name=Hangar id=io.github.csnewman.hangar ;;
*)
    echo "unknown channel $channel: dev, experimental or stable" >&2
    exit 2
    ;;
esac
repo=$(cd "$(dirname "$0")/.." && pwd)
app=$dir/$name.app

mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>$name</string>
  <key>CFBundleDisplayName</key><string>$name</string>
  <key>CFBundleIdentifier</key><string>$id</string>
  <key>CFBundleExecutable</key><string>hangar-app</string>
  <key>CFBundleIconFile</key><string>Hangar</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# The icon, from the web app's, made once per bundle.
icns=$app/Contents/Resources/Hangar.icns
if [ ! -f "$icns" ]; then
    set_=$(mktemp -d)/Hangar.iconset
    mkdir -p "$set_"
    for s in 16 32 128 256 512; do
        sips -z $s $s "$repo/web/public/icon-512.png" --out "$set_/icon_${s}x${s}.png" >/dev/null
        d=$((s * 2))
        [ $d -le 512 ] && sips -z $d $d "$repo/web/public/icon-512.png" --out "$set_/icon_${s}x${s}@2x.png" >/dev/null
    done
    iconutil -c icns "$set_" -o "$icns"
    rm -rf "$(dirname "$set_")"
fi

# Copied, not linked: macOS reads the bundle's own executable.
cp "$bin" "$app/Contents/MacOS/hangar-app"
echo "$app"
