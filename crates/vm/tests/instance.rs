//! A machine booted, resized, suspended and resumed under a stand-in for
//! hangar-krun, with a stand-in guest dialling the host as the agent does.

use std::path::{Path, PathBuf};
use std::time::Duration;

use hangar_vm::instance::{Instance, InstanceConfig};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Answers the worker's requests on descriptor 3, and stops when its stdin
/// closes or it is told to.
const FAKE_KRUN: &str = r#"#!/bin/sh
echo "fake hangar-krun $1" >&2
while read -r req; do
  case "$req" in
    memory) echo "512 1024" >&3 ;;
    "memory "*) echo "ok" >&3 ;;
    "suspend "*) mkdir -p "${req#suspend }" && echo "ok" >&3 ;;
    exit) exit 0 ;;
    *) echo "error: unknown request $req" >&3 ;;
  esac
done
"#;

/// Dials the agent's socket whenever it is there, says hello, and answers
/// every request as done.
async fn guest(socket: PathBuf) {
    loop {
        let Ok(c) = tokio::net::UnixStream::connect(&socket).await else {
            tokio::time::sleep(Duration::from_millis(20)).await;
            continue;
        };
        let (r, mut w) = c.into_split();
        let hello = r#"{"kind":"hello","version":1,"hostname":"fake","kernel":"7.0","boot_micros":1500000,"page_size":16384}"#;
        if w.write_all(format!("{hello}\n").as_bytes()).await.is_err() {
            continue;
        }
        let mut lines = BufReader::new(r).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            let req: serde_json::Value = serde_json::from_str(&l).unwrap();
            let reply = format!("{{\"id\":{},\"code\":0}}\n", req["id"]);
            if w.write_all(reply.as_bytes()).await.is_err() {
                break;
            }
        }
    }
}

fn krun_json(dir: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(dir.join("run/krun.json")).unwrap()).unwrap()
}

#[tokio::test]
async fn krun_lifecycle() {
    if hangar_vm::host::detect("krun").is_err() {
        eprintln!("skipped: this host cannot run machines under libkrun");
        return;
    }
    let id = format!("t{}", std::process::id());
    let dir = std::env::temp_dir().join(format!("hangar-vm-{id}-instance"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("hangar-krun");
    std::fs::write(&bin, FAKE_KRUN).unwrap();
    std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    for f in ["vmlinuz", "agent", "image.erofs", "upper.ext4"] {
        std::fs::write(dir.join(f), f).unwrap();
    }
    let cfg = InstanceConfig {
        id: id.clone(),
        name: "env".into(),
        monitor: "krun".into(),
        monitor_bin: Some(bin),
        dir: dir.join("env"),
        kernel: dir.join("vmlinuz"),
        agent: dir.join("agent"),
        base_file: dir.join("image.erofs"),
        disks: vec![hangar_vm::ch::Disk {
            path: dir.join("upper.ext4"),
            ..Default::default()
        }],
        memory_mib: 4096,
        memory_max_mib: 8192,
        swap_mib: 64,
        cpus: 2,
        console_file: dir.join("env/console.log"),
        agent_wait: Duration::from_secs(20),
        ..Default::default()
    };
    let g = tokio::spawn(guest(PathBuf::from(format!("/tmp/hangar-{id}/v_8100"))));

    let inst = Instance::boot(cfg.clone()).await.unwrap();
    assert!(!inst.resumed());
    assert_eq!(inst.session().hello.page_size, 16384);
    let kc = krun_json(&cfg.dir);
    // A quarter from boot, the rest plugged into a region reaching the most.
    assert_eq!(
        (&kc["memory_mib"], &kc["hotplug_mib"], &kc["plugged_mib"]),
        (&1024.into(), &7168.into(), &3072.into())
    );
    let disks: Vec<(String, bool, bool)> = kc["disks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["id"].as_str().unwrap().to_string(),
                d["read_only"].as_bool().unwrap_or(false),
                d["direct"].as_bool().unwrap_or(false),
            )
        })
        .collect();
    assert_eq!(
        disks,
        [
            ("vda".into(), false, false),
            ("vdb".into(), false, true),
            ("vdc".into(), true, false)
        ]
    );
    let cl = kc["cmdline"].as_str().unwrap();
    assert!(
        cl.starts_with("console=hvc0 ")
            && cl.contains(" hangar.swap=/dev/vdb")
            && cl.contains(" hangar.image=/dev/vdc"),
        "{cl}"
    );
    assert_eq!(
        std::fs::metadata(cfg.dir.join("swap.img")).unwrap().len(),
        64 << 20
    );

    let m = inst.memory().await.unwrap();
    assert_eq!(
        (m.boot, m.max, m.actual, m.requested),
        (1024, 8192, 1536, 2048)
    );
    assert_eq!(inst.resize_memory(3001).await.unwrap(), 3002);

    inst.suspend().await.unwrap();
    assert!(
        inst.has_exited() && inst.err().is_none(),
        "{:?}",
        inst.err()
    );
    let rec = hangar_vm::suspend::suspended(&cfg.dir).expect("a suspend record");
    assert_eq!(
        rec.krun_memory,
        Some(hangar_vm::suspend::KrunMemory {
            boot: 1024,
            region: 7168
        })
    );
    assert!(cfg.dir.join("snapshot").is_dir() && cfg.dir.join("swap.img").exists());
    drop(inst);

    // Resumed as it was laid out, from its snapshot, which is then spent.
    let inst = Instance::boot(InstanceConfig {
        memory_mib: 6000,
        ..cfg.clone()
    })
    .await
    .unwrap();
    assert!(inst.resumed());
    let kc = krun_json(&cfg.dir);
    assert_eq!(
        (&kc["memory_mib"], &kc["hotplug_mib"]),
        (&1024.into(), &7168.into())
    );
    assert_eq!(
        kc["restore"].as_str().unwrap(),
        cfg.dir.join("snapshot").to_str().unwrap()
    );
    assert!(hangar_vm::suspend::suspended(&cfg.dir).is_none());

    inst.close().await;
    assert!(inst.has_exited());
    assert!(
        !cfg.dir.join("swap.img").exists(),
        "a stopped machine kept its swap"
    );
    assert!(!Path::new(&format!("/tmp/hangar-{id}")).exists());
    g.abort();
    let _ = std::fs::remove_dir_all(dir);
}
