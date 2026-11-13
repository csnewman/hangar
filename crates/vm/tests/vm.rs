use std::path::PathBuf;

use hangar_vm::hugepages::{self, Counts};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hangar-vm-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

// The initramfs is a newc cpio holding the agent as /init, as the kernel
// unpacks it.
#[test]
fn initrd() {
    let d = scratch("initrd");
    std::fs::write(d.join("agent"), b"agent!").unwrap();
    hangar_vm::write_initrd(&d.join("agent"), &d.join("initrd")).unwrap();
    let got = std::fs::read(d.join("initrd")).unwrap();
    // As internal/vm's WriteInitrd writes it.
    let want: &[u8] = b"07070100000001000081ED00000000000000000000000100000000000000060000000000\
        00000000000000000000000000000500000000init\x00\x00agent!\x00\x0007070100000002000000\
        000000000000000000000000010000000000000000000000000000000000000000000000\
        000000000B00000000TRAILER!!!\x00\x00\x00\x00";
    assert_eq!(String::from_utf8_lossy(&got), String::from_utf8_lossy(want));
    assert!(!d.join("initrd.new").exists());
    let _ = std::fs::remove_dir_all(d);
}

// A swap disk is made sparse and empty at its size, and taken away for none
// or once its machine stops, unless it is suspended.
#[test]
fn swap() {
    let d = scratch("swap");
    let img = d.join("swap.img");
    hangar_vm::make_swap(&d, 64).unwrap();
    assert_eq!(std::fs::metadata(&img).unwrap().len(), 64 << 20);
    hangar_vm::drop_swap(&d, true);
    assert!(img.exists(), "a suspended machine's swap was dropped");
    hangar_vm::drop_swap(&d, false);
    assert!(!img.exists());
    hangar_vm::make_swap(&d, 64).unwrap();
    hangar_vm::make_swap(&d, 0).unwrap();
    assert!(!img.exists());
    hangar_vm::make_swap(&d, 0).unwrap();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn huge_page_policy() {
    assert!(hugepages::check_mode("always within_size [advise] never deny force\n").is_ok());
    assert!(hugepages::check_mode("[always] within_size advise never deny force").is_ok());
    let never = hugepages::check_mode("always within_size advise [never] deny force").unwrap_err();
    assert!(never.to_string().contains("\"never\""), "{never}");
    assert!(hugepages::check_mode("always advise never").is_err());
}

#[test]
fn huge_page_counts() {
    let vmstat = "nr_free_pages 12\nthp_file_alloc 40\nthp_file_mapped 3\nthp_file_fallback 7\n";
    assert_eq!(
        hugepages::parse_counts(vmstat).unwrap(),
        Counts {
            allocated: 40,
            fell_back: 7
        }
    );
    assert!(hugepages::parse_counts("thp_file_alloc 1\n").is_err());

    // 4 KiB pages, 2 MiB huge ones: order 9 and up count.
    let buddy = "Node 0, zone      DMA      1      0      0      0      0      0      0      0      0      2      3\n\
                 Node 0, zone   Normal    100     50      0      0      0      0      0      0      0      1      0\n";
    let page = 4096;
    let want = (2u64 << 9) * page + (3u64 << 10) * page + (1u64 << 9) * page;
    assert_eq!(
        hugepages::free_huge_blocks(buddy, page, 2 << 20).unwrap(),
        want
    );
}

#[test]
fn host() {
    let caps = hangar_vm::host::detect("");
    if cfg!(target_os = "macos") {
        let caps = caps.unwrap();
        assert_eq!(caps.monitor, hangar_vm::host::Monitor::Krun);
        assert_eq!(caps.console_tty, "hvc0");
        assert!(hangar_vm::host::detect("cloud-hypervisor").is_err());
    }
    assert!(hangar_vm::host::detect("qemu").is_err());
}
