#!/bin/sh
# Runs the OCI distribution-spec conformance suite against Hangar's registry.
#
# It starts a hangar-server of its own on 127.0.0.1:8099, against
# HANGAR_DATABASE_URL, with its registry in a scratch directory, signs in as
# the first administrator it creates and makes an access token for the
# suite. The registry is registry.<host>, so that name must reach this
# machine: CI adds registry.hangar.test to /etc/hosts, and on a Mac
# HOST=hangar.localhost needs nothing, since *.localhost is this machine.
#
#   HANGAR_DATABASE_URL=postgres://... [HOST=hangar.localhost] dev/registry-conformance.sh
#
# The database must have no users, as a new installation has none: the
# first administrator is made only then.
set -eu

: "${HANGAR_DATABASE_URL:?}"
version=v1.1.1
host=${HOST:-hangar.test}:8099
work=$(mktemp -d)
trap 'kill "$server" 2>/dev/null; rm -rf "$work"' EXIT

go build -o "$work/hangar-server" ./cmd/hangar-server
HANGAR_PUBLIC_URL="http://$host" HANGAR_REGISTRY_DIR="$work/registry" \
	HANGAR_INITIAL_ADMIN_USERNAME=admin HANGAR_INITIAL_ADMIN_PASSWORD=conformance-password \
	"$work/hangar-server" -listen 127.0.0.1:8099 > "$work/server.log" 2>&1 &
server=$!
until curl -sf "http://$host/api/healthz" > /dev/null; do
	kill -0 "$server" 2>/dev/null || { cat "$work/server.log"; exit 1; }
	sleep 1
done

# Signing in and making a token are state-changing requests from a browser's
# point of view, so they say they come from Hangar's own origin.
curl -sf -c "$work/cookies" -H "Origin: http://$host" -H 'Content-Type: application/json' \
	-d '{"username":"admin","password":"conformance-password"}' "http://$host/api/frontend/auth/login" > /dev/null
token=$(curl -sf -b "$work/cookies" -H "Origin: http://$host" -H 'Content-Type: application/json' \
	-d '{"name":"conformance"}' "http://$host/api/frontend/me/tokens" |
	sed -n 's/.*"token":"\([^"]*\)".*/\1/p')
[ -n "$token" ] || { echo "no access token"; exit 1; }

git clone -q --depth 1 --branch "$version" https://github.com/opencontainers/distribution-spec.git "$work/spec"
(cd "$work/spec/conformance" && go test -c -o "$work/conformance.test")

mkdir -p "$work/report"
cd "$work/report"
OCI_ROOT_URL="http://registry.$host" OCI_NAMESPACE=admin/conformance \
	OCI_CROSSMOUNT_NAMESPACE=admin/conformance-other OCI_USERNAME=admin OCI_PASSWORD="$token" \
	OCI_TEST_PULL=1 OCI_TEST_PUSH=1 OCI_TEST_CONTENT_DISCOVERY=1 OCI_TEST_CONTENT_MANAGEMENT=1 \
	OCI_HIDE_SKIPPED_WORKFLOWS=1 OCI_REPORT_DIR="$work/report" "$work/conformance.test"
