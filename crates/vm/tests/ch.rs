//! Cloud Hypervisor's command line, held to what internal/ch gives for the
//! same machines, and its API spoken to a stand-in for the monitor.

use std::path::{Path, PathBuf};
use std::time::Duration;

use hangar_vm::ch::{self, Api, Config, Disk, Memory};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

fn p(s: &str) -> PathBuf {
    PathBuf::from(s)
}

fn cmdline(tty: &str, extra: &str) -> String {
    let mut s = format!(
        "console={tty} systemd.show_status=1 mitigations=off memory_hotplug.online_policy=auto-movable \
         damon_reclaim.min_age=300000000 damon_reclaim.skip_anon=Y \
         damon_reclaim.wmarks_high=1000 damon_reclaim.wmarks_mid=999 damon_reclaim.wmarks_low=0"
    );
    if !extra.is_empty() {
        s += " ";
        s += extra;
    }
    s
}

#[test]
fn args() {
    let mut a = Config {
        name: "env".into(),
        kernel: p("/k/vmlinuz"),
        initrd: Some(p("/e/initrd")),
        memory_mb: 8192,
        memory_max_mb: 16384,
        cpus: 12,
        disks: vec![
            Disk {
                path: p("/e/upper.ext4"),
                ..Default::default()
            },
            Disk {
                path: p("/e/swap.img"),
                direct: true,
                ..Default::default()
            },
            Disk {
                path: p("/i/image.erofs"),
                read_only: true,
                ..Default::default()
            },
        ],
        console_file: p("/e/console.log"),
        console_tty: "ttyS0".into(),
        extra_cmdline: "hangar.image=/dev/vdc quiet".into(),
        guest_cid: 7,
        vsock_socket: p("/e/vsock.sock"),
        pmem: vec![p("/i/a.erofs")],
        net_socket: Some(p("/tmp/hangar-x/passt.sock")),
        gpu_socket: Some(p("/e/gpu.sock")),
        gpu_shm_mib: 4096,
        api_socket: Some(p("/e/api.sock")),
    };
    let cl = cmdline("ttyS0", "hangar.image=/dev/vdc quiet");
    let want = [
        "--kernel",
        "/k/vmlinuz",
        "--cpus",
        "boot=12",
        "--memory",
        "size=2048M,shared=on,hotplug_method=virtio-mem,hotplug_size=14336M,hotplugged_size=6144M",
        "--balloon",
        "size=0,free_page_reporting=on",
        "--pvpanic",
        "--initramfs",
        "/e/initrd",
        "--console",
        "off",
        "--serial",
        "file=/e/console.log",
        "--disk",
        "path=/e/upper.ext4,image_type=raw,num_queues=8",
        "path=/e/swap.img,image_type=raw,num_queues=8,direct=on",
        "path=/i/image.erofs,image_type=raw,num_queues=8,readonly=on",
        "--pmem",
        "file=/i/a.erofs,discard_writes=on",
        "--vsock",
        "cid=7,socket=/e/vsock.sock",
        "--net",
        "vhost_user=on,socket=/tmp/hangar-x/passt.sock",
        "--generic-vhost-user",
        "socket=/e/gpu.sock,device_type=gpu,queue_sizes=[256,256],shm_size=4096M,shm_id=1",
        "--api-socket",
        "/e/api.sock",
        "--cmdline",
        &cl,
    ];
    assert_eq!(a.args().unwrap(), want);

    let mut b = Config {
        kernel: p("/k"),
        console_file: p("/c"),
        disks: vec![Disk {
            path: p("/d"),
            ..Default::default()
        }],
        ..Default::default()
    };
    let cl = cmdline("ttyAMA0", "");
    let want = [
        "--kernel",
        "/k",
        "--cpus",
        "boot=2",
        "--memory",
        "size=4096M,shared=on",
        "--balloon",
        "size=0,free_page_reporting=on",
        "--pvpanic",
        "--console",
        "off",
        "--serial",
        "file=/c",
        "--disk",
        "path=/d,image_type=raw,num_queues=2",
        "--cmdline",
        &cl,
    ];
    assert_eq!(b.args().unwrap(), want);

    assert!(
        Config::default().args().is_err(),
        "a machine with no kernel"
    );
}

// The layout of resizable memory, as internal/ch's Hotplug and Fit give it.
#[test]
fn hotplug() {
    for [mb, max, boot, region, plugged] in [
        [4096, 0, 4096, 0, 0],
        [4096, 4096, 4096, 0, 0],
        [4096, 16384, 1024, 15360, 3072],
        [2048, 16384, 1024, 15360, 1024],
        [1000, 3000, 1000, 2048, 0],
        [8192, 65536, 2048, 63488, 6144],
        [12000, 12345, 2944, 9472, 9056],
        [3000, 3001, 1024, 2048, 1976],
    ] {
        assert_eq!(
            ch::hotplug(mb, max),
            (boot, region, plugged),
            "{mb} up to {max}"
        );
    }
    let m = Memory {
        boot: 2048,
        max: 16384,
        ..Default::default()
    };
    for (mb, fit) in [
        (0, 2048),
        (1000, 2048),
        (2048, 2048),
        (2049, 2050),
        (3000, 3000),
        (16384, 16384),
        (20000, 16384),
    ] {
        assert_eq!(m.fit(mb), fit, "{mb}");
    }
}

