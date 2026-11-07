use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use super::files::collate;
use super::git::{Git, GitListening};
use super::paths::{relative, resolve};
use super::workspace::scan;

/// The most repositories a folder's status covers.
const MAX_REPOS: usize = 64;

/// A change as the folder sees it, named from the folder. A submodule's is
/// its commit having moved, which its parent records.
#[derive(Clone, Serialize)]
pub struct RepoChange {
    path: String,
    index: String,
    worktree: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    from: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    submodule: bool,
}

#[derive(Serialize)]
pub struct Repo {
    /// From the folder: "" for the repository the folder is in.
    path: String,
    name: String,
    /// How the repository came to be in the folder: the one the folder is
    /// in, a submodule another names, or a clone of its own.
    kind: &'static str,
    /// The repository naming a submodule.
    #[serde(skip_serializing_if = "Option::is_none")]
    parent: Option<String>,
    /// A submodule named but not cloned.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    uninitialised: bool,
    /// Why git could not say what it holds.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ahead: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    behind: Option<i64>,
    changes: Vec<RepoChange>,
}

/// Every repository in the folder, and all their changes together.
#[derive(Serialize)]
pub struct Statuses {
    repo: bool,
    changes: Vec<RepoChange>,
    repos: Vec<Repo>,
}

/// Which repository a path is in.
pub struct At {
    pub git: Arc<Git>,
    /// Where that repository's own paths start, from the folder.
    pub base: String,
    pub abs: PathBuf,
}

/// The git repositories in a folder: the one it is in, those within it, its
/// submodules and theirs. Each call goes to the repository a path is in, its
/// innermost, and paths in what comes back are the folder's. The
/// repositories are found again with each status, which the browser asks for
/// whenever files change.
pub struct Repos {
    root: PathBuf,
    gits: Mutex<HashMap<PathBuf, Arc<Git>>>,
    /// The repositories the last status found.
    known: Mutex<Option<Vec<Known>>>,
    changed: broadcast::Sender<()>,
    watch: Mutex<ReposWatch>,
}

#[derive(Default)]
struct ReposWatch {
    listeners: usize,
    following: HashMap<PathBuf, (JoinHandle<()>, GitListening)>,
}

/// Held by each listener; the repositories are followed while any is.
pub struct ReposListening {
    repos: Arc<Repos>,
}

impl Drop for ReposListening {
    fn drop(&mut self) {
        self.repos.watch.lock().unwrap().listeners -= 1;
        self.repos.follow();
    }
}

struct Known {
    /// From the folder.
    path: String,
    uninitialised: bool,
    /// Whether git could read it, and so follow it.
    readable: bool,
}

struct Found {
    abs: PathBuf,
    kind: &'static str,
    parent: Option<String>,
    uninitialised: bool,
}

impl Repos {
    pub fn new(root: &Path) -> Arc<Self> {
        Arc::new(Repos {
            root: root.to_path_buf(),
            gits: Mutex::new(HashMap::new()),
            known: Mutex::new(None),
            changed: broadcast::channel(16).0,
            watch: Mutex::new(ReposWatch::default()),
        })
    }

    fn git(&self, abs: &Path) -> Arc<Git> {
        self.gits
            .lock()
            .unwrap()
            .entry(abs.to_path_buf())
            .or_insert_with(|| Git::new(abs))
            .clone()
    }

    fn abs(&self, path: &str) -> PathBuf {
        if path.is_empty() {
            self.root.clone()
        } else {
            self.root.join(path)
        }
    }

