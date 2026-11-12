#!/bin/sh
# Signs a build of the desktop app, or a tool of it, with a stable identity:
#
#   dev/codesign.sh IDENTIFIER PATH
#
# The Keychain lets a program at the places' tokens by its signature. An
# ad-hoc signed build, as cargo makes, is a new program each time, so macOS
# asks again after every build. Signed with an Apple Development
# certificate (Xcode makes one, under Settings > Accounts > Manage
# Certificates), a build is known by its identifier and team, and "Always
# Allow" holds across builds. HANGAR_SIGN_IDENTITY names another identity.
# With none, the build is left as it is.
set -eu
identifier=$1
path=$2
identity=${HANGAR_SIGN_IDENTITY:-$(security find-identity -v -p codesigning 2>/dev/null |
    awk '/"Apple Development:/ { print $2; exit }')}
if [ -z "$identity" ]; then
    echo "no Apple Development identity: $path stays ad-hoc signed, and the Keychain will ask after each build" >&2
    exit 0
fi
codesign --force --sign "$identity" --identifier "$identifier" --timestamp=none "$path"
