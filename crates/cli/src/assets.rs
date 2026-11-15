//! What the builds take into their containers, built into the CLI so it
//! runs from anywhere: the image builder's scripts (images/scripts) and
//! the kernel's build script, configuration fragments, patches and source
//! (kernel/). A debug build reads them from the checkout as it runs.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(rust_embed::RustEmbed)]
#[folder = "../../images/scripts/"]
struct ImageScripts;

#[derive(rust_embed::RustEmbed)]
#[folder = "../../kernel/"]
struct Kernel;

/// The kernel version `hangar kernel` builds unless told another: a
/// longterm release, since environments are long lived and a kernel CVE
/// should be a point release rather than a series bump. The worker image's
/// build reads the same file, so the kernel it ships is this one.
pub fn kernel_version() -> String {
    Kernel::get("version")
        .map(|f| String::from_utf8_lossy(&f.data).trim().to_string())
        .unwrap_or_default()
}

/// A directory of the assets' own, removed when this goes.
pub struct Written(PathBuf);

impl Written {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Written {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write<E: rust_embed::RustEmbed>(prefix: &str) -> io::Result<Written> {
    let dir = std::env::temp_dir().join(format!(
        "hangar-{prefix}-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    fs::create_dir_all(&dir)?;
    let written = Written(dir);
    for name in E::iter() {
        let f = E::get(&name).ok_or_else(|| io::Error::other(format!("{name} went missing")))?;
        let path = written.0.join(name.as_ref());
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, &f.data)?;
    }
    Ok(written)
}

/// The image builder's scripts, for the builder container's /scripts.
pub fn image_scripts() -> io::Result<Written> {
    write::<ImageScripts>("scripts")
}

/// The kernel's build script and what it builds from, for the builder
/// container's /scripts and /cfg alike.
pub fn kernel() -> io::Result<Written> {
    write::<Kernel>("kernel")
}
