//! Hangar's guest kernel, built in a throwaway container.
//!
//! The kernel is Hangar's, not the distribution's, for two reasons. It has
//! to match the device model environments use (virtio for the disks, the
//! filesystem and the network, vsock for the agent), and everything on the
//! boot path has to be built in rather than a module, because the
//! initramfs assembles an overlay root before any filesystem exists to
//! load modules from. Distribution kernels satisfy neither reliably:
//! Ubuntu ships overlayfs as a module and wraps arm64 kernels in an EFI
//! zboot container a direct kernel boot cannot use.
//!
//! Only the configuration fragments, patches and Hangar's own kernel code
//! are source (kernel/): the kernel tree is downloaded as it builds and
//! never committed.

use std::path::{Path, PathBuf};

use crate::build::{docker, require_docker};

const BUILDER: &str = "ubuntu:26.04";

pub struct Options {
    /// The upstream version, such as 7.2.9.
    pub version: String,
    /// arm64 or x86_64.
    pub arch: String,
    pub out: PathBuf,
    pub jobs: u32,
    pub verbose: bool,
    /// The kconfig target the fragments merge onto. tinyconfig starts from
    /// nothing, so anything in the kernel is there because it was asked
    /// for.
    pub base: String,
    /// Resolves and checks the configuration without compiling: seconds,
    /// where compiling takes the best part of an hour.
    pub config_only: bool,
    /// arm64 with 16 KiB pages (hangar-arm64-16k.config): the kernel for
    /// guests on a Mac, where libkrun maps their GPU memory in the Mac's
    /// 16 KiB pages.
    pub pages_16k: bool,
}

pub struct Built {
    /// The raw Image or bzImage, for a direct kernel boot.
    pub image: PathBuf,
    /// x86_64's kernel as ELF, which libkrun boots.
    pub elf: Option<PathBuf>,
    /// The resolved .config.
    pub config: PathBuf,
    /// Such as 7.2.9.
    pub release: String,
}

/// The user's cache directory, as Go's os.UserCacheDir finds it.
fn cache_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        return home.map(|h| h.join("Library/Caches"));
    }
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home.map(|h| h.join(".cache")))
}

pub fn build(o: &Options) -> Result<Built, String> {
    if o.pages_16k && o.arch != "arm64" {
        return Err(format!(
            "16 KiB pages are an arm64 kernel's, not {}'s",
            o.arch
        ));
    }
    std::fs::create_dir_all(&o.out).map_err(|e| format!("{}: {e}", o.out.display()))?;
    require_docker("the kernel")?;
    let out = std::path::absolute(&o.out).map_err(|e| e.to_string())?;
    let assets = crate::assets::kernel().map_err(|e| e.to_string())?;
    let base = if o.base.is_empty() {
        "tinyconfig"
    } else {
        &o.base
    };
    let mut args: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "-e".into(),
        format!("KERNEL_VERSION={}", o.version),
        "-e".into(),
        format!("KARCH={}", o.arch),
        "-e".into(),
        format!("KBASE={base}"),
        "-v".into(),
        format!("{}:/out", out.display()),
        "-v".into(),
        format!("{}:/cfg:ro", assets.path().display()),
        "-v".into(),
        format!("{}:/scripts:ro", assets.path().display()),
    ];
    // The kernel's tarball is kept across runs: iterating on the
    // configuration should not mean downloading it every time.
    if let Some(cache) = cache_dir() {
        let k = cache.join("hangar").join("kernel-src");
        if std::fs::create_dir_all(&k).is_ok() {
            args.extend(["-v".into(), format!("{}:/cache", k.display())]);
        }
    }
    if o.config_only {
        args.extend(["-e".into(), "CONFIG_ONLY=1".into()]);
    }
    if o.pages_16k {
        args.extend(["-e".into(), "KEXTRA=hangar-arm64-16k.config".into()]);
    }
    if o.jobs > 0 {
        args.extend(["-e".into(), format!("JOBS={}", o.jobs)]);
    }
    // The guest kernel must match the guest's architecture, which follows
    // the host's, so the builder runs on the same platform.
    args.extend([
        "--platform".into(),
        crate::build::platform(),
        BUILDER.into(),
        "sh".into(),
        "/scripts/build.sh".into(),
    ]);
    docker(&args, o.verbose).map_err(|e| format!("kernel build: {e}"))?;

    let built = Built {
        image: o.out.join("vmlinuz"),
        elf: (o.arch == "x86_64").then(|| o.out.join("vmlinux")),
        config: o.out.join("config"),
        release: std::fs::read_to_string(o.out.join("kernelrelease"))
            .map(|r| r.trim().to_string())
            .unwrap_or_default(),
    };
    if o.config_only {
        return Ok(built);
    }
    let want: Vec<&Path> = [Some(&built.image), Some(&built.config), built.elf.as_ref()]
        .into_iter()
        .flatten()
        .map(PathBuf::as_path)
        .collect();
    for p in want {
        if !p.exists() {
            return Err(format!("expected artefact missing: {}", p.display()));
        }
    }
    Ok(built)
}
