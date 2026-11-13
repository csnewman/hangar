//! The runtime holding one environment through its life -- started,
//! suspended, resumed, stopped and deleted -- as the worker hands it specs,
//! under a stand-in for hangar-krun and a stand-in guest.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hangar_api::{DesiredState, EnvironmentSpec, Phase};
use hangar_vm::runtime::{Config, Runtime};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Answers the worker's requests on descriptor 3 and stops when told to,
/// when its stdin closes, or when the guest powers off (a file the
/// stand-in guest leaves beside its config).
const FAKE_KRUN: &str = r#"#!/usr/bin/env python3
import os, sys, threading, time
if sys.argv[1] == "-features":
    print('{"gpu":false}')
    sys.exit(0)
run = os.path.dirname(sys.argv[1])
off = os.path.join(run, "poweroff")
if os.path.exists(off):
    os.remove(off)
out = os.fdopen(3, "w", buffering=1)
def watch():
    while not os.path.exists(off):
        time.sleep(0.05)
    os._exit(0)
threading.Thread(target=watch, daemon=True).start()
for line in sys.stdin:
    req = line.strip()
    if req == "memory":
        out.write("0 0\n")
    elif req.startswith("suspend "):
        os.makedirs(req[len("suspend "):], exist_ok=True)
        out.write("ok\n")
    elif req == "exit":
        os._exit(0)
    else:
        out.write("error: unknown request\n")
os._exit(0)
"#;

/// Dials the agent's socket whenever it is there and answers everything as
/// done; asked to power off, it tells the stand-in monitor to stop.
async fn guest(socket: PathBuf, run: PathBuf, asked: Arc<Mutex<Vec<Vec<String>>>>) {
    loop {
        let Ok(c) = tokio::net::UnixStream::connect(&socket).await else {
            tokio::time::sleep(Duration::from_millis(20)).await;
            continue;
        };
        let (r, mut w) = c.into_split();
        if w.write_all(b"{\"kind\":\"hello\",\"version\":1,\"page_size\":16384}\n")
            .await
            .is_err()
        {
            continue;
        }
        let mut lines = BufReader::new(r).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            let req: serde_json::Value = serde_json::from_str(&l).unwrap();
            if let Some(cmd) = req["cmd"].as_array() {
                let cmd = cmd
                    .iter()
                    .map(|c| c.as_str().unwrap_or_default().to_string())
                    .collect();
                asked.lock().unwrap().push(cmd);
            }
            if req["cmd"][1] == "poweroff" {
                std::fs::write(run.join("poweroff"), "").unwrap();
            }
            let reply = format!("{{\"id\":{},\"code\":0}}\n", req["id"]);
            if w.write_all(reply.as_bytes()).await.is_err() {
                break;
            }
        }
    }
}

async fn wait(rt: &Arc<Runtime>, id: &str, want: Option<Phase>) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let got = rt
            .observe()
            .into_iter()
            .find(|o| o.id == id)
            .map(|o| (o.phase, o.reason));
        if got.as_ref().map(|g| g.0) == want {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "waiting for {want:?}, the environment is {got:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn exe(p: &Path, text: &str) {
    std::fs::write(p, text).unwrap();
    std::fs::set_permissions(p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

#[tokio::test]
async fn environment_lifecycle() {
    if hangar_vm::host::detect("krun").is_err() {
        eprintln!("skipped: this host cannot run machines under libkrun");
        return;
    }
    let id = format!("e{}", std::process::id());
    let d = std::env::temp_dir().join(format!("hangar-vm-{id}-runtime"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("src/usr/share/hangar")).unwrap();
    std::fs::write(
        d.join("src/usr/share/hangar/image.json"),
        r#"{"desktop":false,"build":"b1"}"#,
    )
    .unwrap();
    std::fs::write(d.join("src/hello"), "hello").unwrap();
    exe(&d.join("hangar-krun"), FAKE_KRUN);
    exe(&d.join("mkfs"), "#!/bin/sh\n");
    // Makes the socket the guest's frames go to, and waits to be stopped.
    exe(
        &d.join("gvproxy"),
        "#!/bin/sh\ntouch \"${4#unixgram://}\"\nexec sleep 300\n",
    );
    for f in ["vmlinuz", "agent"] {
        std::fs::write(d.join(f), f).unwrap();
    }
    let rt = Runtime::new(Config {
        monitor: "krun".into(),
        state_dir: d.join("env"),
        images_dir: d.join("images"),
        kernel: d.join("vmlinuz"),
        agent: d.join("agent"),
        images: [("local/base:1".to_string(), d.join("src"))].into(),
        upper_gib: 1,
        docker_gib: 1,
        monitor_bin: Some(d.join("hangar-krun")),
        mkfs: Some(d.join("mkfs")),
        gvproxy: Some(d.join("gvproxy")),
        boot_timeout: Duration::from_secs(20),
        ..Default::default()
    })
    .await
    .unwrap();
    assert!(rt.gpu().is_none());
    let run = d.join("env").join(&id).join("run");
    let asked = Arc::new(Mutex::new(vec![]));
    let g = tokio::spawn(guest(
        PathBuf::from(format!("/tmp/hangar-{id}/v_8100")),
        run,
        asked.clone(),
    ));
    // What the guest was last told of reclaim: the reclaim file's content.
    let reclaim = || {
        asked
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|c| {
                c.iter()
                    .any(|a| a.contains(hangar_vm::provision::RECLAIM_FILE))
            })
            .and_then(|c| c.last().cloned())
    };

    let mut spec: EnvironmentSpec = serde_json::from_value(serde_json::json!({
        "id": id, "name": "test", "desired": "running",
        "spec": {"image": "local/base:1", "cpus": 2, "memory_mib": 2048, "display": "none", "gpu": "none"}
    }))
    .unwrap();
    rt.apply(spec.clone());
    wait(&rt, &id, Some(Phase::Running)).await;
    let o = rt.observe().into_iter().find(|o| o.id == id).unwrap();
    assert!(o.image_pinned && !o.image_digest.is_empty(), "{o:?}");
    let images = rt.images();
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].environments, std::slice::from_ref(&id));
    assert!(d.join("env").join(&id).join("provisioned").exists());
    assert_eq!(
        reclaim().as_deref(),
        Some("0"),
        "reclaim was not off from the start"
    );

    // Turned on as it runs, the guest is told at once.
    spec.spec.memory_reclaim = true;
    rt.apply(spec.clone());
    let deadline = Instant::now() + Duration::from_secs(10);
    while reclaim().as_deref() != Some("1") {
        assert!(
            Instant::now() < deadline,
            "the guest was not told to reclaim"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    spec.desired = DesiredState::Suspended;
    rt.apply(spec.clone());
    wait(&rt, &id, Some(Phase::Suspended)).await;
    assert!(d.join("env").join(&id).join("suspended.json").exists());

    spec.desired = DesiredState::Running;
    rt.apply(spec.clone());
    wait(&rt, &id, Some(Phase::Running)).await;
    assert!(
        !d.join("env").join(&id).join("suspended.json").exists(),
        "the spent snapshot was kept"
    );

    spec.desired = DesiredState::Stopped;
    rt.apply(spec.clone());
    wait(&rt, &id, Some(Phase::Stopped)).await;

    spec.desired = DesiredState::Deleted;
    rt.apply(spec);
    wait(&rt, &id, None).await;
    assert!(!d.join("env").join(&id).exists());

    rt.shutdown(Duration::from_secs(10)).await.unwrap();
    g.abort();
    let _ = std::fs::remove_dir_all(d);
}
