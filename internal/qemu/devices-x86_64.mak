# x86-64-specific devices. QEMU's `q35` machine and its 8250-compatible UART.
#
# Separate from devices.mak so the build can insist every symbol it asks for
# survives: a machine for the other architecture is absent rather than
# disabled, which would make that check unenforceable in a shared file.
CONFIG_Q35=y
CONFIG_SERIAL_ISA=y
