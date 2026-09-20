# aarch64-specific devices. QEMU's `virt` machine and its PL011 UART.
#
# Separate from devices.mak so the build can insist every symbol it asks for
# survives: a machine for the other architecture is absent rather than
# disabled, which would make that check unenforceable in a shared file.
CONFIG_ARM_VIRT=y
CONFIG_PL011=y
