//! hangar-krun runs one machine under libkrun, from the JSON description the
//! worker writes (hangar_api::monitor::Config). libkrun takes the process
//! over, and it exits when the guest does.
//!
//!     hangar-krun CONFIG.json   run the machine
//!     hangar-krun -features     what this build has (monitor::Features)
//!     hangar-krun -probe        on Linux, what the host renders with
//!
//! libkrun is Hangar's fork, linked as a crate (crates/krun/libkrun is its
//! checkout, made by dev/krun-build.sh). The GPU is built in with the
//! `gpu` feature, when virglrenderer is there to link.

mod control;
#[cfg(feature = "gpu")]
mod display;
mod machine;
#[cfg(target_os = "linux")]
mod probe;
#[cfg(all(target_os = "macos", feature = "gpu"))]
mod vulkan;

use std::process::ExitCode;
use std::sync::Arc;

use hangar_api::monitor::{Config, Features};

const USAGE: &str = "usage: hangar-krun CONFIG.json | -features | -probe";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Chosen before any thread starts: it is told to the Vulkan loader
    // through the environment.
    let vulkan = choose_vulkan();
    match args.as_slice() {
        [a] if a == "-features" => {
            let f = Features {
                gpu: cfg!(feature = "gpu"),
                zink: can_zink(),
                vulkan,
            };
            println!("{}", serde_json::to_string(&f).unwrap_or_default());
            ExitCode::SUCCESS
        }
        #[cfg(target_os = "linux")]
        [a] if a == "-probe" => {
            println!(
                "{}",
                serde_json::to_string(&probe::probe()).unwrap_or_default()
            );
            ExitCode::SUCCESS
        }
        [path] if !path.starts_with('-') => match run(path) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("hangar-krun: {e}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn run(path: &str) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let c: Config = serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
    if c.gpu.is_some() {
        render_server_env();
    }
    let ctl = Arc::new(control::Control::default());
    // The worker holds the other end of standard input: when the worker
    // goes, however it goes, so does the machine, rather than running on
    // with nothing to supervise it. macOS has no parent-death signal.
    if control::stdin_is_pipe() {
        let ctl = ctl.clone();
        std::thread::spawn(move || control::serve(&ctl));
    }
    machine::start(&c, &ctl)
}

#[cfg(all(target_os = "macos", feature = "gpu"))]
fn choose_vulkan() -> String {
    vulkan::choose()
}

#[cfg(not(all(target_os = "macos", feature = "gpu")))]
fn choose_vulkan() -> String {
    String::new()
}

#[cfg(all(target_os = "macos", feature = "gpu"))]
fn can_zink() -> bool {
    vulkan::can_zink()
}

/// Only the Mac draws a guest's OpenGL with zink: on Linux virglrenderer
/// draws it with the host's own OpenGL.
#[cfg(not(all(target_os = "macos", feature = "gpu")))]
fn can_zink() -> bool {
    false
}

/// Points virglrenderer at the render server bundled beside hangar-krun,
/// and on the Mac the render server at the Vulkan loader bundled there,
/// which it opens by name. A render server named already is left alone.
fn render_server_env() {
    if std::env::var_os("RENDER_SERVER_EXEC_PATH").is_some() {
        return;
    }
    let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|p| p.to_path_buf()))
    else {
        return;
    };
    let server = dir.join("virgl_render_server");
    if !server.is_file() {
        return;
    }
    // SAFETY: no other thread has started yet.
    unsafe {
        std::env::set_var("RENDER_SERVER_EXEC_PATH", &server);
        if cfg!(target_os = "macos") {
            let rest = std::env::var("DYLD_FALLBACK_LIBRARY_PATH").unwrap_or_default();
            std::env::set_var(
                "DYLD_FALLBACK_LIBRARY_PATH",
                format!("{}:{rest}", dir.display()),
            );
        }
    }
}
