#!/bin/sh
# Signs a build of the desktop app, or a tool of it, with a stable identity:
#
#   dev/codesign.sh PATH [IDENTIFIER]
#
# PATH is an app bundle, signed as its bundle identifier, or a program,
# signed as IDENTIFIER.
#
# The Keychain lets a program at the places' tokens by its signature. An
# ad-hoc signed build, as cargo makes, is a new program each time, so macOS
# asks again after every build. Signed with a certificate, a build is known
# by its identifier and the certificate, and "Always Allow" holds across
# builds. The identity is HANGAR_SIGN_IDENTITY, else the Mac's Apple
# Development one (Xcode makes one, under Settings > Accounts > Manage
# Certificates); HANGAR_SIGN_KEYCHAIN names the keychain it is in, as a
# release's is (.github/workflows/app.yml). With none, the build is left as
# it is.
set -eu
path=$1
identifier=${2:-}
if [ -z "$identifier" ]; then
    identifier=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$path/Contents/Info.plist")
fi
identity=${HANGAR_SIGN_IDENTITY:-$(security find-identity -v -p codesigning 2>/dev/null |
    awk '/"Apple Development:/ { print $2; exit }')}
if [ -z "$identity" ]; then
    echo "no Apple Development identity: $path stays ad-hoc signed, and the Keychain will ask after each build" >&2
    exit 0
fi
set -- --force --sign "$identity" --identifier "$identifier" --timestamp=none
[ -n "${HANGAR_SIGN_KEYCHAIN:-}" ] && set -- "$@" --keychain "$HANGAR_SIGN_KEYCHAIN"
codesign "$@" "$path"
