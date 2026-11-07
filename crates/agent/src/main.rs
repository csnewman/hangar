//! hangar-agent-rs: the guest agent's Rust half, run beside the Go agent
//! while the agent moves to Rust a service at a time.
//!
//!   hangar-agent-rs code --socket <path> --root <dir>
//!
//! serves the Code tab, as the environment's user, on a unix socket the
//! agent joins the browser to.

mod code;

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("code") => {
            let (mut socket, mut root) = (None, None);
            let mut rest = args[1..].iter();
            while let Some(a) = rest.next() {
                match a.as_str() {
                    "--socket" => socket = rest.next().map(PathBuf::from),
                    "--root" => root = rest.next().map(PathBuf::from),
                    _ => return usage(),
                }
            }
            let (Some(socket), Some(root)) = (socket, root) else {
                return usage();
            };
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            match rt.block_on(code::serve(socket, root)) {
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
    eprintln!("usage: hangar-agent-rs code --socket <path> --root <dir>");
    ExitCode::from(2)
}
