//! hangar-agent-rs: the guest agent's Rust half, run beside the Go agent
//! while the agent moves to Rust a service at a time.
//!
//!   hangar-agent-rs serve
//!
//! serves, as root, what has moved over (`serve.rs`): terminals, the
//! desktop, the Code tab's service, the process list and the guest's ports,
//! and hands the guest's idle memory back to the host.
//!
//!   hangar-agent-rs code --socket <path> --root <dir> [--trust <file>]
//!
//! serves the Code tab, as the environment's user, on a unix socket the
//! agent joins the browser to. `--trust` names the worker's list of trusted
//! folders, the only ones whose code it runs; without it, every folder is
//! trusted.

mod code;

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("serve") if args.len() == 1 => {
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            match rt.block_on(hangar_agent::serve::serve()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("hangar-agent-rs: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("code") => {
            let (mut socket, mut root, mut trust) = (None, None, None);
            let mut rest = args[1..].iter();
            while let Some(a) = rest.next() {
                match a.as_str() {
                    "--socket" => socket = rest.next().map(PathBuf::from),
                    "--root" => root = rest.next().map(PathBuf::from),
                    "--trust" => trust = rest.next().map(PathBuf::from),
                    _ => return usage(),
                }
            }
            let (Some(socket), Some(root)) = (socket, root) else {
                return usage();
            };
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            match rt.block_on(code::serve(socket, root, trust)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("hangar-code: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => usage(),
    }
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: hangar-agent-rs serve\n       hangar-agent-rs code --socket <path> --root <dir> [--trust <file>]"
    );
    ExitCode::from(2)
}
