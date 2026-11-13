#!/bin/sh
# Runs the pjdfstest POSIX suite against the NFS server through Linux's own
# client, with no environment: the server (crates/nfs/examples/tcp.rs)
# serves a scratch files root over TCP, the kernel mounts it as the agent
# mounts the shared files, and pjdfstest runs in it once for each way a
# pack squashes its callers. Each run's failures are compared with those
# expected (dev/pjdfstest/tcp-<run>.expected), which are what the files'
# design leaves different from a local disk, as dev/pjdfstest.sh says,
# seen through a plain mount rather than hangarfs.
#
#   dev/pjdfstest-tcp.sh            compare
#   dev/pjdfstest-tcp.sh -update    write the expected failures
#
# As root on Linux, with cargo, perl's prove, mount.nfs (nfs-common) and
# /etc/protocols (netbase): a CI runner, or on the Mac a privileged
# OrbStack container (AGENTS.md). SERVER names another server's binary,
# taking the example's arguments, to run instead. A run takes about three
# minutes.
set -eu

update=
if [ "${1:-}" = -update ]; then
	update=1
fi
here=$(cd "$(dirname "$0")" && pwd)
expected=$here/pjdfstest
work=$(mktemp -d)
mnt=$work/mnt
files=${FILES:-/var/tmp/hangar-pjdfstest-files}
suite=${PJDFSTEST:-/var/tmp/pjdfstest}
port=20490
server_pid=
cleanup() {
	umount "$mnt" 2>/dev/null || umount -f -l "$mnt" 2>/dev/null || true
	[ -n "$server_pid" ] && kill "$server_pid" 2>/dev/null || true
	rm -rf "$work"
}
trap cleanup EXIT

if [ "$(id -u)" != 0 ]; then
	echo "pjdfstest-tcp: run as root: the server acts as its callers" >&2
	exit 1
fi

server=${SERVER:-}
if [ -z "$server" ]; then
	(cd "$here/../crates" && cargo build -q --release -p hangar-nfs --example tcp)
	server=${CARGO_TARGET_DIR:-$here/../crates/target}/release/examples/tcp
fi
if [ ! -x "$suite/pjdfstest" ]; then
	rm -rf "$suite" && git clone -q https://github.com/pjd/pjdfstest "$suite"
	(cd "$suite" && autoreconf -ifs >/dev/null 2>&1 && ./configure -q && make -s pjdfstest)
fi

# failures reads prove's summary as dev/pjdfstest.sh does.
failures() {
	sed -n '/^Test Summary Report/,$p' | awk '
		/\.t +\(Wstat/ { if (f) print f, n; f = $1; sub(".*/tests/", "", f); n = ""; next }
		/Failed tests?:/ { sub(/.*Failed tests?: */, ""); n = n $0; next }
		/^ +[0-9]/ { gsub(/ /, ""); n = n (n == "" || n ~ /,$/ ? "" : ",") $0; next }
		END { if (f) print f, n }' | sed 's/ *, */,/g' | sort
}

mkdir -p "$mnt"
status=0
for run in all_squash root_squash no_root_squash; do
	echo "== $run"
	# One set, its run folder the environment user's, as a home's is.
	rm -rf "$files" && mkdir -p "$files/s1/run" && chown 1000:1000 "$files/s1" "$files/s1/run"
	case $run in
	all_squash) squash=all ;;
	root_squash) squash=root ;;
	no_root_squash) squash=none ;;
	esac
	"$server" "$files" "127.0.0.1:$port" "$squash" 2> "$work/$run.server" &
	server_pid=$!
	sleep 1
	if ! kill -0 "$server_pid" 2>/dev/null; then
		echo "   the server did not start:" >&2
		sed 's/^/   /' "$work/$run.server" >&2
		exit 1
	fi
	# The agent's options, over TCP.
	mount -t nfs4 -o "vers=4.2,proto=tcp,port=$port,hard,lookupcache=positive,actimeo=1" \
		127.0.0.1:/ "$mnt"
	(cd "$mnt/s1/run" && prove -r "$suite/tests") > "$work/$run.out" 2>&1 || true
	umount "$mnt"
	kill "$server_pid"
	wait "$server_pid" 2>/dev/null || true
	server_pid=
	if ! grep -q '^Result: ' "$work/$run.out"; then
		echo "   no result: the run did not finish" >&2
		tail -5 "$work/$run.out" | sed 's/^/   /' >&2
		exit 1
	fi
	failures < "$work/$run.out" > "$work/$run"
	if [ -n "$update" ]; then
		cp "$work/$run" "$expected/tcp-$run.expected"
		echo "   $(wc -l < "$work/$run" | tr -d ' ') failing files recorded"
	elif diff -u "$expected/tcp-$run.expected" "$work/$run" > "$work/$run.diff"; then
		echo "   as expected"
	else
		status=1
		echo "   differs from tcp-$run.expected (- expected, + this run):"
		tail -n +3 "$work/$run.diff" | sed 's/^/   /'
	fi
done
rm -rf "$files"
exit $status
