//! hangar-worker runs the environments the control plane places on this
//! machine. It dials out to hangar-server and needs no inbound route.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use hangar_vm::log::Log;
use hangar_worker::config::Config;
use hangar_worker::logs::{Logger, Rings};
use hangar_worker::{Worker, vm};

mod tools;

const USAGE: &str = "usage: hangar-worker [-config FILE] [-debug]
       hangar-worker doctor                  check this machine can run environments
       hangar-worker erofs -o IMAGE DIR      write a directory as an EROFS image
       hangar-worker erofs -o IMAGE -tar     write a tar archive on standard input as one
       hangar-worker proxmox-cmdline         print the kernel command line Proxmox VMs boot with";

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match argv.first().map(String::as_str) {
        Some("doctor") => return tools::doctor(),
        Some("erofs") => return tools::erofs(&argv[1..]),
        Some("proxmox-cmdline") => {
            println!("{}", hangar_vm::proxmox::CMDLINE);
            return ExitCode::SUCCESS;
        }
        _ => {}
    }
    raise_file_limit();
    let mut config = PathBuf::from("/etc/hangar/worker.yaml");
    let mut debug = false;
    let mut args = argv.into_iter();
    while let Some(a) = args.next() {
        match a.trim_start_matches('-') {
            "config" => match args.next() {
                Some(p) => config = p.into(),
                None => return usage(),
            },
            s if s.starts_with("config=") => config = s["config=".len()..].into(),
            "debug" => debug = true,
            "h" | "help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            _ => return usage(),
        }
    }

    // What the worker logs is kept in memory too, for the control plane to
    // show: whole, and each environment's lines.
    let rings = Arc::new(Rings::new());
    let level = if debug {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    };
    Logger::install(rings.clone(), level);

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("hangar-worker: {e}");
            return ExitCode::FAILURE;
        }
    };
    let out = rt.block_on(run(config, rings));
    // A request the server is holding has nothing left to deliver to.
    rt.shutdown_timeout(Duration::from_secs(1));
    match out {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("hangar-worker: {e}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

async fn run(path: PathBuf, rings: Arc<Rings>) -> Result<(), String> {
    let cfg = Config::load(&path).map_err(|e| e.to_string())?;
    let log = Log::default();
    match cfg.runtime.as_str() {
        "cloud-hypervisor" | "krun" | "proxmox" => {}
        other => {
            return Err(format!(
                "unknown runtime {other:?}: want cloud-hypervisor, krun or proxmox"
            ));
        }
    }
    let mut rtcfg = vm::runtime_config(&cfg, log.clone()).map_err(|e| e.to_string())?;
    if !cfg.hub.listen.is_empty() {
        if cfg.hub.address.is_empty() {
            return Err("hub.listen is set without hub.address, what guests dial".into());
        }
        // The hub's certificate is kept beside the worker's credential: its
        // fingerprint is in every guest booted since, so it outlives
        // caches.
        let dir = std::path::Path::new(&cfg.server.credential_file)
            .parent()
            .map(|d| d.join("hub"))
            .unwrap_or_else(|| PathBuf::from("hub"));
        let hub = hangar_vm::hub::Hub::start(&cfg.hub.listen, &cfg.hub.address, &dir, log.clone())
            .await
            .map_err(|e| e.to_string())?;
        rtcfg.hub = Some(hub);
    }
    if cfg.runtime == "proxmox" {
        let p = &cfg.proxmox;
        let token = std::fs::read_to_string(&p.token_file)
            .map_err(|e| format!("proxmox.token_file {}: {e}", p.token_file))?;
        let dir = PathBuf::from(&cfg.storage.caches).join("proxmox");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let pve = hangar_vm::proxmox::Pve::new(
            hangar_vm::proxmox::Config {
                url: p.url.clone(),
                token: token.trim().to_string(),
                fingerprint: p.fingerprint.clone(),
                node: p.node.clone(),
                storage: p.storage.clone(),
                iso_storage: p.iso_storage.clone(),
                bridge: if p.bridge.is_empty() {
                    "vmbr0".into()
                } else {
                    p.bridge.clone()
                },
                pool: p.pool.clone(),
                prefix: p.prefix.clone(),
                mke2fs: if p.mke2fs.is_empty() {
                    "/usr/share/hangar/mke2fs".into()
                } else {
                    p.mke2fs.clone().into()
                },
                release_iso: p.release_iso.clone(),
                worker_vm: p.worker_vm,
            },
            &dir,
            log.clone(),
        )
        .map_err(|e| e.to_string())?;
        rtcfg.proxmox = Some(Arc::new(pve));
    }
    // A Proxmox VM runs on without its worker, which takes it back when it
    // starts again.
    let leaves_running = cfg.runtime == "proxmox";
    let machines = hangar_vm::runtime::Runtime::new(rtcfg)
        .await
        .map_err(|e| e.to_string())?;
    let w =
        Worker::new(cfg, machines.clone(), log.clone(), Some(rings)).map_err(|e| e.to_string())?;

    let out = tokio::select! {
        r = w.run() => r.map_err(|e| e.to_string()),
        _ = stopped() => Ok(()),
    };
    log.info(
        if leaves_running {
            "leaving environments' VMs running"
        } else {
            "stopping environments"
        },
        &[],
    );
    if let Err(e) = machines.shutdown(Duration::from_secs(120)).await {
        log.warn("environments did not all stop", &[("err", &e)]);
    }
    out
}

/// Waits for SIGINT or SIGTERM.
async fn stopped() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let (Ok(mut int), Ok(mut term)) = (
            signal(SignalKind::interrupt()),
            signal(SignalKind::terminate()),
        ) else {
            return std::future::pending().await;
        };
        tokio::select! {
            _ = int.recv() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Raises the limit on open files to the most allowed, for the worker and
/// what it starts: Cloud Hypervisor holds several descriptors for every
/// queue of every device, and a large machine passes the 1024 a container
/// often starts with. macOS refuses an unlimited soft limit, so the most it
/// takes is tried there in steps.
fn raise_file_limit() {
    #[cfg(unix)]
    unsafe {
        let mut lim: libc::rlimit = std::mem::zeroed();
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) != 0 {
            return;
        }
        for want in [lim.rlim_max, 1 << 20, 1 << 16, 10240] {
            let cur = want.min(lim.rlim_max);
            if cur <= lim.rlim_cur {
                return;
            }
            let l = libc::rlimit {
                rlim_cur: cur,
                rlim_max: lim.rlim_max,
            };
            if libc::setrlimit(libc::RLIMIT_NOFILE, &l) == 0 {
                return;
            }
        }
    }
}
