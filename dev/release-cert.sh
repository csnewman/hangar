#!/bin/sh
# Makes, once, the certificate the desktop app's releases are signed with:
#
#   dev/release-cert.sh DIR
#
# A self-signed code-signing certificate, "Hangar Release", good for ten
# years, with its key in DIR/hangar-release.p12 under a random password,
# DIR/hangar-release.password. Its base64 (DIR/hangar-release.p12.base64)
# and the password go into the repository's secrets, APP_SIGNING_P12 and
# APP_SIGNING_PASSWORD, in the app-signing environment
# (.github/workflows/app.yml); then DIR is deleted.
#
# It is not Apple's, so Gatekeeper does not trust it, which the cask's
# clearing of the quarantine flag makes no matter. What it gives is the same
# signature on every release: the Keychain knows the app by it, so a user's
# "Always Allow" for the app's tokens lasts across upgrades. Keep it: a new
# one has every user's Keychain ask again.
set -eu
if [ $# -ne 1 ]; then
    echo "usage: dev/release-cert.sh DIR    (e.g. dev/release-cert.sh ~/hangar-release)" >&2
    exit 2
fi
dir=$1
if [ -e "$dir/hangar-release.p12" ]; then
    echo "$dir already holds a release certificate: a new one would have every user's Keychain ask again" >&2
    exit 1
fi
umask 077
mkdir -p "$dir"
cd "$dir"
cat > cert.cnf <<'CNF'
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = Hangar Release
O = Hangar
[ext]
basicConstraints = critical,CA:false
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
subjectKeyIdentifier = hash
CNF
# macOS's own openssl (LibreSSL), whose PKCS#12 the Keychain imports.
/usr/bin/openssl req -x509 -newkey rsa:3072 -sha256 -days 3650 -nodes \
    -keyout key.pem -out cert.pem -config cert.cnf 2>/dev/null
password=$(/usr/bin/openssl rand -hex 24)
printf '%s' "$password" > hangar-release.password
/usr/bin/openssl pkcs12 -export -name "Hangar Release" -inkey key.pem -in cert.pem \
    -out hangar-release.p12 -passout "pass:$password"
base64 -i hangar-release.p12 -o hangar-release.p12.base64
rm -f key.pem cert.cnf
echo "made $dir/hangar-release.p12 (APP_SIGNING_P12: its .base64) and its password (APP_SIGNING_PASSWORD)"
