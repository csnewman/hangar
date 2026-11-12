//! The guest agent: the initramfs's init (`init`), the control channel to
//! the host (`control`), and the services the host reaches on vsock
//! (`serve`).

pub mod agents;
pub mod clock;
pub mod codeservice;
pub mod control;
pub mod desktop;
pub mod fdio;
pub mod guestport;
pub mod init;
pub mod launch;
pub mod memory;
pub mod procs;
pub mod profile;
pub mod serve;
pub mod sftp;
pub mod sshd;
pub mod terminal;
pub mod user;
#[cfg(target_os = "linux")]
pub mod vsock;
