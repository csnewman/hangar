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

pub use worker::{RENDERER_LABEL, REPORT_EVERY, Runtime, Worker, offer, with_renderer_label};
