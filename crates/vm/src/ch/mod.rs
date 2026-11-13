//! Cloud Hypervisor: its command line, its API, and the backends beside it.

mod api;
mod config;
mod launch;
mod passt;

pub use api::{Api, Memory};
pub use config::{Config, Disk, QUEUE_SIZE, boot_memory_mb, find, hotplug, plugged};
pub use launch::{Backend, launch};
pub use passt::Passt;
