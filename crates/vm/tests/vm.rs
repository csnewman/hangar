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

#[test]
fn gpu_env() {
    use hangar_vm::ch::gpu_env;
    let env = gpu_env("auto", "").unwrap();
    assert_eq!(
        env,
        [("EGL_PLATFORM".to_string(), "surfaceless".to_string())]
    );
    let env = gpu_env("software", "").unwrap();
    assert_eq!(
        env[..2],
        [
            ("EGL_PLATFORM".to_string(), "surfaceless".to_string()),
            ("LIBGL_ALWAYS_SOFTWARE".to_string(), "1".to_string())
        ]
    );
    assert!(gpu_env("software", "/dev/dri/renderD128").is_err());
    assert!(gpu_env("fast", "").is_err());
    assert_eq!(
        hangar_vm::ch::ready_path(std::path::Path::new("/e/gpu.state")),
        std::path::Path::new("/e/gpu.ready")
    );
}

// A guest's connection to a port arrives on that port's socket, reported as
// the environment's; a stale socket is replaced, anything else refused.
#[tokio::test]
async fn hybrid_vsock() {
    use hangar_vm::vsock::{HybridListener, hybrid_path};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let d = PathBuf::from(format!("/tmp/hvm-{}-vsock", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let base = d.join("v");
    assert_eq!(hybrid_path(&base, 8100), d.join("v_8100"));
    drop(std::os::unix::net::UnixListener::bind(d.join("v_8100")).unwrap());
    let l = HybridListener::listen(&base, 8100, 42).unwrap();
    let mut guest = tokio::net::UnixStream::connect(d.join("v_8100"))
        .await
        .unwrap();
    let (mut c, cid) = l
        .accept(Some(std::time::Duration::from_secs(5)))
        .await
        .unwrap();
    assert_eq!(cid, 42);
    guest.write_all(b"hello").await.unwrap();
    let mut b = [0u8; 5];
    c.read_exact(&mut b).await.unwrap();
    assert_eq!(&b, b"hello");
    let e = l
        .accept(Some(std::time::Duration::from_millis(50)))
        .await
        .unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::TimedOut);
    drop(l);
    assert!(
        !d.join("v_8100").exists(),
        "the socket outlived its listener"
    );

    std::fs::write(d.join("v_2049"), "not a socket").unwrap();
    assert!(HybridListener::listen(&base, 2049, 42).is_err());
    let long = d.join("x".repeat(120));
    assert!(HybridListener::listen(&long, 1, 1).is_err());
    let _ = std::fs::remove_dir_all(d);
}

// The guest's /proc, as the agent reads it back: meminfo, net/dev,
// diskstats and vmstat, one after the other.
#[test]
fn guest_stats() {
    use hangar_vm::stats::{self, Counters};
    let text = "MemTotal:        4028512 kB\nMemFree:          100000 kB\nMemAvailable:    3004512 kB\n\
        Buffers:           10240 kB\nCached:           512000 kB\nShmem:             20480 kB\nMlocked:            2048 kB\n\
        SwapTotal:       4194304 kB\nSwapFree:        3145728 kB\nZswap:             65536 kB\nZswapped:         262144 kB\n\
        Inter-|   Receive                                                |  Transmit\n \
        face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n    \
        lo:  999999     100    0    0    0     0          0         0   999999     100    0    0    0     0       0          0\n  \
        eth0: 1000000    2000    0    0    0     0          0         0   500000    1000    0    0    0     0       0          0\n \
        253       0 vda 100 0 2048 50 200 0 4096 80 0 100 130 0 0 0 0\n \
        253       1 vda1 100 0 2048 50 200 0 4096 80 0 100 130 0 0 0 0\n \
        253      16 vdb 10 0 8 5 20 0 16 8 0 10 13 0 0 0 0\n\
        pswpin 10\npswpout 30\nzswpin 100\nzswpout 200\nzswpwb 5\nnr_free_pages 25000\n";
    let mut st = hangar_api::EnvironmentStats::default();
    let mut c = Counters::new(std::time::Instant::now());
    stats::guest(text, &mut st, &mut c);
    assert_eq!((st.memory_total_mib, st.memory_used_mib), (3934, 1000));
    assert_eq!((st.memory_cached_mib, st.memory_locked_mib), (490, 2));
    assert_eq!((st.swap_total_mib, st.swap_used_mib), (4096, 1024));
    assert_eq!((st.swap_compressed_mib, st.swap_pool_mib), (256, 64));
    assert_eq!(
        (c.net_rx, c.net_tx),
        (1_000_000, 500_000),
        "loopback counted"
    );
    assert_eq!(
        (c.disk_read, c.disk_write),
        ((2048 + 8) * 512, (4096 + 16) * 512),
        "a partition counted"
    );
    assert_eq!((c.swap_in, c.swap_out), (110, 225));

    let mut later = c;
    later.at = c.at + std::time::Duration::from_secs(2);
    later.cpu_ticks = c.cpu_ticks + 100;
    later.net_rx = c.net_rx + 4096;
    later.swap_out = c.swap_out + 10;
    stats::rates(&c, &later, 2, 16384.0, &mut st);
    assert_eq!(st.cpu_percent, 25.0);
    assert_eq!(st.net_rx_bps, 2048.0);
    assert_eq!(st.swap_out_bps, 5.0 * 16384.0);
}

#[test]
fn host_procs() {
    use hangar_vm::stats::{HostProcs, Proc, parse_smaps, parse_stat};
    let p = parse_stat(
        "1234 (hangar (gpu) x) S 1 1234 1234 0 -1 4194560 100 0 0 0 250 50 7 3 20 0 9 0 100 0\n",
    )
    .unwrap();
    assert_eq!(
        p,
        Proc {
            ppid: 1,
            ticks: 300,
            reaped: 10
        }
    );

    let ps = HostProcs(
        [
            (10, (1, 5, 1)),
            (11, (10, 7, 0)),
            (12, (11, 3, 0)),
            (13, (10, 2, 0)),
            (20, (1, 100, 0)),
        ]
        .into_iter()
        .map(|(pid, (ppid, ticks, reaped))| {
            (
                pid,
                Proc {
                    ppid,
                    ticks,
                    reaped,
                },
            )
        })
        .collect(),
    );
    let mut tree = ps.tree(10);
    assert_eq!(tree.remove(0), 10);
    tree.sort();
    assert_eq!(tree, [11, 12, 13]);
    assert_eq!(ps.tree_ticks(10), 1 + 5 + 7 + 3 + 2);
    assert!(ps.tree(99).is_empty());

    let smaps = "\
7f0000000000-7f0040000000 rw-s 00000000 00:01 1234 /memfd:ch_ram (deleted)\nRss: 524288 kB\nPss: 524288 kB\n\
7f1000000000-7f1000100000 rw-p 00000000 00:00 0\nRss: 1024 kB\nPss: 1000 kB\n\
7f2000000000-7f2080000000 rw-p 00000000 00:00 0\nRss: 2097152 kB\nPss: 2097152 kB\n\
5555000-5556000 r-xp 00000000 08:01 99 /usr/bin/cloud-hypervisor\nRss: 4 kB\nPss: 4 kB\n\
7f3000000000-7f3000010000 rw-s 00000000 00:05 7 /dev/dri/renderD128\nRss: 64 kB\nPss: 32 kB\n\
7ffc000-7ffd000 rw-p 00000000 00:00 0 [stack]\nRss: 8 kB\nPss: 8 kB\n";
    assert_eq!(
        parse_smaps(smaps),
        ((524288 + 2097152) << 10, (1000 + 32 + 8) << 10)
    );
}

// A suspend record matches the same machine, and not one whose base or
// read-only disks were made again.
#[test]
fn suspend_record() {
    use hangar_vm::suspend::{self, Record};
    let d = scratch("suspend");
    let f = d.join("image.erofs");
    std::fs::write(&f, "one").unwrap();
    let first = hangar_vm::suspend::fingerprint(&f);
    std::fs::remove_file(&f).unwrap();
    std::fs::write(&f, "one").unwrap();
    assert_ne!(
        first,
        suspend::fingerprint(&f),
        "a file made again has the same fingerprint"
    );
    assert_eq!(suspend::fingerprint(&d.join("none")), "");

    let r = Record {
        base: "b".into(),
        image: "disk:false:x".into(),
        read_only: vec!["r".into()],
        krun_memory: None,
    };
    let pulled = Record {
        base: String::new(),
        ..r.clone()
    };
    assert!(
        r.same_machine(&pulled),
        "a pulled copy has no base to compare"
    );
    assert!(!r.same_machine(&Record {
        image: "pmem:true:x".into(),
        ..r.clone()
    }));
    assert!(!r.same_machine(&Record {
        read_only: vec![],
        ..r.clone()
    }));

    assert!(suspend::suspended(&d).is_none());
    std::fs::create_dir_all(d.join("snapshot")).unwrap();
    suspend::mark(&d, &r).unwrap();
    assert_eq!(suspend::suspended(&d), Some(r));
    suspend::discard(&d);
    assert!(suspend::suspended(&d).is_none() && !d.join("snapshot").exists());
    let _ = std::fs::remove_dir_all(d);
}

struct NoFiles;

impl hangar_nfs::View for NoFiles {
    fn sets(&self) -> Vec<String> {
        vec![]
    }
    fn hidden(&self, _: &str, _: &[u8]) -> bool {
        false
    }
    fn squash(&self, _: &str) -> hangar_nfs::Squash {
        hangar_nfs::Squash::All
    }
}

// The shared files are answered on their vsock port, and closing ends the
// guest's connections.
#[tokio::test]
async fn shared_files() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let d = PathBuf::from(format!("/tmp/hvm-{}-files", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("root")).unwrap();
    let files = hangar_vm::files::SharedFiles::serve(
        &d.join("v"),
        3,
        &d.join("root"),
        std::sync::Arc::new(NoFiles),
    )
    .unwrap();
    let mut c = tokio::net::UnixStream::connect(d.join("v_2049"))
        .await
        .unwrap();
    // An RPC call to NFS's NULL procedure: xid 9, a call, RPC 2, program
    // 100003 version 4 procedure 0, no credential or verifier.
    let call: Vec<u8> = [9u32, 0, 2, 100003, 4, 0, 0, 0, 0, 0]
        .iter()
        .flat_map(|v| v.to_be_bytes())
        .collect();
    let mut rec = (0x8000_0000u32 | call.len() as u32).to_be_bytes().to_vec();
    rec.extend(call);
    c.write_all(&rec).await.unwrap();
    let mut reply = [0u8; 28];
    c.read_exact(&mut reply).await.unwrap();
    let word = |i: usize| u32::from_be_bytes(reply[i * 4..i * 4 + 4].try_into().unwrap());
    assert_eq!(
        (word(1), word(2), word(3), word(6)),
        (9, 1, 0, 0),
        "not an accepted reply to the call"
    );
    files.close();
    let mut rest = vec![];
    let n = tokio::time::timeout(std::time::Duration::from_secs(5), c.read_to_end(&mut rest))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(n, 0);
    drop(files);
    assert!(!d.join("v_2049").exists());
    let _ = std::fs::remove_dir_all(d);
}
