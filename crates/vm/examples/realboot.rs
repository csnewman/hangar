//! Boots one environment for real through the Runtime -- the real
//! hangar-krun, kernel, agent and image -- and checks it from inside the
//! guest at each stage: booted and set up, its network out and none to the
//! host's loopback, its shared files over NFS, its idle memory reclaim
//! switched as it runs, its memory resized, suspended and resumed, stopped
//! and deleted.
//!
//! Run from the repository's root, on a Mac with dev/krun-build.sh's
//! hangar-krun and dev/kernel-dev.sh's 16 KiB kernel:
//!
//!     cargo run --example realboot -- DIR
//!
//! DIR holds hangar-agent-guest, gvproxy, an images/ store with a copy of
//! the image, and is where the environment and its shared files are kept.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use hangar_api::{DesiredState, EnvironmentSpec, Phase, Resources};
use hangar_vm::runtime::{Config, Runtime};

const ID: &str = "rb1";
const IMAGE: &str = "ghcr.io/csnewman/hangar/base:ubuntu-26.04";

static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

fn t() -> f64 {
    START.get().map_or(0.0, |s| s.elapsed().as_secs_f64())
}

struct Stderr;

impl log::Log for Stderr {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::Level::Info
    }
    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) {
            eprintln!("{:7.1}s   {} {}", t(), r.level(), r.args());
        }
    }
    fn flush(&self) {}
}

struct Files;

impl hangar_nfs::View for Files {
    fn sets(&self) -> Vec<String> {
        vec!["s1".into()]
    }
    fn hidden(&self, _: &str, path: &[u8]) -> bool {
        path == b"secret.txt"
    }
    fn squash(&self, _: &str) -> hangar_nfs::Squash {
        hangar_nfs::Squash::All
    }
}

/// What failed, by step.
#[derive(Default)]
struct Report(Vec<String>);

impl Report {
    fn check(&mut self, step: &str, ok: bool, detail: &str) {
        let mark = if ok { "ok  " } else { "FAIL" };
        println!("{:7.1}s {mark} {step}: {}", t(), detail.trim());
        if !ok {
            self.0.push(step.to_string());
        }
    }
}

