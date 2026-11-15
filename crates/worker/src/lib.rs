//! hangar-worker: it holds the environments the control plane places on
//! this machine, under the runtime its configuration names.

pub mod client;
pub mod config;
pub mod files;
pub mod logs;
pub mod sys;
pub mod tunnel;
pub mod vm;
mod worker;

/// This build, as the worker reports it: HANGAR_VERSION when it was
/// compiled, as images are built, else the crate's version.
pub const VERSION: &str = match option_env!("HANGAR_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

pub use worker::{RENDERER_LABEL, REPORT_EVERY, Runtime, Worker, offer, with_renderer_label};
