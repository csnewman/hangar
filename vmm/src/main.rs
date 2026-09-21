//! A virtual machine monitor that runs one Hangar environment.
//!
//! It is invoked by `internal/vmm` with a JSON description of the machine and
//! runs until the guest stops. There is no API, no machine types and no
//! device discovery: everything the guest will ever see is in the
//! description, decided by the caller.

mod boot;
mod config;
mod devices;
mod fdt;
mod gic;
mod layout;
mod memory;
mod vcpu;
mod vmm;

use std::io::Read;
use std::process::ExitCode;

fn main() -> ExitCode {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("warn,hangar_vmm=info"),
    )
    .format_timestamp_millis()
    .init();

    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            log::error!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// Raise the open file limit to the most this process is allowed.
///
/// The filesystem keeps an `O_PATH` descriptor for every inode the guest has
/// looked up, so a base image of a few thousand files needs a few thousand
/// descriptors. The usual soft limit is 1024; past it every lookup fails with
/// EMFILE, and the guest sees a filesystem that has abruptly lost most of its
/// contents rather than an error it can report.
fn raise_file_limit() -> std::io::Result<u64> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit fills in a struct this call owns.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if limit.rlim_cur < limit.rlim_max {
        limit.rlim_cur = limit.rlim_max;
        // SAFETY: the new soft limit is the hard limit, which a process may
        // always set.
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(limit.rlim_cur)
}

fn run() -> Result<(), String> {
    let path = std::env::args().nth(1);
    let text = match path.as_deref() {
        None | Some("-") => {
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .map_err(|e| format!("reading the configuration from standard input: {e}"))?;
            s
        }
        Some(p) => std::fs::read_to_string(p)
            .map_err(|e| format!("reading the configuration from {p}: {e}"))?,
    };

    let cfg: config::Config =
        serde_json::from_str(&text).map_err(|e| format!("parsing the configuration: {e}"))?;
    cfg.validate()?;

    let files = raise_file_limit().map_err(|e| format!("raising the open file limit: {e}"))?;
    log::info!("open file limit {files}");

    let mut vm = vmm::Vm::new(&cfg).map_err(|e| format!("building the machine: {e}"))?;
    log::info!(
        "{} started: {} vCPUs, {} MiB",
        cfg.name,
        cfg.cpus,
        vm.ram_size() / (1024 * 1024)
    );
    vm.run().map_err(|e| format!("running the machine: {e}"))?;
    Ok(())
}