async fn wait(rt: &Arc<Runtime>, want: Option<Phase>, limit: Duration) -> Result<(), String> {
    let deadline = Instant::now() + limit;
    let mut last = String::new();
    loop {
        let o = rt.observe().into_iter().find(|o| o.id == ID);
        let got = o.as_ref().map(|o| o.phase);
        let now = match &o {
            Some(o) => format!(
                "{:?} {} {}",
                o.phase,
                o.reason,
                o.progress
                    .as_ref()
                    .map(|p| format!("{:?} {}/{}", p.step, p.done, p.total))
                    .unwrap_or_default()
            ),
            None => "gone".into(),
        };
        if now != last {
            println!("{:7.1}s      {now}", t());
            last = now;
        }
        if got == want {
            return Ok(());
        }
        if got == Some(Phase::Failed) && want != Some(Phase::Failed) {
            return Err(last);
        }
        if Instant::now() > deadline {
            return Err(format!("still {last} after {limit:?}"));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn sh(rt: &Arc<Runtime>, script: &str) -> (i32, String) {
    let Some(s) = rt.session(ID) else {
        return (-1, "no session".into());
    };
    match s.exec(Duration::from_secs(60), &["sh", "-c", script]).await {
        Ok(r) => (r.code, format!("{}{}", r.stdout, r.stderr)),
        Err(e) => (-1, e.to_string()),
    }
}

async fn until(rt: &Arc<Runtime>, script: &str, limit: Duration) -> (bool, String) {
    let deadline = Instant::now() + limit;
    loop {
        let (code, out) = sh(rt, script).await;
        if code == 0 {
            return (true, out);
        }
        if Instant::now() > deadline {
            return (false, out);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

#[tokio::main]
async fn main() {
    START.set(Instant::now()).unwrap();
    log::set_logger(&Stderr).unwrap();
    log::set_max_level(log::LevelFilter::Info);
    let dir = PathBuf::from(std::env::args().nth(1).expect("the boot directory"));
    let dir = std::path::absolute(dir).unwrap();
    let repo = std::env::current_dir().unwrap();
    let mut report = Report::default();

    // The environment is pinned to the copy held, so nothing is looked up
    // or fetched to start it.
    let env_dir = dir.join("env").join(ID);
    let _ = std::fs::remove_dir_all(&env_dir);
    std::fs::create_dir_all(&env_dir).unwrap();
    let digest = std::fs::read_dir(dir.join("images"))
        .unwrap()
        .flatten()
        .find_map(|e| std::fs::read_to_string(e.path().join("digest")).ok())
        .expect("a copy of the image in images/");
    std::fs::write(
        env_dir.join("image.json"),
        serde_json::json!({"ref": IMAGE, "digest": digest.trim()}).to_string(),
    )
    .unwrap();
    let files = dir.join("files");
    std::fs::create_dir_all(files.join("s1")).unwrap();
    std::fs::write(files.join("s1/hello.txt"), "hello from the host\n").unwrap();
    std::fs::write(files.join("s1/secret.txt"), "hidden\n").unwrap();
    let _ = std::fs::remove_file(files.join("s1/guest.txt"));

    let rt = Runtime::new(Config {
        monitor: "krun".into(),
        state_dir: dir.join("env"),
        images_dir: dir.join("images"),
        kernel: repo.join("out/kernel-dev-16k/vmlinuz"),
        agent: dir.join("hangar-agent-guest"),
        // HANGAR_KRUN names another hangar-krun to run it under.
        monitor_bin: Some(
            std::env::var_os("HANGAR_KRUN")
                .map(PathBuf::from)
                .unwrap_or_else(|| repo.join("out/krun/bin/hangar-krun")),
        ),
        mkfs: Some(repo.join("out/krun/bin/mkfs.ext4")),
        gvproxy: Some(dir.join("gvproxy")),
        upper_gib: 8,
        docker_gib: 8,
        ..Default::default()
    })
    .await
    .expect("the runtime");
    rt.use_capacity(Resources {
        cpus: 6,
        memory_mib: 8192,
    });
    rt.use_shared_files(
        files.clone(),
        Arc::new(|_: &str| Arc::new(Files) as Arc<dyn hangar_nfs::View>),
    );
    println!("{:7.1}s      runtime up, monitor {}", t(), rt.monitor());

    let mut spec: EnvironmentSpec = serde_json::from_value(serde_json::json!({
        "id": ID, "name": "rustboot", "desired": "running",
        "spec": {"image": IMAGE, "cpus": 2, "memory_mib": 2048, "display": "none", "gpu": "none",
                 "swap_mib": 1024}
    }))
    .unwrap();
    rt.apply(spec.clone());
    let booted = wait(&rt, Some(Phase::Running), Duration::from_secs(300)).await;
    report.check(
        "boot",
        booted.is_ok(),
        &booted
            .err()
            .unwrap_or_else(|| format!("running after {:.1}s", t())),
    );

    if report.0.is_empty() {
        steps(&rt, &mut spec, &files, &mut report).await;
    }

    // Whatever happened, the environment goes.
    spec.desired = DesiredState::Deleted;
    rt.apply(spec);
    let gone = wait(&rt, None, Duration::from_secs(180)).await;
    report.check(
        "delete",
        gone.is_ok() && !env_dir.exists() && !Path::new(&format!("/tmp/hangar-{ID}")).exists(),
        &gone
            .err()
            .unwrap_or_else(|| "its directory and sockets are gone".into()),
    );
    let _ = rt.shutdown(Duration::from_secs(30)).await;
    if report.0.is_empty() {
        println!("{:7.1}s      all passed", t());
    } else {
        println!("{:7.1}s      failed: {}", t(), report.0.join(", "));
        std::process::exit(1);
    }
}

async fn steps(rt: &Arc<Runtime>, spec: &mut EnvironmentSpec, files: &Path, report: &mut Report) {
    let (code, out) = sh(
        rt,
        "uname -r; hostname; nproc; free -m | sed -n 2p; swapon --show=NAME,SIZE --noheadings; \
         findmnt -no FSTYPE /; cat /var/lib/hangar-workspace/projects.code-workspace 2>/dev/null | head -3",
    )
    .await;
    report.check(
        "guest",
        code == 0 && out.contains("rustboot") && out.contains("hangarfs"),
        &out.replace('\n', " | "),
    );

    let (code, out) = sh(
        rt,
        "getent hosts github.com | head -1 && curl -sS -o /dev/null -w '%{http_code}' --max-time 15 https://github.com",
    )
    .await;
    report.check("network out", code == 0, &out.replace('\n', " | "));

    let (code, out) = sh(
        rt,
        "curl -sS -o /dev/null -w '%{http_code}' --max-time 5 http://192.168.127.254:8080/; echo \" exit=$?\"",
    )
    .await;
    report.check(
        "no route to the host's loopback",
        code == 0 && !out.contains("exit=0"),
        &out,
    );

    let (_, out) = sh(
        rt,
        "cat /run/hangar/memory-reclaim; echo; cat /sys/module/damon_reclaim/parameters/enabled",
    )
    .await;
    report.check(
        "reclaim off from boot",
        out.split_whitespace().collect::<Vec<_>>() == ["0", "N"],
        &out.replace('\n', " "),
    );

    let (code, out) = sh(
        rt,
        "mkdir -p /mnt/f && mount -t nfs4 -o vers=4.2,proto=vsock,addr=vsock:2,port=2049,hard,actimeo=1 hangar:/ /mnt/f \
         && ls /mnt/f /mnt/f/s1 && cat /mnt/f/s1/hello.txt && echo from the guest > /mnt/f/s1/guest.txt && sync \
         && flock /mnt/f/s1/lockfile true && echo locked && umount /mnt/f",
    )
    .await;
    let written = std::fs::read_to_string(files.join("s1/guest.txt")).unwrap_or_default();
    report.check(
        "shared files over NFS",
        code == 0
            && out.contains("hello from the host")
            && !out.contains("secret.txt")
            && out.contains("locked")
            && written == "from the guest\n",
        &format!("{} | host reads {:?}", out.replace('\n', " | "), written),
    );

    spec.spec.memory_reclaim = true;
    rt.apply(spec.clone());
    let (ok, out) = until(
        rt,
        "[ \"$(cat /run/hangar/memory-reclaim)\" = 1 ] && grep -qx Y /sys/module/damon_reclaim/parameters/enabled && echo on",
        Duration::from_secs(30),
    )
    .await;
    report.check("reclaim switched on as it runs", ok, &out);
    spec.spec.memory_reclaim = false;
    rt.apply(spec.clone());
    let (ok, out) = until(
        rt,
        "[ \"$(cat /run/hangar/memory-reclaim)\" = 0 ] && grep -qx N /sys/module/damon_reclaim/parameters/enabled && echo off",
        Duration::from_secs(30),
    )
    .await;
    report.check("reclaim switched off again", ok, &out);

    spec.spec.memory_mib = 3072;
    rt.apply(spec.clone());
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut has = 0;
    while Instant::now() < deadline {
        has = rt
            .observe()
            .into_iter()
            .find(|o| o.id == ID)
            .and_then(|o| o.stats)
            .map_or(0, |s| s.machine_memory_mib);
        if has >= 3000 {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let (_, free) = sh(rt, "free -m | sed -n 2p").await;
    report.check(
        "memory resized to 3 GB",
        has >= 3000,
        &format!("the monitor says {has} MiB | {}", free.trim()),
    );

    let (_, before) = sh(
        rt,
        "echo marker > /run/rb-marker; cut -d' ' -f1 /proc/uptime",
    )
    .await;
    spec.desired = DesiredState::Suspended;
    rt.apply(spec.clone());
    let s = wait(rt, Some(Phase::Suspended), Duration::from_secs(300)).await;
    report.check("suspend", s.is_ok(), &s.err().unwrap_or_default());
    spec.desired = DesiredState::Running;
    rt.apply(spec.clone());
    let r = wait(rt, Some(Phase::Running), Duration::from_secs(300)).await;
    let (_, after) = sh(rt, "cat /run/rb-marker; cut -d' ' -f1 /proc/uptime").await;
    let up = |s: &str| {
        s.split_whitespace()
            .last()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0)
    };
    report.check(
        "resume where it was",
        r.is_ok() && after.contains("marker") && up(&after) >= up(&before),
        &format!(
            "{} | uptime {} then {}",
            r.err().unwrap_or_default(),
            before.trim(),
            after.replace('\n', " ")
        ),
    );
    let (code, out) = sh(
        rt,
        "mount -t nfs4 -o vers=4.2,proto=vsock,addr=vsock:2,port=2049,hard hangar:/ /mnt/f && cat /mnt/f/s1/guest.txt && umount /mnt/f",
    )
    .await;
    report.check(
        "shared files after resuming",
        code == 0,
        &out.replace('\n', " | "),
    );

    spec.desired = DesiredState::Stopped;
    rt.apply(spec.clone());
    let s = wait(rt, Some(Phase::Stopped), Duration::from_secs(180)).await;
    report.check(
        "stop",
        s.is_ok(),
        &s.err().unwrap_or_else(|| "powered off".into()),
    );
}
