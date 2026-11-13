//! What the machine Hangar runs on can do.
//!
//! The guest architecture follows the host's: an arm64 node runs aarch64
//! guests and an x86-64 node runs x86-64 guests. The monitor and the
//! serial console the guest sees follow from the host too.

use crate::err;
use std::fmt;
use std::io;

/// The monitors environments run under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Monitor {
    /// Runs guests on Linux's KVM.
    CloudHypervisor,
    /// Runs guests through libkrun (hangar-krun): on macOS's
    /// Hypervisor.framework, and on Linux's KVM beside Cloud Hypervisor.
    Krun,
}

impl Monitor {
    /// The name a worker's config and the API give it.
    pub fn name(self) -> &'static str {
        match self {
            Monitor::CloudHypervisor => "cloud-hypervisor",
            Monitor::Krun => "krun",
        }
    }

    pub fn parse(s: &str) -> Option<Monitor> {
        match s {
            "cloud-hypervisor" => Some(Monitor::CloudHypervisor),
            "krun" => Some(Monitor::Krun),
            _ => None,
        }
    }
}

impl fmt::Display for Monitor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What this host can do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caps {
    /// The host's system: "linux" or "darwin".
    pub os: &'static str,
    /// The guest architecture: "aarch64" or "x86_64".
    pub arch: &'static str,
    /// The serial console's device as the guest sees it.
    pub console_tty: &'static str,
    pub monitor: Monitor,
}

/// The host's capabilities under monitor, or under its own when monitor
/// is empty: Cloud Hypervisor on Linux, libkrun on macOS. An error means
/// Hangar cannot run machines here under it.
pub fn detect(monitor: &str) -> io::Result<Caps> {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        other => return Err(err(format!("unsupported host architecture {other:?}"))),
    };
    let named = match monitor {
        "" => None,
        m => Some(Monitor::parse(m).ok_or_else(|| err(format!("unknown monitor {m:?}")))?),
    };
    match std::env::consts::OS {
        "linux" => {
            let (monitor, console_tty) = match named {
                None | Some(Monitor::CloudHypervisor) => (
                    Monitor::CloudHypervisor,
                    if arch == "x86_64" { "ttyS0" } else { "ttyAMA0" },
                ),
                // libkrun's console is a virtio one.
                Some(Monitor::Krun) => (Monitor::Krun, "hvc0"),
            };
            // Neither monitor has an emulation mode: without KVM there is
            // no way to run a guest at all, fast or slow. So this is a
            // refusal rather than a warning about performance.
            if let Err(e) = std::fs::metadata("/dev/kvm") {
                return Err(err(format!(
                    "no /dev/kvm: {monitor} runs guests on KVM only, and there is no emulation fallback ({e})"
                )));
            }
            Ok(Caps {
                os: "linux",
                arch,
                console_tty,
                monitor,
            })
        }
        "macos" => {
            // libkrun runs guests on Apple silicon alone, its console a
            // virtio one.
            if arch != "aarch64" {
                return Err(err("environments run on Apple silicon Macs, not amd64"));
            }
            if let Some(m) = named
                && m != Monitor::Krun
            {
                return Err(err(format!("a Mac runs environments under krun, not {m}")));
            }
            if !hypervisor_framework() {
                return Err(err(
                    "this Mac does not offer Hypervisor.framework (kern.hv_support)",
                ));
            }
            Ok(Caps {
                os: "darwin",
                arch,
                console_tty: "hvc0",
                monitor: Monitor::Krun,
            })
        }
        other => Err(err(format!(
            "Hangar runs environments on Linux and macOS, not {other}"
        ))),
    }
}

impl Caps {
    /// The capabilities as `hangar doctor` shows them.
    pub fn summary(&self) -> String {
        let goarch = if self.arch == "aarch64" {
            "arm64"
        } else {
            "amd64"
        };
        format!(
            "host        {}/{goarch}\nguest arch  {}\nconsole     {}\nmonitor     {}\n",
            self.os, self.arch, self.console_tty, self.monitor
        )
    }
}

/// Whether macOS offers its hypervisor here.
#[cfg(target_os = "macos")]
fn hypervisor_framework() -> bool {
    let mut v: u32 = 0;
    let mut n = std::mem::size_of::<u32>();
    // SAFETY: the name is NUL-terminated and v holds n bytes.
    let r = unsafe {
        libc::sysctlbyname(
            c"kern.hv_support".as_ptr(),
            (&mut v as *mut u32).cast(),
            &mut n,
            std::ptr::null_mut(),
            0,
        )
    };
    r == 0 && v == 1
}

/// macOS's alone.
#[cfg(not(target_os = "macos"))]
fn hypervisor_framework() -> bool {
    false
}
