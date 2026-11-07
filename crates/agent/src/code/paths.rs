//! Paths as Node's `path` module treats them: resolved and made relative by
//! their text alone, `.` and `..` folded away, never through the disk.

use std::path::{Component, Path, PathBuf};

/// `path.resolve(base, p)`: `p` if it is absolute, else `p` from `base`,
/// either normalised.
pub fn resolve(base: &Path, p: &Path) -> PathBuf {
    normalize(&base.join(p))
}

/// `.` dropped and `..` taken back, a leading `..` at the root staying there.
pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        out.push("/");
    }
    out
}

/// `path.relative(from, to)`: how to get from one absolute path to another,
/// empty when they are the same.
pub fn relative(from: &Path, to: &Path) -> PathBuf {
    let (from, to) = (normalize(from), normalize(to));
    let a: Vec<_> = from.components().collect();
    let b: Vec<_> = to.components().collect();
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut out = PathBuf::new();
    for _ in common..a.len() {
        out.push("..");
    }
    for c in &b[common..] {
        out.push(c.as_os_str());
    }
    out
}

/// Whether `p` is `base` or inside it.
pub fn within(base: &Path, p: &Path) -> bool {
    let rel = relative(base, p);
    !rel.starts_with("..") && !rel.is_absolute()
}

/// An absolute path as a tab names it: from `root` when inside it.
pub fn named(root: &Path, abs: &Path) -> String {
    let rel = relative(root, abs);
    if !rel.as_os_str().is_empty() && within(root, abs) {
        rel.display().to_string()
    } else {
        abs.display().to_string()
    }
}