    pub async fn status(self: &Arc<Self>) -> Statuses {
        let mut queue: Vec<Found> = Vec::new();
        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut add = |queue: &mut Vec<Found>, f: Found| {
            if seen.insert(f.abs.clone()) {
                queue.push(f);
            }
        };
        let top = self.git(&self.root).top().await;
        if top.is_some() {
            add(&mut queue, found(self.root.clone(), "folder"));
        } else {
            for abs in scan(&self.root).await {
                add(&mut queue, found(PathBuf::from(abs), "clone"));
            }
        }
        let name_of_root = top
            .as_ref()
            .map(|(t, _)| t.clone())
            .unwrap_or_else(|| self.root.clone());

        let mut repos: Vec<Repo> = Vec::new();
        while !queue.is_empty() && repos.len() < MAX_REPOS {
            let take = queue.len().min(MAX_REPOS - repos.len());
            let batch: Vec<Found> = queue.drain(..take).collect();
            let done = futures_all(batch.into_iter().map(|f| {
                let me = self.clone();
                let name_of_root = name_of_root.clone();
                async move { me.one(f, &name_of_root).await }
            }))
            .await;
            for (repo, more) in done {
                repos.push(repo);
                for f in more {
                    add(&mut queue, f);
                }
            }
        }
        repos.sort_by(|a, b| match (a.path.is_empty(), b.path.is_empty()) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => collate(&a.path, &b.path),
        });
        *self.known.lock().unwrap() = Some(
            repos
                .iter()
                .map(|r| Known {
                    path: r.path.clone(),
                    uninitialised: r.uninitialised,
                    readable: !r.uninitialised && r.error.is_none(),
                })
                .collect(),
        );
        self.follow();
        Statuses {
            repo: !repos.is_empty(),
            changes: repos.iter().flat_map(|r| r.changes.clone()).collect(),
            repos,
        }
    }

    /// One repository's status, and the repositories it leads to: its
    /// submodules, and clones of their own within it.
    async fn one(&self, f: Found, name_of_root: &Path) -> (Repo, Vec<Found>) {
        let path = relative(&self.root, &f.abs).display().to_string();
        let mut repo = Repo {
            name: if path.is_empty() {
                base_name(name_of_root)
            } else {
                path.clone()
            },
            path: path.clone(),
            kind: f.kind,
            parent: f.parent,
            uninitialised: false,
            error: None,
            branch: None,
            ahead: None,
            behind: None,
            changes: Vec::new(),
        };
        if f.uninitialised {
            repo.uninitialised = true;
            return (repo, Vec::new());
        }
        let git = self.git(&f.abs);
        let st = git.status().await;
        if !st.repo {
            repo.error = Some(st.error.unwrap_or_else(|| "git cannot read it".into()));
            return (repo, Vec::new());
        }
        let mut more = Vec::new();
        for sub in git.submodules().await {
            let uninitialised = !has_git(&sub).await;
            more.push(Found {
                abs: sub,
                kind: "submodule",
                parent: Some(path.clone()),
                uninitialised,
            });
        }
        let base = if path.is_empty() {
            String::new()
        } else {
            format!("{path}/")
        };
        for c in st.changes {
            // A folder untracked whole is a repository of its own.
            if c.index == "?" && c.path.ends_with('/') {
                let nested = f.abs.join(&c.path);
                if has_git(&nested).await {
                    more.push(found(
                        PathBuf::from(nested.display().to_string().trim_end_matches('/')),
                        "clone",
                    ));
                    continue;
                }
            }
            let submodule = c.submodule.is_some();
            let mut worktree = c.worktree;
            // A submodule's own changes are its own; its parent has only its
            // commit to record, and has it in its working tree only once the
            // commit has moved.
            if let Some(s) = &c.submodule
                && s.chars().nth(1) != Some('C')
            {
                worktree = ".".into();
            }
            if submodule && c.index == "." && worktree == "." {
                continue;
            }
            repo.changes.push(RepoChange {
                path: format!("{base}{}", c.path),
                index: c.index,
                worktree,
                from: c.from.map(|f| format!("{base}{f}")),
                submodule,
            });
        }
        repo.branch = st.branch;
        repo.ahead = st.ahead;
        repo.behind = st.behind;
        (repo, more)
    }

    /// The repository a path is in, the innermost, and the path from the
    /// folder at which that repository's own paths start. A path in none goes
    /// to the folder's own, which says it is in none. A repository's own
    /// folder is in it when the repository is asked for (`whole`), and else
    /// is in its parent, as the submodule or folder its parent sees.
    pub async fn at(self: &Arc<Self>, path: &str, whole: bool) -> At {
        let abs = resolve(&self.root, Path::new(path));
        if self.known.lock().unwrap().is_none() {
            self.status().await;
        }
        let mut best = String::new();
        for r in self.known.lock().unwrap().iter().flatten() {
            if r.path.is_empty() || r.uninitialised || r.path.len() <= best.len() {
                continue;
            }
            let dir = self.abs(&r.path);
            if (whole && abs == dir) || abs.starts_with(&dir) && abs != dir {
                best = r.path.clone();
            }
        }
        At {
            git: self.git(&self.abs(&best)),
            base: if best.is_empty() {
                String::new()
            } else {
                format!("{best}/")
            },
            abs,
        }
    }

    /// Called for a status when what any of the repositories would report
    /// may have changed.
    pub fn listen(self: &Arc<Self>) -> (broadcast::Receiver<()>, ReposListening) {
        let rx = self.changed.subscribe();
        self.watch.lock().unwrap().listeners += 1;
        self.follow();
        (
            rx,
            ReposListening {
                repos: self.clone(),
            },
        )
    }

    /// Follows each repository found while anyone listens, and none once
    /// nobody does.
    fn follow(&self) {
        let want: HashSet<PathBuf> = {
            let w = self.watch.lock().unwrap();
            if w.listeners == 0 {
                HashSet::new()
            } else {
                match &*self.known.lock().unwrap() {
                    Some(known) => known
                        .iter()
                        .filter(|r| r.readable)
                        .map(|r| self.abs(&r.path))
                        .collect(),
                    None => [self.root.clone()].into(),
                }
            }
        };
        let mut w = self.watch.lock().unwrap();
        w.following.retain(|abs, (task, _)| {
            let keep = want.contains(abs);
            if !keep {
                task.abort();
            }
            keep
        });
        for abs in want {
            if w.following.contains_key(&abs) {
                continue;
            }
            let (mut rx, listening) = self.git(&abs).listen();
            let changed = self.changed.clone();
            let task = tokio::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(()) => {
                            let _ = changed.send(());
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => return,
                    }
                }
            });
            w.following.insert(abs, (task, listening));
        }
    }
}

fn found(abs: PathBuf, kind: &'static str) -> Found {
    Found {
        abs,
        kind,
        parent: None,
        uninitialised: false,
    }
}

fn base_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

async fn has_git(dir: &Path) -> bool {
    tokio::fs::metadata(dir.join(".git")).await.is_ok()
}

/// Every future's output, in order, the futures run together.
async fn futures_all<F: std::future::Future<Output = T> + Send + 'static, T: Send + 'static>(
    futs: impl Iterator<Item = F>,
) -> Vec<T> {
    let handles: Vec<JoinHandle<T>> = futs.map(tokio::spawn).collect();
    let mut out = Vec::with_capacity(handles.len());
    for h in handles {
        if let Ok(v) = h.await {
            out.push(v);
        }
    }
    out
}
