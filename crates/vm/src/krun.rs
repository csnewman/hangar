//! hangar-krun, which runs one environment's machine under libkrun: the
//! monitor on macOS, where Cloud Hypervisor cannot run, and on Linux the
//! other to Cloud Hypervisor. The worker starts it with a Config as JSON;
//! libkrun takes the process over, and it exits when the guest does.
//!
//! libkrun is built from source with dev/krun-build.sh, which also builds
//! hangar-krun against it, and on macOS signs it with the hypervisor
//! entitlement macOS requires.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::ch::GpuProbe;
use crate::err;

pub use hangar_api::monitor::{
    Config, Disk, Features, Gpu, Net, VULKAN_KOSMICKRISP, VULKAN_MOLTENVK,
};

/// Where hangar-krun is: on PATH, or what dev/krun-build.sh builds, from
/// the repository's root. Linux's is apart from the Mac's, which a Linux
/// machine sharing the checkout also sees.
pub fn find() -> io::Result<PathBuf> {
    if let Some(p) = std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .map(|d| d.join("hangar-krun"))
        .find(|p| p.is_file())
    {
        return Ok(p);
    }
    let out = if cfg!(target_os = "linux") {
        "krun-linux"
    } else {
        "krun"
    };
    let p = Path::new("out").join(out).join("bin/hangar-krun");
    if p.exists() {
        return std::path::absolute(p);
    }
    Err(err(format!(
        "hangar-krun not found on PATH or in {}: build it with dev/krun-build.sh",
        p.display()
    )))
}

/// A program built beside hangar-krun for a worker on macOS, such as
/// mkfs.ext4.
pub fn tool(name: &str) -> io::Result<PathBuf> {
    let bin = find()?;
    let p = bin.parent().unwrap_or(Path::new("/")).join(name);
    if !p.exists() {
        return Err(err(format!(
            "{name} is not beside hangar-krun: build it with dev/krun-build.sh"
        )));
    }
    Ok(p)
}

async fn ask(
    bin: Option<&Path>,
    arg: &str,
    env: &[(String, String)],
    timeout: Duration,
) -> io::Result<Vec<u8>> {
    let bin = match bin {
        Some(b) => b.to_path_buf(),
        None => find()?,
    };
    let out = tokio::time::timeout(
        timeout,
        tokio::process::Command::new(bin)
            .arg(arg)
            .envs(env.iter().map(|(k, v)| (k, v)))
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| {
        err(format!(
            "hangar-krun {arg} did not answer within {timeout:?}"
        ))
    })??;
    if !out.status.success() {
        return Err(err(format!("hangar-krun {arg}: {}", out.status)));
    }
    Ok(out.stdout)
}

/// What hangar-krun's libkrun was built with; bin names hangar-krun, or
/// none finds it.
pub async fn read_features(bin: Option<&Path>) -> io::Result<Features> {
    let out = ask(bin, "-features", &[], Duration::from_secs(30))
        .await
        .map_err(|e| err(format!("asking hangar-krun what it offers: {e}")))?;
    serde_json::from_slice(&out).map_err(|e| err(format!("hangar-krun's features: {e}")))
}

/// What the host renders with under env, as hangar-krun's libkrun's GPU
/// renders on Linux.
pub async fn probe_gpu(bin: Option<&Path>, env: &[(String, String)]) -> io::Result<GpuProbe> {
    let out = ask(bin, "-probe", env, Duration::from_secs(30))
        .await
        .map_err(|e| err(format!("probing the GPU with hangar-krun: {e}")))?;
    serde_json::from_slice(&out).map_err(|e| err(format!("hangar-krun's GPU probe: {e}")))
}
