//! hangar-agent: the guest agent. The worker supplies it in every
//! environment's initramfs, and the image's hangar-agent.service runs it.
//!
//! As PID 1, in the initramfs, it assembles the root and hands over to the
//! image's init (`init`). With no arguments, as the service, it is the
//! agent: it dials the host's control channel (`control`) and serves, as
//! root, the rest on their own vsock ports (`serve`) -- terminals, the
//! desktop, the Code tab, SSH, the profile session, processes, the guest's
//! ports -- and hands the guest's idle memory back to the host.
//!
//!   hangar-agent code --socket <path> --root <dir> [--trust <file>]
//!
//! serves the Code tab, as the environment's user, on a unix socket the
//! agent joins the browser to. `--trust` names the worker's list of trusted
//! folders, the only ones whose code it runs; without it, every folder is
//! trusted.
//!
//!   hangar-agent hook <agent>
//!   hangar-agent mcp
//!
//! are a coding agent's hook and the MCP server Claude Code runs, both as
//! whoever runs the coding agent, reaching the agent through its socket
//! (`agents`).
//!
//!   hangar-agent sftp-server
//!
//! serves SFTP on standard input and output, as whoever runs it: the SSH
//! server's sftp subsystem where the image has no OpenSSH one (`sftp`).
//!
//! Run as `docker-credential-hangar`, it is Docker's credential helper for
//! Hangar's registry.

mod code;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use hangar_agent::{agents, profile};

fn main() -> ExitCode {
    let mut argv = std::env::args();
    let name = argv.next().unwrap_or_default();
    let args: Vec<String> = argv.collect();
    if Path::new(&name).file_name() == Some("docker-credential-hangar".as_ref()) {
        return profile::credential_helper(&args, Path::new(profile::REGISTRY_SOCKET));
    }
    // In the initramfs the kernel starts the agent as init.
    #[cfg(target_os = "linux")]
    if std::process::id() == 1 {
        hangar_agent::init::run();
    }
    match args.first().map(String::as_str) {
        None => {
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            match rt.block_on(agent()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("hangar-agent: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("hook") if args.len() == 2 => {
            // SAFETY: getppid cannot fail.
            let parent = unsafe { libc::getppid() };
            agents::run_hook(
                &args[1],
                std::io::stdin(),
                std::io::stdout(),
                Path::new(agents::SOCKET),
                |k| std::env::var(k).ok(),
                parent,
            );
            ExitCode::SUCCESS
        }
        Some("sftp-server") if args.len() == 1 => {
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            match rt.block_on(hangar_agent::sftp::serve()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("hangar-agent: sftp: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("mcp") if args.len() == 1 => {
            let socket = Path::new(agents::SOCKET);
            match agents::serve_mcp(std::io::stdin().lock(), std::io::stdout(), socket) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("hangar-agent: mcp: {e}");
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
        "usage: hangar-agent\n       hangar-agent code --socket <path> --root <dir> [--trust <file>]\n       hangar-agent hook <agent>\n       hangar-agent mcp\n       hangar-agent sftp-server"
    );
    ExitCode::from(2)
}

/// The agent, for as long as the guest runs: the control channel to the
/// host, and every service.
async fn agent() -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    tokio::spawn(hangar_agent::control::run());
    hangar_agent::serve::serve().await
}
