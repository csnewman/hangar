#!/bin/sh
# Builds hangar-worker inside the Lima VM and runs a second one there as
# root, under libkrun (dev/worker-lima-krun.yaml), beside dev/lima-worker.sh's
# Cloud Hypervisor one. Run from the Mac, after `dev/krun-build.sh linux`:
#
#   dev/lima-krun-worker.sh        start it, detached; log in /var/tmp/hangar-worker-krun.log
#   dev/lima-krun-worker.sh stop   stop it, powering its environments off
#
# It is told apart from the other worker by its PID file: both are
# hangar-worker by name.
set -eu
repo=/Users/csnewman/Projects/hangar
pid=/var/tmp/hangar-worker-krun.pid

stop='
    if p=$(cat '$pid' 2>/dev/null) && sudo kill -TERM "$p" 2>/dev/null; then
        for i in $(seq 1 150); do sudo kill -0 "$p" 2>/dev/null || break; sleep 1; done
    fi
    sudo rm -f '$pid
if [ "${1:-}" = stop ]; then
    limactl shell hangar bash -c "$stop"
    exit 0
fi

limactl shell hangar bash -c "
    set -e
    cd $repo
    [ -x out/krun-linux/bin/hangar-krun ] || { echo 'no out/krun-linux/bin/hangar-krun: run dev/krun-build.sh linux' >&2; exit 1; }
    /usr/local/go/bin/go build -o /var/tmp/hangar-worker-krun ./cmd/hangar-worker
    CGO_ENABLED=0 /usr/local/go/bin/go build -trimpath -ldflags '-s -w' \
        -o /var/tmp/hangar-agent-guest ./cmd/hangar-agent
    $stop
    sudo mkdir -p /var/lib/hangar-krun
    # hangar-krun is found on PATH, and links the VM's own virglrenderer.
    # HANGAR_KRUN_LOG, from the Mac, is libkrun's log level (4 is debug);
    # HANGAR_KRUN_CONSOLE_IN a FIFO typed at its guests' consoles.
    sudo env HANGAR_KRUN_LOG=${HANGAR_KRUN_LOG:-2} HANGAR_KRUN_CONSOLE_IN=${HANGAR_KRUN_CONSOLE_IN:-} PATH=$repo/out/krun-linux/bin:\$PATH \
        LD_LIBRARY_PATH=/usr/local/lib/aarch64-linux-gnu \
        setsid /var/tmp/hangar-worker-krun -config $repo/dev/worker-lima-krun.yaml \
        > /var/tmp/hangar-worker-krun.log 2>&1 < /dev/null &
    echo \$! | sudo tee $pid > /dev/null
    sleep 2
    tail -5 /var/tmp/hangar-worker-krun.log
"
