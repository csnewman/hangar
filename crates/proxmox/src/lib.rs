//! Proxmox VE: its API, over HTTPS with an API token (`client`), and a
//! Hangar of one's own made on a Proxmox as a VM (`hangar`), as the app
//! makes one for a person: the release ISO uploaded, the VM made, and its
//! settings given through the guest agent channel.

pub mod client;
pub mod hangar;

use std::io;

fn err(msg: impl Into<String>) -> io::Error {
    io::Error::other(msg.into())
}
