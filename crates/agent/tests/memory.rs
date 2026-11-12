use hangar_agent::memory::{ram_span, scattered_free};

/// As an arm64 guest with virtio-mem has it, its device region beyond a
/// hole above the RAM it booted with, part of it plugged in.
const ARM64: &str = "08ff0000-08ffffff : GICD
09000000-09000fff : pl011@9000000
  09000000-09000fff : 9000000.pl011 pl011@9000000
10000000-2fffffff : pci@30000000
    10000000-1007ffff : virtio-pci-modern
  10000000-1007ffff : 0000:00:01.0
    10000000-1007ffff : virtio1
30000000-300fffff : PCI ECAM
40000000-7fffffff : System RAM
  40000000-40000fff : reserved
  40400000-4170ffff : Kernel code
100000000-32fffffff : virtio6
  100000000-13fffffff : System RAM (virtio_mem)
330000000-ffffeeffff : pci@30000000
";

#[test]
fn ram_spans() {
    assert_eq!(ram_span(ARM64), Ok((0x40000000, 0x330000000)), "virtio-mem");
    let fixed = "00000000-00000fff : Reserved\n00001000-0009fbff : System RAM\n00100000-bfffffff : System RAM\n100000000-23fffffff : System RAM\n";
    assert_eq!(ram_span(fixed), Ok((0x1000, 0x240000000)), "fixed memory");
    assert!(
        ram_span("00000000-00000000 : System RAM\n").is_err(),
        "addresses hidden from an unprivileged reader gave a span"
    );
}

/// Free memory in blocks smaller than free page reporting's order counts;
/// whole blocks do not.
#[test]
fn scattered_free_memory() {
    let buddyinfo = "Node 0, zone   Normal  3 2 1 0 0 0 0 0 0 5 7\n";
    // Order 0 to 8 at 4 KiB pages: 3 pages, 2 pairs, 1 quad.
    assert_eq!(scattered_free(buddyinfo, 9, 4096), (3 + 4 + 4) * 4096);
    assert_eq!(scattered_free(buddyinfo, 1, 4096), 3 * 4096);
}
