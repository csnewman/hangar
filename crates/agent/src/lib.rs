//! The guest agent's Rust half: what has moved over from the Go agent, which
//! starts it (`hangar-agent-rs serve`) and stays the guest's init until the
//! rest has moved too.

pub mod clock;
pub mod codeservice;
pub mod desktop;
pub mod fdio;
pub mod guestport;
pub mod launch;
pub mod procs;
pub mod serve;
pub mod terminal;
pub mod user;
#[cfg(target_os = "linux")]
pub mod vsock;
