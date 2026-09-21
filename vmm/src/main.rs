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
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
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
