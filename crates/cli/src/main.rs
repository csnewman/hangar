//! hangar: the Hangar CLI. It builds what workers boot: the guest kernel
//! and the distributions' images. Checking a machine and writing EROFS
//! images by hand are hangar-worker's (`hangar-worker doctor`,
//! `hangar-worker erofs`).

mod assets;
mod build;
mod flags;
mod kernel;
mod oci;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use flags::{Flag, Stop};

const USAGE: &str = "hangar - development environments for coding agents

Usage:
  hangar build [flags]       build a distribution's images with mkosi
  hangar oci   [flags]       write built images as an OCI layout, to push
  hangar kernel [flags]      build the guest kernel

Checking a machine and writing EROFS images are hangar-worker's:
\"hangar-worker doctor\" and \"hangar-worker erofs\".

Run \"hangar <command> -h\" for the flags of a command.
";

/// This machine's architecture as Go and OCI name it.
pub fn go_arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "amd64",
        other => other,
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first() else {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    };
    let rest = &args[1..];
    let run = match command.as_str() {
        "build" => build(rest),
        "oci" => write_oci(rest),
        "kernel" => build_kernel(rest),
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        other => {
            eprint!("unknown command {other:?}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failed::Stop(Stop::Help)) => ExitCode::SUCCESS,
        Err(Failed::Stop(Stop::Wrong)) => ExitCode::from(2),
        Err(Failed::Why(why)) => {
            eprintln!("hangar: {why}");
            ExitCode::FAILURE
        }
    }
}

enum Failed {
    Stop(Stop),
    Why(String),
}

impl From<Stop> for Failed {
    fn from(s: Stop) -> Failed {
        Failed::Stop(s)
    }
}

impl From<String> for Failed {
    fn from(s: String) -> Failed {
        Failed::Why(s)
    }
}

fn write_oci(args: &[String]) -> Result<(), Failed> {
    let f = flags::parse(
        "oci",
        &[
            Flag::value(
                "i",
                "out",
                "what `hangar build -o` wrote: <i>/<tier>/rootfs",
            ),
            Flag::value(
                "o",
                "out/oci",
                "OCI layout to write, with an image per tier named by its tier",
            ),
            Flag::value("arch", go_arch(), "the images' architecture"),
        ],
        args,
    )?;
    let input = PathBuf::from(f.get("i"));
    let rootfs: Vec<PathBuf> = oci::TIERS
        .iter()
        .map(|t| input.join(t).join("rootfs"))
        .collect();
    let created = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
    let out = Path::new(f.get("o"));
    oci::write(out, &rootfs, f.get("arch"), &created)
        .map_err(|e| format!("writing {}: {e}", out.display()))?;
    eprintln!(
        "wrote {}: [{}], each named by its tier",
        out.display(),
        oci::TIERS.join(" ")
    );
    Ok(())
}

fn build(args: &[String]) -> Result<(), Failed> {
    let f = flags::parse(
        "build",
        &[
            Flag::value(
                "C",
                "images/ubuntu-26.04",
                "distribution directory, holding minimal/ and base/",
            ),
            Flag::value(
                "o",
                "out",
                "output directory, on a Linux filesystem; each tier's image is <o>/<tier>/rootfs",
            ),
            Flag::switch("v", "show build output"),
        ],
        args,
    )?;
    let platform = build::platform();
    eprintln!("building {} with mkosi for {platform}", f.get("C"));
    let built = build::build(
        Path::new(f.get("C")),
        Path::new(f.get("o")),
        &platform,
        f.on("v"),
    )?;
    eprintln!();
    for r in &built.rootfs {
        eprintln!("built {}", r.display());
    }
    eprintln!("build {}", built.build);
    Ok(())
}

fn build_kernel(args: &[String]) -> Result<(), Failed> {
    let f = flags::parse(
        "kernel",
        &[
            Flag::value(
                "version",
                assets::kernel_version(),
                "upstream kernel version",
            ),
            Flag::value("arch", "", "guest architecture (default: host)"),
            Flag::value("o", "out/kernel", "output directory"),
            Flag::value("j", "0", "parallel build jobs (default: all cores)"),
            Flag::switch("v", "show build output"),
            Flag::value(
                "base",
                "tinyconfig",
                "kconfig base target the fragment merges onto",
            ),
            Flag::switch(
                "config-only",
                "resolve and verify the config without compiling",
            ),
            Flag::switch("16k", "16 KiB pages: the arm64 kernel for workers on a Mac"),
        ],
        args,
    )?;
    let arch = match f.get("arch") {
        "" => match go_arch() {
            "arm64" => "arm64".to_string(),
            "amd64" => "x86_64".to_string(),
            other => return Err(format!("unsupported host architecture {other:?}").into()),
        },
        a => a.to_string(),
    };
    let o = kernel::Options {
        version: f.get("version").to_string(),
        arch,
        out: PathBuf::from(f.get("o")),
        jobs: f.number("j")?,
        verbose: f.on("v"),
        base: f.get("base").to_string(),
        config_only: f.on("config-only"),
        pages_16k: f.on("16k"),
    };
    if o.config_only {
        eprintln!("resolving kernel config {} (base={})", o.version, o.base);
    } else {
        eprintln!("building kernel {} (this takes a while)", o.version);
    }
    let built = kernel::build(&o)?;
    if o.config_only {
        eprintln!("config written to {}", built.config.display());
        return Ok(());
    }
    eprintln!("\nbuilt {}", built.release);
    let size = |p: &Path| std::fs::metadata(p).map(|m| human_size(m.len())).ok();
    if let Some(s) = size(&built.image) {
        eprintln!("  {:<16} {s}", "vmlinuz");
    }
    if let Some(s) = built.elf.as_deref().and_then(size) {
        eprintln!("  {:<16} {s}", "vmlinux");
    }
    eprintln!("  {:<16} {}", "config", built.config.display());
    Ok(())
}

fn human_size(n: u64) -> String {
    const UNIT: u64 = 1024;
    if n < UNIT {
        return format!("{n} B");
    }
    let (mut div, mut exp) = (UNIT, 0);
    let mut m = n / UNIT;
    while m >= UNIT {
        div *= UNIT;
        exp += 1;
        m /= UNIT;
    }
    format!(
        "{:.1} {}iB",
        n as f64 / div as f64,
        ['K', 'M', 'G', 'T'][exp]
    )
}
