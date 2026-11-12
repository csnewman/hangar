#!/bin/sh
# Builds hangar-worker and runs it on the Mac, against the control plane in
# compose.yaml, running environments under libkrun. Run from the Mac:
#
#   dev/mac-worker.sh          start it, detached; log in out/mac-worker/worker.log
#   dev/mac-worker.sh stop     stop it, powering its environments off
#
# It needs dev/krun-build.sh's hangar-krun and mkfs.ext4, and
# `dev/kernel-dev.sh 16k`'s kernel.
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
# The agent every environment boots with (crates/agent): static against
# musl, for the guest, and linked by rust-lld.
musl=aarch64-unknown-linux-musl
if ! rustup target list --installed 2>/dev/null | grep -qx $musl; then
    echo "no $musl Rust target, which the guest agent needs: rustup target add $musl" >&2
    exit 1
fi
# ring's C (the SSH server's crypto) is archived by LLVM's ar: the Mac's
# own writes archives ELF's linker finds nothing in.
llvm_ar=""
for a in "$(brew --prefix llvm 2>/dev/null)/bin/llvm-ar" /opt/homebrew/opt/llvm/bin/llvm-ar /usr/local/opt/llvm/bin/llvm-ar; do
    [ -x "$a" ] && llvm_ar=$a && break
done
if [ -z "$llvm_ar" ]; then
    echo "no llvm-ar, which the guest agent needs: brew install llvm" >&2
    exit 1
fi
(cd crates && CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld AR_aarch64_unknown_linux_musl=$llvm_ar \
    cargo build -q --release --target $musl -p hangar-agent)
install -m 0755 crates/target/$musl/release/hangar-agent "$dir/hangar-agent-guest"
# A stopping worker powers its environments off first, so the new one waits
# for it: two would fight over the same machines.
stop
PATH=$repo/out/krun/bin:$PATH nohup "$dir/hangar-worker" -config "$repo/dev/worker-mac.yaml" \
    > "$dir/worker.log" 2>&1 < /dev/null &
sleep 2
tail -5 "$dir/worker.log"
