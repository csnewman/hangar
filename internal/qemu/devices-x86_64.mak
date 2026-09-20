# x86-64-specific devices. QEMU's `microvm` machine and its 8250-compatible
# UART.
#
# microvm rather than q35. q35 is a PC: it hard-selects an AHCI SATA
# controller, an i8042 PS/2 keyboard controller, a PC speaker, an i8257 DMA
# controller, an ICH9 LPC bridge and an SMBus, all instantiated in every
# guest and none of them addressable by a Hangar environment. microvm carries
# none of those, and with pcie=on it still has the generic PCIe host bridge
# that virtio-gpu, virtio-blk and virtio-net attach to.
#
# Measured on x86-64 under KVM with -cpu host, booting an initramfs:
#
#                        kernel to init    sleep overshoot   virtio-gpu
#   q35                      0.661 s           74.7 us         binds
#   microvm,pcie=on          0.394 s           67.5 us         binds
#
# microvm needs -cpu host, and this is a sharp edge rather than a preference.
# It has no PIT for the kernel to calibrate the LAPIC timer against, so
# without the TSC-deadline timer that -cpu host exposes the kernel finds no
# one-shot clockevent and every timer falls back to tick granularity -- 3.9ms
# instead of 67us, with nothing logged to say so. internal/vm always passes
# -cpu host under an accelerator.
#
# Separate from devices.mak so the build can insist every symbol it asks for
# survives: a machine for the other architecture is absent rather than
# disabled, which would make that check unenforceable in a shared file.
CONFIG_MICROVM=y
CONFIG_SERIAL_ISA=y
