//! Where each shared path is served from.

use serde::{Deserialize, Serialize};

/// A copy's ID and the paths it shares, as a session sends them: in the
/// home directory (`~/`), or absolute.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct SetPaths {
    pub id: String,
    #[serde(default)]
    pub paths: Vec<String>,
}

/// A path in the guest served from the shared files: `target`, a copy's
/// ID and the path in it. A directory's is everything under it. One that
/// excludes, inside a routed directory, is the guest's own again: served
/// from its disk, with no target.
///
/// Kept in the agent's state as the Go agent kept it, field names and all,
/// so an environment it routed carries on.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct Route {
    pub path: String,
    pub target: String,
    pub dir: bool,
    pub exclude: bool,
}

/// The routes for the copies an environment uses, in the order given: a
/// path two of them name, or one inside another's, is the first's. A path
/// in the home directory, `~/` -- or relative, as a server before packs
/// sent a profile's -- is routed under `home`.
pub fn routes_for(home: &str, sets: &[SetPaths]) -> Vec<Route> {
    let mut out: Vec<Route> = vec![];
    // The paths the copies before this one route: a path inside one of them
    // is theirs.
    let mut earlier: Vec<String> = vec![];
    for s in sets {
        for r in &out {
            if !r.exclude && !earlier.contains(&r.path) {
                earlier.push(r.path.clone());
            }
        }
        for p in &s.paths {
            let (x, exclude) = match p.strip_prefix('!') {
                Some(x) => (x, true),
                None => (p.as_str(), false),
            };
            let clean = x.strip_suffix('/').unwrap_or(x);
            if !valid(clean) {
                continue;
            }
            let (path, target) = if let Some(rel) = clean.strip_prefix("~/") {
                (join(home, rel), format!("{}/{clean}", s.id))
            } else if !clean.starts_with('/') {
                (join(home, clean), format!("{}/{clean}", s.id))
            } else {
                (clean.to_string(), format!("{}{clean}", s.id))
            };
            if out.iter().any(|r| r.path == path)
                || earlier.iter().any(|e| path.starts_with(&format!("{e}/")))
            {
                continue;
            }
            out.push(if exclude {
                Route {
                    path,
                    exclude: true,
                    ..Route::default()
                }
            } else {
                Route {
                    path,
                    target,
                    dir: x.ends_with('/'),
                    exclude: false,
                }
            });
        }
    }
    out
}

fn join(home: &str, rel: &str) -> String {
    format!("{}/{rel}", home.trim_end_matches('/'))
}

/// Whether `p` is a clean path in the home directory (`~/…`), a clean
/// relative one, or a clean absolute one.
fn valid(p: &str) -> bool {
    let rel = p.strip_prefix("~/").unwrap_or(p);
    if rel.contains('\0') {
        return false;
    }
    if let Some(abs) = rel.strip_prefix('/') {
        return !abs.is_empty() && clean(abs);
    }
    clean(rel)
}

/// Whether a relative path has no empty, `.` or `..` element.
fn clean(rel: &str) -> bool {
    !rel.is_empty()
        && rel
            .split('/')
            .all(|e| !e.is_empty() && e != "." && e != "..")
}

/// The copies `routes` route to.
pub fn copies_in(routes: &[Route]) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for r in routes.iter().filter(|r| !r.exclude) {
        let (copy, _) = copy_of(&r.target);
        if !out.contains(&copy) {
            out.push(copy);
        }
    }
    out
}

/// Where a guest path is served from under `routes`: the target, a copy's
/// file under the shared files, or nothing, for the guest's own disk. As
/// hangarfs does, an exclusion wins, then a route at the path itself, then
/// the nearest route it is inside, whether a directory route or not.
pub fn served(routes: &[Route], p: &str) -> String {
    let inside = |r: &Route| {
        p.strip_prefix(r.path.as_str())
            .is_some_and(|rest| rest.starts_with('/'))
    };
    let mut best: Option<&Route> = None;
    for r in routes {
        if r.exclude {
            if p == r.path || inside(r) {
                return String::new();
            }
            continue;
        }
        if p == r.path {
            return r.target.clone();
        }
        if inside(r) && best.is_none_or(|b| r.path.len() > b.path.len()) {
            best = Some(r);
        }
    }
    best.map(|b| format!("{}{}", b.target, &p[b.path.len()..]))
        .unwrap_or_default()
}

/// The copy a target is in, and the file's key in it: `~/…` in the home
/// directory, or absolute.
pub fn copy_of(target: &str) -> (String, String) {
    let (copy, rel) = target.split_once('/').unwrap_or((target, ""));
    if rel.starts_with("~/") {
        (copy.to_string(), rel.to_string())
    } else {
        (copy.to_string(), format!("/{rel}"))
    }
}