fn scratch(name: &str) -> PathBuf {
    // Short: a socket's path has to fit in 104 bytes on macOS.
    let d = PathBuf::from(format!("/tmp/hvm-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A stand-in for the monitor's API: answers each request with the next of
/// replies, and gives back what it was asked.
async fn monitor(
    socket: &Path,
    replies: Vec<(&'static str, &'static str)>,
) -> tokio::task::JoinHandle<Vec<String>> {
    let l = tokio::net::UnixListener::bind(socket).unwrap();
    tokio::spawn(async move {
        let mut asked = vec![];
        let mut replies = replies.into_iter().peekable();
        while replies.peek().is_some() {
            let (c, _) = l.accept().await.unwrap();
            let mut r = BufReader::new(c);
            let mut head = String::new();
            let mut length = 0;
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).await.unwrap() == 0 {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap();
                }
                if line == "\r\n" {
                    break;
                }
                if head.is_empty() {
                    head = line.trim().to_string();
                }
            }
            let mut b = vec![0; length];
            r.read_exact(&mut b).await.unwrap();
            asked.push(format!("{head} {}", String::from_utf8(b).unwrap()));
            if head.is_empty() {
                continue;
            }
            let (status, body) = replies.next().unwrap();
            // As the monitor's micro_http answers: the connection kept.
            let reply = format!(
                "HTTP/1.1 {status}\r\nServer: Cloud Hypervisor API\r\nConnection: keep-alive\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            r.get_mut().write_all(reply.as_bytes()).await.unwrap();
        }
        asked
    })
}

#[tokio::test]
async fn api() {
    let d = scratch("api");
    let socket = d.join("api.sock");
    let info = r#"{"config":{"memory":{"size":2147483648,"hotplug_size":15032385536,"hotplugged_size":3221225472,"shared":true}},"state":"Running","memory_actual_size":4294967296}"#;
    let srv = monitor(
        &socket,
        vec![
            ("204 No Content", ""),
            ("200 OK", info),
            ("204 No Content", ""),
            (
                "500 Internal Server Error",
                "Error from API: snapshot failed",
            ),
        ],
    )
    .await;
    let api = Api::new(&socket);
    api.wait_ready(Duration::from_secs(5)).await.unwrap();
    api.pause().await.unwrap();
    let m = api.memory().await.unwrap();
    assert_eq!(
        m,
        Memory {
            boot: 2048,
            max: 16384,
            requested: 5120,
            actual: 4096
        }
    );
    api.resize_memory(6144).await.unwrap();
    let e = api.snapshot(Path::new("/e/snap")).await.unwrap_err();
    assert!(e.to_string().contains("snapshot failed"), "{e}");
    let asked = srv.await.unwrap();
    assert_eq!(
        asked,
        [
            // wait_ready's connection, which sends nothing.
            " ",
            "PUT /api/v1/vm.pause HTTP/1.1 ",
            "GET /api/v1/vm.info HTTP/1.1 ",
            r#"PUT /api/v1/vm.resize HTTP/1.1 {"desired_ram":6442450944}"#,
            r#"PUT /api/v1/vm.snapshot HTTP/1.1 {"destination_url":"file:///e/snap"}"#,
        ]
    );
    let _ = std::fs::remove_dir_all(d);
}

// A backend that exits says why; one that makes its socket is running.
#[tokio::test]
async fn launch() {
    let d = scratch("launch");
    let socket = d.join("b.sock");
    let mut cmd = tokio::process::Command::new("/bin/sh");
    cmd.args(["-c", "echo 'unknown option --frob' >&2; exit 3"]);
    let e = ch::launch(cmd, "frob", Some(&socket), None, Duration::from_secs(5))
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(
        e.contains("frob exited before creating") && e.contains("unknown option --frob"),
        "{e}"
    );

    let log = d.join("b.log");
    let mut cmd = tokio::process::Command::new("/bin/sh");
    cmd.arg("-c").arg(format!(
        "echo started >&2; touch {}; exec sleep 30",
        socket.display()
    ));
    let b = ch::launch(
        cmd,
        "sleeper",
        Some(&socket),
        Some(&log),
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert!(b.exited().is_none());
    b.stop().await;
    assert!(b.exited().is_some());
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "started\n");

    let mut cmd = tokio::process::Command::new("/bin/sleep");
    cmd.arg("30");
    let e = ch::launch(
        cmd,
        "slow",
        Some(&d.join("never")),
        None,
        Duration::from_millis(200),
    )
    .await
    .err()
    .unwrap()
    .to_string();
    assert!(e.contains("slow did not create"), "{e}");
    let _ = std::fs::remove_dir_all(d);
}
