#!/bin/sh
# Builds hangar-worker inside the Lima VM and runs it there as root, against
# the control plane in compose.yaml. Run from the Mac:
#
#   dev/lima-worker.sh          start it, detached; log in /var/tmp/hangar-worker.log
#   dev/lima-worker.sh stop     stop it, powering its environments off
#
# The patched Cloud Hypervisor goes first on PATH: /usr/local/bin has an
# older, unpatched build (see AGENTS.md).
set -eu
repo=/Users/csnewman/Projects/hangar

if [ "${1:-}" = stop ]; then
    limactl shell hangar sudo pkill -TERM -x hangar-worker || true
    exit 0
fi

limactl shell hangar bash -c "
    set -e
    cd $repo
    /usr/local/go/bin/go build -o /var/tmp/hangar-worker ./cmd/hangar-worker
    # By process name: a pattern over whole command lines would also match
    # this shell, whose command line names the worker.
    sudo pkill -TERM -x hangar-worker && sleep 3 || true
    sudo env PATH=\$HOME/ch-upstream/target/release:\$PATH \
        LD_LIBRARY_PATH=/usr/local/lib/aarch64-linux-gnu \
        setsid /var/tmp/hangar-worker -config $repo/dev/worker-lima.yaml \
        > /var/tmp/hangar-worker.log 2>&1 < /dev/null &
    sleep 2
    tail -5 /var/tmp/hangar-worker.log
"
