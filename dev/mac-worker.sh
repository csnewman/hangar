#!/bin/sh
# Builds hangar-worker and runs it on the Mac, against the control plane in
# compose.yaml, running environments under libkrun. Run from the Mac:
#
#   dev/mac-worker.sh          start it, detached; log in out/mac-worker/worker.log
#   dev/mac-worker.sh stop     stop it, powering its environments off
#
# It needs dev/krun-build.sh's hangar-krun and mkfs.ext4, and
# dev/kernel-dev.sh's kernel (which has virtio-mmio).
set -eu
repo=$(cd "$(dirname "$0")/.." && pwd)
dir=$repo/out/mac-worker

stop() {
    pkill -TERM -x hangar-worker 2>/dev/null || return 0
    for i in $(seq 1 150); do pgrep -x hangar-worker >/dev/null || return 0; sleep 1; done
}

if [ "${1:-}" = stop ]; then
    stop
    exit 0
fi

mkdir -p "$dir"
cd "$repo"
go build -o "$dir/hangar-worker" ./cmd/hangar-worker
# The agent every environment boots with: static, for the guest.
CGO_ENABLED=0 GOOS=linux GOARCH=arm64 go build -trimpath -ldflags '-s -w' \
    -o "$dir/hangar-agent-guest" ./cmd/hangar-agent
# A stopping worker powers its environments off first, so the new one waits
# for it: two would fight over the same machines.
stop
PATH=$repo/out/krun/bin:$PATH nohup "$dir/hangar-worker" -config "$repo/dev/worker-mac.yaml" \
    > "$dir/worker.log" 2>&1 < /dev/null &
sleep 2
tail -5 "$dir/worker.log"
