//! Environments as virtual machines, under Cloud Hypervisor or libkrun.

use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use hangar_api::{EnvironmentSpec, GpuInfo, ImageCopy, LocalImage, ObservedEnvironment, Resources};
use hangar_images::{Auth, HangarRegistry};
use hangar_vm::guest::GuestConn;
use hangar_vm::instance::ImageDevice;
use hangar_vm::log::Log;
use hangar_vm::runtime::{self, FilesView};

use crate::config::Config;

/// The runtime's configuration, from the worker's.
pub fn runtime_config(cfg: &Config, log: Log) -> io::Result<runtime::Config> {
    let image_device = ImageDevice::parse(&cfg.vm.image_device).ok_or_else(|| {
        io::Error::other(format!(
            "vm.image_device {:?}: want disk or pmem",
            cfg.vm.image_device
        ))
    })?;
    Ok(runtime::Config {
        monitor: cfg.runtime.clone(),
        state_dir: PathBuf::from(&cfg.storage.environments),
        images_dir: PathBuf::from(&cfg.storage.images),
        kernel: PathBuf::from(&cfg.vm.kernel),
        agent: PathBuf::from(&cfg.vm.agent),
        images: cfg
            .vm
            .images
            .iter()
            .map(|(r, i)| (r.clone(), PathBuf::from(&i.base)))
            .collect(),
        registries: cfg
            .vm
            .registries
            .iter()
            .map(|(host, a)| {
                (
                    host.clone(),
                    Auth {
                        username: a.username.clone(),
                        password_file: PathBuf::from(&a.password_file),
                    },
                )
            })
            .collect(),
        upper_gib: cfg.vm.upper_gib,
        docker_gib: cfg.vm.docker_gib,
        swap: cfg.vm.swap.unwrap_or(1.0),
        image_device,
        gpu_venus: cfg.vm.gpu.venus,
        gpu_venus_restore: cfg.vm.gpu.venus_restore,
        gpu_window_mib: cfg.vm.gpu.window_mib,
        gpu_renderer: cfg.vm.gpu.renderer.clone(),
        gpu_device: cfg.vm.gpu.device.clone(),
        log,
        ..runtime::Config::default()
    })
}

impl crate::Runtime for Arc<runtime::Runtime> {
    type Conn = GuestConn;

    fn apply(&self, spec: EnvironmentSpec) {
        runtime::Runtime::apply(self, spec);
    }

    fn observe(&self) -> Vec<ObservedEnvironment> {
        runtime::Runtime::observe(self)
    }

    fn changed(&self) -> impl Future<Output = ()> + Send {
        runtime::Runtime::changed(self)
    }

    fn dial(&self, id: &str, port: u32) -> impl Future<Output = io::Result<GuestConn>> + Send {
        runtime::Runtime::dial(self, id, port)
    }

    fn log_file(&self, id: &str, name: &str) -> Option<PathBuf> {
        runtime::Runtime::log_file(self, id, name)
    }

    fn images(&self) -> Vec<LocalImage> {
        runtime::Runtime::images(self)
    }

    fn remove_image(&self, copy: &ImageCopy) {
        runtime::Runtime::remove_image(self, copy);
    }

    fn use_registry(&self, hangar: Option<HangarRegistry>) {
        runtime::Runtime::use_registry(self, hangar);
    }

    fn gpu(&self) -> Option<GpuInfo> {
        runtime::Runtime::gpu(self).cloned()
    }

    fn monitor(&self) -> (String, bool) {
        (
            runtime::Runtime::monitor(self).to_string(),
            runtime::Runtime::suspends(self),
        )
    }

    fn use_capacity(&self, offered: Resources) {
        runtime::Runtime::use_capacity(self, offered);
    }

    fn use_shared_files(&self, root: PathBuf, view: FilesView) {
        runtime::Runtime::use_shared_files(self, root, view);
    }
}
