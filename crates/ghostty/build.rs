//! Builds libghostty-vt from Ghostty's source at the release pinned here,
//! with Zig (the version that release asks for, 0.17), and links it in
//! statically. HANGAR_GHOSTTY_SOURCE names a checkout to build instead.
//!
//! src/sys.rs is bindgen's reading of that release's include/ghostty/vt.h;
//! dev/ghostty-bindings.sh writes it again for a new release.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The release of libghostty-vt built, and its source archive's SHA-256.
const TAG: &str = "vt-1.0.1";
const SHA256: &str = "a441bd6fd79505d5611aa54e90031d48a0625571cd40bfed7c03c8b956ea110c";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=HANGAR_GHOSTTY_SOURCE");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let source = match std::env::var("HANGAR_GHOSTTY_SOURCE") {
        Ok(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => fetch(&out),
    };
    let install = out.join("install");
    let stamp = install.join(".built");
    let wanted = format!("{}\n{}", source.display(), target());
    if std::fs::read_to_string(&stamp).ok().as_deref() != Some(wanted.as_str()) {
        build(&source, &install);
        std::fs::write(&stamp, &wanted).expect("writing the build's stamp");
    }
    println!(
        "cargo:rustc-link-search=native={}",
        install.join("lib").display()
    );
    let name = if target().contains("windows") {
        "ghostty-vt-static"
    } else {
        "ghostty-vt"
    };
    println!("cargo:rustc-link-lib=static={name}");
}

/// The Rust target being built for.
fn target() -> String {
    std::env::var("TARGET").unwrap()
}

/// Downloads and unpacks the release's source into out, once.
fn fetch(out: &Path) -> PathBuf {
    let dir = out.join(format!("ghostty-{TAG}"));
    if dir.join("build.zig").exists() {
        return dir;
    }
    let archive = out.join(format!("ghostty-{TAG}.tar.gz"));
    let url = format!("https://github.com/ghostty-org/ghostty/archive/refs/tags/{TAG}.tar.gz");
    run(Command::new("curl")
        .args(["-sSfL", "--retry", "3", "-o"])
        .arg(&archive)
        .arg(&url));
    let bytes = std::fs::read(&archive).expect("reading the downloaded source");
    let got = hex(&sha2::Digest::finalize(
        <sha2::Sha256 as sha2::Digest>::new_with_prefix(&bytes),
    ));
    assert_eq!(got, SHA256, "{url} is not the source pinned in build.rs");
    let unpacked = out.join("unpacked");
    let _ = std::fs::remove_dir_all(&unpacked);
    std::fs::create_dir_all(&unpacked).unwrap();
    run(Command::new("tar")
        .arg("xzf")
        .arg(&archive)
        .arg("-C")
        .arg(&unpacked));
    let top = std::fs::read_dir(&unpacked)
        .unwrap()
        .filter_map(Result::ok)
        .find(|e| e.path().join("build.zig").exists())
        .expect("the archive holds Ghostty's source")
        .path();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::rename(top, &dir).unwrap();
    let _ = std::fs::remove_file(&archive);
    dir
}

/// Builds the library from source into install.
fn build(source: &Path, install: &Path) {
    let mut zig = Command::new(std::env::var("ZIG").unwrap_or_else(|_| "zig".into()));
    zig.current_dir(source).args([
        "build",
        "-Demit-lib-vt",
        "-Doptimize=ReleaseFast",
        "-Demit-xcframework=false",
        "-Dapp-runtime=none",
    ]);
    zig.arg("--prefix").arg(install);
    // Zig builds for the machine it runs on unless told otherwise.
    if std::env::var("HOST").unwrap() != target() {
        zig.arg(format!("-Dtarget={}", zig_target(&target())));
    }
    run(&mut zig);
}

/// Zig's name for a Rust target.
fn zig_target(rust: &str) -> &'static str {
    match rust {
        "aarch64-apple-darwin" => "aarch64-macos",
        "x86_64-apple-darwin" => "x86_64-macos",
        "aarch64-unknown-linux-gnu" => "aarch64-linux-gnu",
        "x86_64-unknown-linux-gnu" => "x86_64-linux-gnu",
        "aarch64-unknown-linux-musl" => "aarch64-linux-musl",
        "x86_64-unknown-linux-musl" => "x86_64-linux-musl",
        "x86_64-pc-windows-msvc" => "x86_64-windows-msvc",
        "aarch64-pc-windows-msvc" => "aarch64-windows-msvc",
        other => panic!("no Zig target for {other}"),
    }
}

fn run(c: &mut Command) {
    let status = c
        .status()
        .unwrap_or_else(|e| panic!("running {:?}: {e}", c.get_program()));
    assert!(status.success(), "{c:?} failed: {status}");
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
