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
    limactl shell hangar bash -c '
        sudo pkill -TERM -x hangar-worker || exit 0
        for i in $(seq 1 150); do pgrep -x hangar-worker > /dev/null || exit 0; sleep 1; done'
    exit 0
fi

limactl shell hangar bash -c "
    set -e
    cd $repo
    # The files environments share, exported for the control plane on the
    # Mac (compose.yaml's files volume), as one NFS share serves every
    # worker of a real deployment.
    sudo mkdir -p /var/lib/hangar/files /etc/exports.d
    if ! dpkg -s nfs-kernel-server >/dev/null 2>&1; then
        sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq nfs-kernel-server >/dev/null
    fi
    echo '/var/lib/hangar/files *(rw,insecure,no_root_squash,no_subtree_check,fsid=0)' |
        sudo tee /etc/exports.d/hangar-files.exports >/dev/null
    sudo rm -f /etc/exports.d/hangar-nfstest.exports
    sudo systemctl enable --now nfs-server >/dev/null 2>&1
    sudo exportfs -ra
    /usr/local/go/bin/go build -o /var/tmp/hangar-worker ./cmd/hangar-worker
    # The agent every environment boots with: static, for the guest.
    CGO_ENABLED=0 /usr/local/go/bin/go build -trimpath -ldflags '-s -w' \
        -o /var/tmp/hangar-agent-guest ./cmd/hangar-agent
    # By process name: a pattern over whole command lines would also match
    # this shell, whose command line names the worker. A stopping worker
    # powers its environments off first, so the new one waits for it: two
    # workers would fight over the same machines' sockets.
    if sudo pkill -TERM -x hangar-worker; then
        for i in \$(seq 1 150); do pgrep -x hangar-worker > /dev/null || break; sleep 1; done
    fi
    sudo env PATH=\$HOME/ch-upstream/target/release:\$PATH \
        LD_LIBRARY_PATH=/usr/local/lib/aarch64-linux-gnu \
        setsid /var/tmp/hangar-worker -config $repo/dev/worker-lima.yaml \
        > /var/tmp/hangar-worker.log 2>&1 < /dev/null &
    sleep 2
    tail -5 /var/tmp/hangar-worker.log
"
