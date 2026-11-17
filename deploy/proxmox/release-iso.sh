#!/bin/sh
# Builds the release ISO a Proxmox place's VMs boot, worker VM and
# environments alike (crates/vm/src/proxmox, crates/agent/src/worker.rs):
#
#   /isolinux/        isolinux, booting the kernel with the command line below
#   /vmlinuz          the guest kernel
#   /initrd.img       the agent, as init, with mke2fs beside it
#   /hangar-worker    the worker, static, which a worker VM's agent runs
#   /ca-certificates.crt  what the worker trusts
#   /VERSION
#
#   release-iso.sh KERNEL AGENT MKE2FS WORKER CA_BUNDLE VERSION OUT.iso
#
# Needs xorriso, isolinux, syslinux-common and cpio (Debian and Ubuntu's
# names), on the worker's architecture: the command line is the worker's
# own (hangar-worker proxmox-cmdline).
set -eu
[ $# -eq 7 ] || { sed -n '2,16p' "$0"; exit 2; }
kernel=$1 agent=$2 mke2fs=$3 worker=$4 ca=$5 version=$6 out=$7
cmdline=$("$worker" proxmox-cmdline)
w=$(mktemp -d)
trap 'rm -rf "$w"' EXIT
mkdir -p "$w/iso/isolinux" "$w/initrd"
cp /usr/lib/ISOLINUX/isolinux.bin /usr/lib/syslinux/modules/bios/ldlinux.c32 "$w/iso/isolinux/"
printf 'DEFAULT hangar\nPROMPT 0\nTIMEOUT 0\nLABEL hangar\n  LINUX /vmlinuz\n  INITRD /initrd.img\n  APPEND %s\n' "$cmdline" \
    > "$w/iso/isolinux/isolinux.cfg"
install -m 0644 "$kernel" "$w/iso/vmlinuz"
install -m 0755 "$agent" "$w/initrd/init"
install -m 0755 "$mke2fs" "$w/initrd/mke2fs"
(cd "$w/initrd" && printf 'init\nmke2fs\n' | cpio -o -H newc -R 0:0 --quiet) > "$w/iso/initrd.img"
install -m 0755 "$worker" "$w/iso/hangar-worker"
install -m 0644 "$ca" "$w/iso/ca-certificates.crt"
printf '%s\n' "$version" > "$w/iso/VERSION"
xorriso -as mkisofs -quiet -o "$out" -V HANGAR -R -J \
    -b isolinux/isolinux.bin -c isolinux/boot.cat \
    -no-emul-boot -boot-load-size 4 -boot-info-table "$w/iso"
