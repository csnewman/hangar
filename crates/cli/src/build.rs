//! A distribution's images, built with mkosi: each tier's root filesystem
//! as a directory, which a worker makes into the EROFS image its
//! environments mount. An image carries nothing else: the kernel, the
//! initramfs and the agent are the node's, and an environment's writable
//! layer and Docker disk are made by the worker that runs it.
//!
//! mkosi assembles an OS image from distribution packages, which matters
//! for two things a VM needs and a container image does not: udev (on
//! Debian and Ubuntu a separate package from systemd, without which a
//! guest has no network), and no container markers, so systemd boots the
//! guest as a machine rather than in container mode.
//!
//! It runs in a throwaway builder container needing mount and namespace
//! permissions: CAP_SYS_ADMIN with Docker's seccomp and AppArmor profiles
//! off, not --privileged, and no Docker socket. This is a trusted base
//! image build, not a sandbox for user builds. Its output keeps the
//! image's owners and modes, so it must be written to a Linux filesystem.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::oci::TIERS;

const BUILDER: &str = "ubuntu:26.04";

/// Where an image says what it offers, inside its root filesystem;
/// mkosi.sh writes the build's ID into it.
const INFO: &str = "usr/share/hangar/image.json";

pub struct Built {
    /// Each tier's root filesystem, in TIERS' order.
    pub rootfs: Vec<PathBuf>,
    /// This build's ID, which a worker copying a local build reads to tell
    /// one build from the next.
    pub build: String,
}

/// Fails unless Docker is on PATH and its daemon answers.
pub fn require_docker(what: &str) -> Result<(), String> {
    match Command::new("docker")
        .arg("info")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Err(_) => Err(format!(
            "docker not found in PATH; it is used to build {what}"
        )),
        Ok(s) if !s.success() => Err(format!("docker daemon is not reachable ({s})")),
        Ok(_) => Ok(()),
    }
}

/// The Docker platform of this machine: the guest's architecture follows
/// the host's, so what is built for it must match.
pub fn platform() -> String {
    format!("linux/{}", crate::go_arch())
}

/// Runs docker with args, its output shown only when verbose.
pub fn docker(args: &[String], verbose: bool) -> Result<(), String> {
    let mut cmd = Command::new("docker");
    cmd.args(args).stdin(Stdio::null()).stderr(Stdio::inherit());
    if !verbose {
        cmd.stdout(Stdio::null());
    } else {
        cmd.stdout(std::io::stderr());
    }
    match cmd.status() {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(s.to_string()),
        Err(e) => Err(e.to_string()),
    }
}

fn abs(p: &Path) -> Result<PathBuf, String> {
    std::path::absolute(p).map_err(|e| format!("{}: {e}", p.display()))
}

/// Builds the distribution in dir, which holds a directory per tier, into
/// out: each tier's image is <out>/<tier>/rootfs.
pub fn build(dir: &Path, out: &Path, platform: &str, verbose: bool) -> Result<Built, String> {
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    require_docker("images")?;
    let (out_abs, cfg) = (abs(out)?, abs(dir)?);
    for tier in TIERS {
        if !cfg.join(tier).join("mkosi.conf").is_file() {
            return Err(format!("no {tier}/mkosi.conf in {}", cfg.display()));
        }
    }
    // The images/ directory is mounted rather than the one distribution,
    // so a configuration can reach a sibling: every tier overlays
    // ../../common. IMAGE says which distribution to build.
    let images = cfg
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", cfg.display()))?;
    let name = cfg
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let scripts = crate::assets::image_scripts().map_err(|e| e.to_string())?;
    let mut args: Vec<String> = [
        "run",
        "--rm",
        // mkosi makes user namespaces and mounts; this is the narrowest set
        // that works, rather than --privileged.
        "--cap-add",
        "SYS_ADMIN",
        "--security-opt",
        "seccomp=unconfined",
        "--security-opt",
        "apparmor=unconfined",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.extend([
        "-e".into(),
        format!("IMAGE={name}"),
        // Somewhere to build that is not the container's own overlay: mkosi
        // stacks an overlay of build packages on the tree, and an overlay's
        // upper layer cannot itself be on overlayfs. An anonymous volume is
        // on the Docker host's filesystem and goes with the container.
        "-v".into(),
        "/work".into(),
        "-v".into(),
        format!("{}:/out", out_abs.display()),
        "-v".into(),
        format!("{}:/cfg:ro", images.display()),
        "-v".into(),
        format!("{}:/scripts:ro", scripts.path().display()),
    ]);
    if !platform.is_empty() {
        args.extend(["--platform".into(), platform.to_string()]);
    }
    args.extend([BUILDER.into(), "sh".into(), "/scripts/mkosi.sh".into()]);
    docker(&args, verbose).map_err(|e| format!("mkosi build: {e}"))?;

    let mut rootfs = Vec::new();
    for tier in TIERS {
        let r = out.join(tier).join("rootfs");
        if !r.is_dir() {
            return Err(format!("expected artefact missing: {}", r.display()));
        }
        rootfs.push(r);
    }
    let info = rootfs[rootfs.len() - 1].join(INFO);
    let b = std::fs::read(&info).map_err(|e| format!("{}: {e}", info.display()))?;
    let v: serde_json::Value = serde_json::from_slice(&b).map_err(|e| format!("{INFO}: {e}"))?;
    let build = v["build"].as_str().unwrap_or_default().to_string();
    if build.is_empty() {
        return Err(format!("{INFO} names no build"));
    }
    Ok(Built { rootfs, build })
}
