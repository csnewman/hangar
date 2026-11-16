//! A Proxmox VM's memory sizes under hotplug: its static gigabyte, then
//! whole DIMMs, 512 MiB for the first 32 and twice that for each 32 after.

use hangar_vm::proxmox::{STATIC_MIB, fit_memory};

#[test]
fn never_below_the_static_memory() {
    assert_eq!(STATIC_MIB, 1024);
    assert_eq!(fit_memory(0, 8192), 1024);
    assert_eq!(fit_memory(512, 8192), 1024);
    assert_eq!(fit_memory(1024, 8192), 1024);
}

#[test]
fn rounds_up_to_whole_dimms() {
    assert_eq!(fit_memory(1025, 8192), 1536);
    assert_eq!(fit_memory(2048, 8192), 2048);
    assert_eq!(fit_memory(3000, 8192), 3072);
    // Past the first 32 DIMMs (1024 + 32 * 512), they are 1 GiB each.
    assert_eq!(fit_memory(17408, 1 << 20), 17408);
    assert_eq!(fit_memory(17409, 1 << 20), 18432);
}

#[test]
fn at_most_the_largest_size_within_the_limit() {
    assert_eq!(fit_memory(100_000, 8192), 8192);
    assert_eq!(fit_memory(8000, 7000), 6656);
    assert_eq!(fit_memory(4096, 1000), 1024);
    // Eight rounds of 32 DIMMs, from 512 MiB, are the most a VM can have.
    assert_eq!(fit_memory(u32::MAX, u32::MAX), 1024 + 32 * 512 * 255);
}
