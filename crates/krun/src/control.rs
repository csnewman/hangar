//! The worker's requests, a line each on standard input, each answered on
//! descriptor 3:
//!
//!     suspend DIR   snapshot the machine into DIR; "ok", or "error: ..." with
//!                   the machine running on
//!     memory        "HAVE REQUESTED": MiB of the virtio-mem region the guest
//!                   has plugged in, and was asked to have
//!     memory MIB    ask the guest to have MIB MiB of the region; "ok"
//!     exit          stop, once the worker has recorded the snapshot
//!
//! Standard input closing is the worker gone, and the machine goes with it.

use std::fs::File;
use std::io::{BufRead, Write};
use std::os::fd::FromRawFd;
use std::sync::OnceLock;

use krun::{MemDeviceHandle, VmmHandle};

/// What the requests act on, set as the machine is built.
#[derive(Default)]
pub struct Control {
    pub vmm: OnceLock<VmmHandle>,
    pub memory: OnceLock<MemDeviceHandle>,
}

/// Whether standard input is a pipe, as the worker gives it.
pub fn stdin_is_pipe() -> bool {
    // SAFETY: fstat fills st for an open descriptor.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    unsafe { libc::fstat(0, &mut st) == 0 && (st.st_mode & libc::S_IFMT) == libc::S_IFIFO }
}

/// Serves the requests until standard input closes, then exits.
pub fn serve(ctl: &Control) {
    // SAFETY: the worker opens descriptor 3 for the replies, and nothing
    // else here uses it.
    let mut reply = unsafe { File::from_raw_fd(3) };
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let answer = answer(ctl, &line);
        let _ = writeln!(reply, "{answer}");
        if line == "exit" {
            std::process::exit(0);
        }
    }
    eprintln!("hangar-krun: the worker has gone; stopping the machine");
    std::process::exit(1);
}

fn answer(ctl: &Control, line: &str) -> String {
    let (verb, arg) = line.split_once(' ').unwrap_or((line, ""));
    match verb {
        "suspend" => {
            let Some(h) = ctl.vmm.get() else {
                return "error: the machine has not started".into();
            };
            match h.snapshot(arg) {
                Ok(()) => "ok".into(),
                // A snapshot that failed leaves the machine paused.
                Err(e) => match h.resume() {
                    Ok(()) => format!("error: taking the snapshot: {e}"),
                    Err(r) => format!("error: taking the snapshot: {e}; and resuming: {r}"),
                },
            }
        }
        "memory" => {
            let Some(m) = ctl.memory.get() else {
                return "error: the machine's memory cannot be resized while it runs".into();
            };
            if arg.is_empty() {
                return format!("{} {}", m.plugged_mib(), m.requested_mib());
            }
            match arg.parse::<u32>() {
                Ok(mib) => match m.set_requested_mib(mib) {
                    Ok(()) => "ok".into(),
                    Err(e) => format!("error: plugging memory in: {e}"),
                },
                Err(e) => format!("error: {arg:?}: {e}"),
            }
        }
        "exit" => "ok".into(),
        _ => format!("error: unknown request {verb:?}"),
    }
}
