//! Cloud Hypervisor: its command line, its API, and the backends beside it.

mod api;
mod config;
mod gpu;
mod launch;
mod passt;
mod state;

pub use api::{Api, Memory};
pub use config::{Config, Disk, QUEUE_SIZE, boot_memory_mb, find, hotplug, plugged};
pub use gpu::{
    GpuBackend, GpuProbe, ProbedGl, ProbedVulkan, RENDERER_AUTO, RENDERER_HARDWARE,
    RENDERER_SOFTWARE, find_gpu_backend, gpu_env, probe_gpu,
};
pub use launch::{Backend, launch};
pub use passt::Passt;
pub use state::{log_path_for, ready_path, save_state, wait_file};
