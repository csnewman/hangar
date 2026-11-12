//! Sorting the files whose sharing changes, as the routes do: copied up
//! into a copy, kept aside as a conflict, or copied down to the guest.

use std::collections::HashSet;
use std::fs::{self, Metadata};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::files::{Files, as_user};
use super::routes::{Route, copy_of, served};
use super::{Guest, Message, RESOLVE_ENVIRONMENT, warn};

/// A file to be copied down once the routes are applied: the path stops
/// being shared, and the environment keeps the copy's file as its own.
pub(super) struct Down {
    path: String,
    data: Vec<u8>,
    mode: u32,
}

/// A path the environment had a file of its own at, differing from the
/// copy's, as the path became shared: the copy, and the file's key in it.
pub(super) struct Conflict {
    pub copy: String,
    pub key: String,
}

impl<F: Files> Guest<F> {
    /// Sorts, before `routes` are applied in place of `old`, each file
    /// whose sharing changes. A file the environment has, at a path
    /// becoming shared, is copied up into the copy, or kept aside as a
    /// conflict if the copy has a different one. A copy's file at a path
    /// becoming unshared is read, to be written once the routes are
    /// applied. A
    /// path that moves from one copy to another is neither: the environment
    /// simply sees the other copy's.
    pub(super) fn sort_paths(&self, old: &[Route], routes: &[Route]) -> (Vec<Down>, Vec<Conflict>) {
        let shared = self.files.shared();
        let mut downs = vec![];
        let mut conflicts = vec![];
        let mut seen = HashSet::new();

        // Becoming shared: the environment's own files under every path the
        // new routes share, or old ones left out.
        let roots = routes
            .iter()
            .filter(|r| !r.exclude)
            .chain(old.iter().filter(|r| r.exclude))
            .map(|r| r.path.clone());
        for root in roots {
            walk(Path::new(&root), &mut |p, md| {
                let Some(p) = p.to_str() else {
                    return;
                };
                if !seen.insert(p.to_string()) {
                    return;
                }
                let (was, now) = (served(old, p), served(routes, p));
                if !was.is_empty() || now.is_empty() {
                    return;
                }
                let Ok(local) = fs::read(p) else {
                    return;
                };
                let mode = md.permissions().mode() & 0o777;
                let theirs = shared.join(&now);
                match fs::read(&theirs) {
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        if let Err(e) = self.write_shared(&theirs, &local, mode) {
                            super::debug(&format!("copying {p} up: {e}"));
                        }
                    }
                    Ok(b) if b != local => {
                        let (copy, key) = copy_of(&now);
                        if let Err(e) = write_file(&self.kept_path(&copy, &key), &local, mode, None)
                        {
                            warn(&format!("keeping {p} aside: {e}"));
                            return;
                        }
                        conflicts.push(Conflict { copy, key });
                    }
                    _ => {}
                }
            });
        }

