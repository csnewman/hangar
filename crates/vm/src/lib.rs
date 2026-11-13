//! An environment's machine: the host it runs on, the monitor that runs it
//! (Cloud Hypervisor or libkrun), and what it boots from -- its image, its
//! writable disk, its swap and the initramfs that is the agent.

pub mod agent;
pub mod ch;
pub mod files;
pub mod gitout;
pub mod guest;
pub mod gvproxy;
pub mod host;
pub mod hugepages;
mod initrd;
pub mod instance;
pub mod krun;
pub mod log;
pub mod provision;
pub mod runtime;
pub mod stats;
pub mod suspend;
mod swap;
pub mod vsock;

pub use initrd::write_initrd;
pub use swap::{drop_swap, make_swap};

/// An error that is a message.
pub(crate) fn err(msg: impl Into<String>) -> std::io::Error {
    std::io::Error::other(msg.into())
}

/// A path as the C library takes it.
#[cfg(target_os = "macos")]
pub(crate) fn fs_cstr(p: &std::path::Path) -> std::io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(p.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))
}
