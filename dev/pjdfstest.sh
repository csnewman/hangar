#!/bin/sh
# Runs the pjdfstest POSIX suite on the shared files, in an environment of
# the dev stack's, once for each way a pack squashes its callers, and once
# on the environment's own disk under hangarfs; then compares each run's
# failures with those expected (dev/pjdfstest/<run>.expected), which are
# what the files' design leaves different from a local disk: one owner for
# everything with all_squash, nobody for root with root_squash. Run from the
# Mac:
#
#   dev/pjdfstest.sh <environment>             compare
#   dev/pjdfstest.sh -update <environment>     write the expected failures
#
# It reaches the environment through the SSH gateway ($SSH, by default
# "ssh -p 2222", as its owner's sign-in key), and makes or reuses a pack of
# the owner's, "pjdfstest", reaching their environments, sharing
# ~/.pjdfstest/. A run takes a few minutes; the environment needs git and
# a compiler, which are installed if it has apt and they are missing.
set -eu

update=
if [ "${1:-}" = -update ]; then
	update=1
	shift
fi
env=${1:?usage: dev/pjdfstest.sh [-update] <environment>}
ssh=${SSH:-ssh -p 2222}
host=${HANGAR_HOST:-hangar.localhost}
api="http://$host:8080/api/frontend"
here=$(cd "$(dirname "$0")" && pwd)
expected=$here/pjdfstest
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

call() { curl -sf --resolve "$host:8080:127.0.0.1" -H 'content-type: application/json' "$@"; }

pack=$(call "$api/packs" | python3 -c '
import json, sys
print(next((k["id"] for k in json.load(sys.stdin) if k["name"] == "pjdfstest"), ""))')
if [ -z "$pack" ]; then
	pack=$(call -X POST "$api/packs" -d '{"name":"pjdfstest","attach":"owner","paths":[{"path":"~/.pjdfstest/","sensitive":false}]}' |
		python3 -c 'import json, sys; print(json.load(sys.stdin)["id"])')
fi

guest() { $ssh "$env@$host" "$@"; }

# The suite, built once in the environment's home.
guest 'set -e
if [ ! -x ~/pjdfstest/pjdfstest ]; then
	command -v cc >/dev/null && command -v autoreconf >/dev/null && command -v git >/dev/null ||
		{ sudo DEBIAN_FRONTEND=noninteractive apt-get update -qq && sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq build-essential autoconf automake git; } >/dev/null
	rm -rf ~/pjdfstest && git clone -q https://github.com/pjd/pjdfstest ~/pjdfstest
	cd ~/pjdfstest && autoreconf -ifs >/dev/null 2>&1 && ./configure -q && make -s pjdfstest
fi'

# The expected failures (dev/pjdfstest/*.expected) are, beside what one
# owner or a squashed root changes: device nodes, which the files root
# never holds (the worker would open them on its host); a file unlinked
# while open, which NFS keeps as a hidden .nfs file (unlink/14.t); and a
# hard link's attributes, a second stale through its other name, as NFS
# caches them by filehandle, which is the path's (link/00.t).

# failures reads prove's summary: each failing test file and the tests in it
# that failed, one file to a line.
failures() {
	sed -n '/^Test Summary Report/,$p' | awk '
		/\.t +\(Wstat/ { if (f) print f, n; f = $1; sub(".*/tests/", "", f); n = ""; next }
		/Failed tests?:/ { sub(/.*Failed tests?: */, ""); n = n $0; next }
		/^ +[0-9]/ { gsub(/ /, ""); n = n (n == "" || n ~ /,$/ ? "" : ",") $0; next }
		END { if (f) print f, n }' | sed 's/ *, */,/g' | sort
}

status=0
for run in local all_squash root_squash no_root_squash; do
	if [ "$run" = local ]; then
		dir='~/.cache/pjdfstest-local'
	else
		call -X PUT "$api/packs/$pack" -d "{\"name\":\"pjdfstest\",\"attach\":\"owner\",\"squash\":\"$run\",\"paths\":[{\"path\":\"~/.pjdfstest/\",\"sensitive\":false}]}" >/dev/null
		# The worker looks its environments' files up every few seconds.
		sleep 12
		dir="~/.pjdfstest/$run"
	fi
	echo "== $run"
	# Root, as nobody with root_squash, cannot clear its own run; without
	# a squash it can.
	guest "sudo rm -rf $dir ~/.pjdfstest/root_squash 2>/dev/null; mkdir -p $dir && cd $dir && sudo prove -r ~/pjdfstest/tests 2>&1; sudo rm -rf $dir 2>/dev/null" \
		> "$work/$run.out" || true
	# A run that did not finish -- the environment restarted, say -- has no
	# result, which is no run at all.
	if ! grep -q '^Result: ' "$work/$run.out"; then
		echo "   no result: the run did not finish" >&2
		tail -5 "$work/$run.out" | sed 's/^/   /' >&2
		exit 1
	fi
	failures < "$work/$run.out" > "$work/$run"
	if [ -n "$update" ]; then
		mkdir -p "$expected"
		cp "$work/$run" "$expected/$run.expected"
		echo "   $(wc -l < "$work/$run" | tr -d ' ') failing files recorded"
	elif diff -u "$expected/$run.expected" "$work/$run" > "$work/$run.diff"; then
		echo "   as expected"
	else
		status=1
		echo "   differs from $run.expected (- expected, + this run):"
		tail -n +3 "$work/$run.diff" | sed 's/^/   /'
	fi
done
call -X PUT "$api/packs/$pack" -d '{"name":"pjdfstest","attach":"owner","squash":"all_squash","paths":[{"path":"~/.pjdfstest/","sensitive":false}]}' >/dev/null
exit $status
