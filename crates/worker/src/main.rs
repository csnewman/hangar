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

const USAGE: &str = "usage: hangar-worker [-config FILE] [-debug]";

fn main() -> ExitCode {
    raise_file_limit();
    let mut config = PathBuf::from("/etc/hangar/worker.yaml");
    let mut debug = false;
    let mut args = std::env::args().skip(1);
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
        "cloud-hypervisor" | "krun" => {}
        other => {
            return Err(format!(
                "unknown runtime {other:?}: want cloud-hypervisor or krun"
            ));
        }
    }
    let machines = hangar_vm::runtime::Runtime::new(
        vm::runtime_config(&cfg, log.clone()).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;
    let w =
        Worker::new(cfg, machines.clone(), log.clone(), Some(rings)).map_err(|e| e.to_string())?;

    let out = tokio::select! {
        r = w.run() => r.map_err(|e| e.to_string()),
        _ = stopped() => Ok(()),
    };
    log.info("stopping environments", &[]);
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
