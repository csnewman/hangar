//! hangar-krun's config, as the worker writes it and hangar-krun reads it,
//! and connections to a guest's ports.

use std::collections::BTreeMap;
use std::path::PathBuf;

use hangar_vm::krun::{Config, Disk, Gpu, Net};

#[test]
fn config() {
    let c = Config {
        cpus: 4,
        memory_mib: 2048,
        hotplug_mib: 14336,
        plugged_mib: 6144,
        kernel: "/k".into(),
        initrd: "/i".into(),
        cmdline: "console=hvc0 x".into(),
        console: "/c".into(),
        disks: vec![
            Disk {
                id: "vda".into(),
                path: "/u".into(),
                ..Default::default()
            },
            Disk {
                id: "vdb".into(),
                path: "/s".into(),
                direct: true,
                ..Default::default()
            },
            Disk {
                id: "vdc".into(),
                path: "/e".into(),
                read_only: true,
                ..Default::default()
            },
        ],
        host_ports: BTreeMap::from([
            (8100, "/tmp/h/v_8100".into()),
            (2049, "/tmp/h/v_2049".into()),
        ]),
        guest_ports: BTreeMap::from([(101, "/tmp/h/v-g101".into())]),
        net: Some(Net {
            socket: "/tmp/h/net".into(),
            mac: "5a:94:ef:e4:0c:ee".into(),
            stream: true,
            ..Default::default()
        }),
        gpu: Some(Gpu {
            window_mib: 512,
            virgl: true,
        }),
        restore: "/d/snapshot".into(),
    };
    // The same machine's krun.json.
    let json = r#"{"cpus":4,"memory_mib":2048,"hotplug_mib":14336,"plugged_mib":6144,"kernel":"/k","initrd":"/i","cmdline":"console=hvc0 x","console":"/c","disks":[{"id":"vda","path":"/u"},{"id":"vdb","path":"/s","direct":true},{"id":"vdc","path":"/e","read_only":true}],"host_ports":{"2049":"/tmp/h/v_2049","8100":"/tmp/h/v_8100"},"guest_ports":{"101":"/tmp/h/v-g101"},"net":{"socket":"/tmp/h/net","mac":"5a:94:ef:e4:0c:ee","stream":true},"gpu":{"window_mib":512,"virgl":true},"restore":"/d/snapshot"}"#;
    assert_eq!(serde_json::to_string(&c).unwrap(), json);
    assert_eq!(serde_json::from_str::<Config>(json).unwrap(), c);
    // passt's connected socket, inherited.
    let inherited = Net {
        mac: "5a:94:ef:e4:0c:ee".into(),
        stream: true,
        fd: Some(4),
        ..Default::default()
    };
    let json = r#"{"socket":"","mac":"5a:94:ef:e4:0c:ee","stream":true,"fd":4}"#;
    assert_eq!(serde_json::to_string(&inherited).unwrap(), json);
    assert_eq!(serde_json::from_str::<Net>(json).unwrap(), inherited);
    let bare = Config {
        cpus: 1,
        memory_mib: 1024,
        ..Default::default()
    };
    assert_eq!(
        serde_json::to_string(&bare).unwrap(),
        r#"{"cpus":1,"memory_mib":1024,"kernel":"","initrd":"","cmdline":"","console":"","disks":[]}"#
    );
}

// Cloud Hypervisor's vsock socket is asked for the port, and what it sends
// past its answer is the guest's.
#[tokio::test]
async fn dial_cloud_hypervisor() {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    let d = PathBuf::from(format!("/tmp/hvm-{}-dial", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let sock = d.join("vsock.sock");
    let l = tokio::net::UnixListener::bind(&sock).unwrap();
    tokio::spawn(async move {
        for answer in ["OK 1073741824\nguest says hi", "connection refused\n"] {
            let (c, _) = l.accept().await.unwrap();
            let mut r = BufReader::new(c);
            let mut line = String::new();
            r.read_line(&mut line).await.unwrap();
            assert_eq!(line, "CONNECT 101\n");
            r.get_mut().write_all(answer.as_bytes()).await.unwrap();
        }
    });
    let mut c = hangar_vm::guest::dial_cloud_hypervisor(&sock, 101)
        .await
        .unwrap();
    let mut b = [0u8; 12];
    c.read_exact(&mut b).await.unwrap();
    assert_eq!(&b, b"guest says h");
    let e = hangar_vm::guest::dial_cloud_hypervisor(&sock, 101)
        .await
        .err()
        .unwrap();
    assert!(e.to_string().contains("connection refused"), "{e}");
    assert_eq!(
        hangar_vm::guest::krun_socket(&d.join("v"), 101),
        d.join("v-g101")
    );
    let _ = std::fs::remove_dir_all(d);
}