        // Becoming unshared: each old route's copy files, for those the new
        // routes leave to the environment.
        for r in old.iter().filter(|r| !r.exclude) {
            let base = shared.join(&r.target);
            walk(&base, &mut |p, md| {
                let Some(rest) = p.strip_prefix(&base).ok().and_then(Path::to_str) else {
                    return;
                };
                let rest = if rest.is_empty() {
                    String::new()
                } else {
                    format!("/{rest}")
                };
                let guest = format!("{}{rest}", r.path);
                if served(old, &guest) != format!("{}{rest}", r.target)
                    || !served(routes, &guest).is_empty()
                {
                    return;
                }
                if let Ok(data) = fs::read(p) {
                    downs.push(Down {
                        path: guest,
                        data,
                        mode: md.permissions().mode() & 0o777,
                    });
                }
            });
        }
        (downs, conflicts)
    }

    /// Writes the files that stop being shared into the environment, once the
    /// routes are applied, as the user's.
    pub(super) fn copy_down(&self, downs: Vec<Down>) {
        let owner = Some((self.user.uid, self.user.gid));
        for d in downs {
            if let Err(e) = write_file(Path::new(&d.path), &d.data, d.mode, owner) {
                warn(&format!("copying {} down: {e}", d.path));
            }
        }
    }

    /// Makes the directories each shared path is in, once the routes are
    /// applied. hangarfs finds a routed name by looking it up in its
    /// directory, so a path whose directory the environment lacks --
    /// ~/.aws/config in an image with no ~/.aws -- is not there until
    /// something makes it. They are the user's in the home directory, and
    /// root's elsewhere, as a mkdir -p by either would leave them; one
    /// inside a routed directory is the copy's, which the router made.
    pub(super) fn make_parents(&self, routes: &[Route]) {
        let home = format!("{}/", self.user.home.display());
        for r in routes.iter().filter(|r| !r.exclude) {
            let owner = r
                .path
                .starts_with(&home)
                .then_some((self.user.uid, self.user.gid));
            if let Some(dir) = Path::new(&r.path).parent()
                && let Err(e) = mkdir_all_owned(dir, owner)
            {
                warn(&format!("making {}'s directory: {e}", r.path));
            }
        }
    }

    /// Writes a file in the shared files as the environment's user, who
    /// owns it there as they would have had they written it themselves.
    fn write_shared(&self, name: &Path, data: &[u8], mode: u32) -> io::Result<()> {
        as_user(&self.user, || write_file(name, data, mode, None))
    }

    /// Where a conflict's file the environment had is kept aside.
    fn kept_path(&self, copy: &str, key: &str) -> PathBuf {
        self.kept_root()
            .join(copy)
            .join(key.trim_start_matches('/'))
    }

    fn kept_root(&self) -> PathBuf {
        self.files.state().join("kept")
    }

    /// Where the routes last applied are kept.
    fn routes_file(&self) -> PathBuf {
        self.files.state().join("routes.json")
    }

    /// The routes last applied, in this boot or before it.
    pub(super) fn last_routes(&self) -> Vec<Route> {
        if let Some(r) = &self.state.lock().unwrap().routes {
            return r.clone();
        }
        fs::read(self.routes_file())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Records the routes applied.
    pub(super) fn keep_routes(&self, routes: &[Route]) {
        let b = serde_json::to_vec(routes).unwrap_or_default();
        if let Err(e) = write_file(&self.routes_file(), &b, 0o600, None) {
            warn(&format!("keeping the routes applied: {e}"));
        }
    }

    /// Does what the server says of a conflict: drops the file kept aside,
    /// or writes it over the copy's.
    pub(super) fn resolve(&self, m: &Message) -> io::Result<()> {
        let kept = self.kept_path(&m.copy, &m.path);
        if m.resolution == RESOLVE_ENVIRONMENT {
            let written = fs::read(&kept).and_then(|data| {
                let mode = fs::metadata(&kept)?.permissions().mode() & 0o777;
                let theirs = self
                    .files
                    .shared()
                    .join(&m.copy)
                    .join(m.path.trim_start_matches('/'));
                self.write_shared(&theirs, &data, mode)
            });
            match written {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        match fs::remove_file(&kept) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        // The directories the file was in, once nothing else is.
        let top = self.kept_root();
        let mut dir = kept.parent();
        while let Some(d) = dir
            && d.starts_with(&top)
            && d != top
        {
            if fs::remove_dir(d).is_err() {
                break;
            }
            dir = d.parent();
        }
        Ok(())
    }

    /// Writes Claude's own record of this machine if it has none. It holds
    /// the projects Claude has been used in, its caches, and that its setup
    /// is done, and Claude rewrites it all the time, so it is not shared;
    /// an environment that has none is given one saying the setup is done,
    /// since the setup's choices -- the theme in settings.json and the
    /// sign-in -- come with the profile.
    pub(super) fn seed_claude_state(&self) -> io::Result<()> {
        let path = self.user.home.join(".claude.json");
        let mut f = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
            r => r?,
        };
        f.write_all(b"{\"hasCompletedOnboarding\": true}\n")?;
        drop(f);
        std::os::unix::fs::lchown(&path, Some(self.user.uid), Some(self.user.gid))
    }
}

/// Calls `f` for each regular file at or under `root`, in order, following
/// no symbolic link. What cannot be read is passed over.
fn walk(root: &Path, f: &mut dyn FnMut(&Path, &Metadata)) {
    let Ok(md) = fs::symlink_metadata(root) else {
        return;
    };
    if md.is_file() {
        f(root, &md);
        return;
    }
    if !md.is_dir() {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let mut names: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    names.sort();
    for p in names {
        walk(&p, f);
    }
}

/// Writes `data` at `name`, beside it and renamed over it, making the
/// directories it is in; owned by `owner`, when there is one.
pub(super) fn write_file(
    name: &Path,
    data: &[u8],
    mode: u32,
    owner: Option<(u32, u32)>,
) -> io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = name.parent().unwrap_or(Path::new("/"));
    mkdir_all_owned(dir, owner)?;
    let tmp = dir.join(format!(
        ".hangarfs-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(data)?;
        drop(f);
        fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))?;
        if let Some((uid, gid)) = owner {
            std::os::unix::fs::lchown(&tmp, Some(uid), Some(gid))?;
        }
        fs::rename(&tmp, name)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

/// Makes a directory and any parents missing, owned by `owner` when there
/// is one.
pub(super) fn mkdir_all_owned(dir: &Path, owner: Option<(u32, u32)>) -> io::Result<()> {
    if fs::metadata(dir).is_ok() {
        return Ok(());
    }
    if let Some(parent) = dir.parent() {
        mkdir_all_owned(parent, owner)?;
    }
    match fs::DirBuilder::new().mode(0o755).create(dir) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
        _ => {}
    }
    if let Some((uid, gid)) = owner {
        std::os::unix::fs::lchown(dir, Some(uid), Some(gid))?;
    }
    Ok(())
}
