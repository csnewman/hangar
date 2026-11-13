//! gvproxy is tethered to the process that starts it: it stops when its
//! network is closed, and when it is dropped without closing, as when the
//! process that holds it dies.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use hangar_vm::gvproxy::Network;

/// Makes the socket it is told to listen on, then waits to be stopped.
const FAKE: &str = "#!/bin/sh\n\
while [ $# -gt 0 ]; do\n\
  case $1 in -listen-vfkit) touch \"${2#unixgram://}\"; shift ;; esac\n\
  shift\n\
done\n\
while :; do sleep 1; done\n";

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hangar-gvproxy-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn fake(dir: &Path) -> PathBuf {
    let bin = dir.join("gvproxy");
    std::fs::write(&bin, FAKE).unwrap();
    std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    bin
}

fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 checks for the process and sends nothing.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

async fn gone(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while alive(pid) {
        assert!(Instant::now() < deadline, "gvproxy {pid} is still running");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn closed() {
    let d = scratch("closed");
    let net = Network::start(&fake(&d), &d.join("net.sock"), &d.join("net.log"))
        .await
        .unwrap();
    let pid = net.pid();
    assert!(alive(pid));
    // The ID is the program's own, not a wrapper's.
    let out = std::process::Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("gvproxy -config"));
    net.close().await;
    gone(pid).await;
    assert!(!d.join("net.sock").exists());
    let _ = std::fs::remove_dir_all(d);
}

#[tokio::test]
async fn dropped() {
    let d = scratch("dropped");
    let net = Network::start(&fake(&d), &d.join("net.sock"), &d.join("net.log"))
        .await
        .unwrap();
    let pid = net.pid();
    drop(net);
    gone(pid).await;
    let _ = std::fs::remove_dir_all(d);
}
