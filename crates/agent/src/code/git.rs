use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use notify::{RecursiveMode, Watcher};
use serde::Serialize;
use tokio::sync::{broadcast, mpsc};

use super::child;
use super::error::{Error, Result};
use super::files::collate;
use super::paths::{normalize, relative, resolve};

/// A change git reports of a path, from the folder.
#[derive(Clone, Debug, Serialize)]
pub struct Change {
    pub path: String,
    /// The porcelain v2 status letters: index then working tree. '?' for an
    /// untracked file, 'U' for a conflict.
    pub index: String,
    pub worktree: String,
    /// For a rename, the path it had.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// For a submodule, git's four letters for it: S, then C where its
    /// commit moved, M where it has tracked changes, U untracked files.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub submodule: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct Status {
    pub repo: bool,
    /// Why git could not read a repository it found.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ahead: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behind: Option<i64>,
    pub changes: Vec<Change>,
}

/// A file a commit or a diff names, with how it changed.
#[derive(Clone, Debug, Serialize)]
pub struct Named {
    pub path: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

#[derive(Serialize)]
pub struct Diff {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    pub head: String,
    pub working: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary: Option<bool>,
}

#[derive(Serialize)]
pub struct Ref {
    name: String,
    kind: &'static str,
}

#[derive(Serialize)]
pub struct LogCommit {
    hash: String,
    parents: Vec<String>,
    author: String,
    email: String,
    time: f64,
    subject: String,
    refs: Vec<Ref>,
}

/// What a log is narrowed to: commits whose message has text, by an
/// author, and since a date git reads ("2026-11-01", "2 weeks ago").
#[derive(Default)]
pub struct LogFilter {
    pub text: Option<String>,
    pub author: Option<String>,
    pub since: Option<String>,
}

#[derive(Serialize)]
pub struct Log {
    commits: Vec<LogCommit>,
    more: bool,
}

#[derive(Serialize)]
pub struct CommitInfo {
    hash: String,
    parents: Vec<String>,
    author: String,
    email: String,
    time: f64,
    committer: String,
    #[serde(rename = "committerTime")]
    committer_time: f64,
    message: String,
    pub files: Vec<Named>,
}

#[derive(Serialize)]
pub struct BranchCommit {
    hash: String,
    subject: String,
    author: String,
    time: f64,
}

/// What a branch has changed since it left its base: the commits since, and
/// every file differing from the base, committed or not.
#[derive(Serialize)]
pub struct BranchChanges {
    base: String,
    #[serde(rename = "mergeBase")]
    merge_base: String,
    commits: Vec<BranchCommit>,
    pub changes: Vec<Named>,
}

#[derive(Default, Serialize)]
pub struct BlameCommit {
    author: String,
    time: f64,
    summary: String,
}

/// Who last changed each line of a file: the commits, by hash, and each
/// line's commit. Lines not yet committed have the all-zero hash.
#[derive(Serialize)]
pub struct Blame {
    commits: BTreeMap<String, BlameCommit>,
    lines: Vec<String>,
}

#[derive(Serialize)]
pub struct Branch {
    name: String,
    /// The branch it tracks, and how far apart the two are.
    #[serde(skip_serializing_if = "Option::is_none")]
    upstream: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ahead: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    behind: Option<i64>,
}

#[derive(Serialize)]
pub struct Branches {
    #[serde(skip_serializing_if = "Option::is_none")]
    current: Option<String>,
    local: Vec<Branch>,
    remote: Vec<String>,
}

/// A revision as given by a browser: a hash, a name or HEAD, with ^ and ~,
/// never an option.
fn revision(r: &str) -> Result<&str> {
    let ok = !r.is_empty()
        && r.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./~^@{}-".contains(c))
        && !r.starts_with('-')
        && !r.contains("..");
    if ok {
        Ok(r)
    } else {
        Err(Error::msg(format!("not a revision: {r}")))
    }
}

/// git's %D, refs named in full (--decorate=full):
/// "HEAD -> refs/heads/main, refs/remotes/origin/main, tag: refs/tags/v1".
fn parse_refs(d: &str) -> Vec<Ref> {
    let mut refs = Vec::new();
    for part in d.split(", ") {
        if part.is_empty() {
            continue;
        }
        let (head, part) = match part.strip_prefix("HEAD -> ") {
            Some(p) => (true, p),
            None => (false, part),
        };
        let part = part.strip_prefix("tag: ").unwrap_or(part);
        let r = |name: &str, kind| Ref {
            name: name.to_owned(),
            kind,
        };
        if part == "HEAD" {
            refs.push(r("HEAD", "head"));
        } else if let Some(n) = part.strip_prefix("refs/heads/") {
            refs.push(r(n, if head { "head" } else { "branch" }));
        } else if let Some(n) = part.strip_prefix("refs/remotes/") {
            if !n.ends_with("/HEAD") {
                refs.push(r(n, "remote"));
            }
        } else if let Some(n) = part.strip_prefix("refs/tags/") {
            refs.push(r(n, "tag"));
        }
    }
    refs
}

/// git's -z --name-status output.
fn name_status(out: &str) -> Vec<Named> {
    let parts: Vec<&str> = out.split('\0').collect();
    let mut files = Vec::new();
    let mut i = 0;
    let at = |i: usize| parts.get(i).copied().unwrap_or("").to_owned();
    while i < parts.len() {
        let status = parts[i];
        if status.is_empty() {
            i += 1;
            continue;
        }
        let letter = status[..1].to_owned();
        if status.starts_with('R') || status.starts_with('C') {
            files.push(Named {
                from: Some(at(i + 1)),
                path: at(i + 2),
                status: letter,
            });
            i += 3;
        } else {
            files.push(Named {
                path: at(i + 1),
                status: letter,
                from: None,
            });
            i += 2;
        }
    }
    files
}

/// Network commands answer or fail: nothing may wait on a prompt nobody
/// sees. A host seen for the first time is trusted, as a first clone is.
/// Reading takes no optional locks: a status that locked the index would be
/// seen by the watcher below as a change, and answered with another status.
const QUIET: [(&str, &str); 3] = [
    ("GIT_OPTIONAL_LOCKS", "0"),
    ("GIT_TERMINAL_PROMPT", "0"),
    (
        "GIT_SSH_COMMAND",
        "ssh -o BatchMode=yes -o StrictHostKeyChecking=accept-new",
    ),
];

const NETWORK: Duration = Duration::from_secs(120);

/// Runs git in `cwd` for its output, as bytes.
async fn run_bytes(cwd: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = exec(cwd, args, None, None, &[]).await?;
    if out.status == Some(0) {
        Ok(out.stdout)
    } else {
        Err(failed(&out))
    }
}

async fn run(cwd: &Path, args: &[&str]) -> Result<String> {
    run_with(cwd, args, None, None, &[]).await
}

async fn run_with(
    cwd: &Path,
    args: &[&str],
    input: Option<&str>,
    timeout: Option<Duration>,
    env: &[(&str, &str)],
) -> Result<String> {
    let out = exec(cwd, args, input.map(str::as_bytes), timeout, env).await?;
    if out.status == Some(0) {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(failed(&out))
    }
}

async fn exec(
    cwd: &Path,
    args: &[&str],
    input: Option<&[u8]>,
    timeout: Option<Duration>,
    env: &[(&str, &str)],
) -> Result<child::Output> {
    let mut c = child::command("git", args);
    c.current_dir(cwd);
    for (k, v) in QUIET.iter().chain(env) {
        c.env(k, v);
    }
    child::run(c, input, timeout)
        .await
        .map_err(|e| Error::msg(format!("git: {e}")))
}

/// What went wrong, as git said: its advice ("hint: ...") is for a
/// terminal, and what went wrong is the rest.
fn failed(out: &child::Output) -> Error {
    if out.timed_out {
        return Error::msg("git took too long");
    }
    let why: Vec<&str> = std::str::from_utf8(&out.stderr)
        .unwrap_or("")
        .lines()
        .filter(|l| !l.starts_with("hint:"))
        .collect();
    let why = why.join("\n").trim().to_owned();
    if why.is_empty() {
        Error::msg(format!("git exited with {}", out.status.unwrap_or(-1)))
    } else {
        Error::msg(why)
    }
}

const STATUS_ARGS: [&str; 8] = [
    "status",
    "--porcelain=v2",
    "--branch",
    "-z",
    "--untracked-files=all",
    "--find-renames",
    "--",
    ".",
];

/// Porcelain v2 status, its paths made the root's: they are from the top of
/// the repository, the root's prefix within it.
fn parse_status(out: &str, prefix: &str) -> Status {
    let own = |p: &str| -> String {
        match p.strip_prefix(prefix) {
            Some(rest) => rest.to_owned(),
            None => relative(&Path::new("/").join(prefix), &Path::new("/").join(p))
                .display()
                .to_string(),
        }
    };
    let mut status = Status {
        repo: true,
        ..Default::default()
    };
    let parts: Vec<&str> = out.split('\0').collect();
    let mut i = 0;
    while i < parts.len() {
        let line = parts[i];
        i += 1;
        if line.is_empty() {
            continue;
        }
        if let Some(b) = line.strip_prefix("# branch.head ") {
            status.branch = Some(b.to_owned());
        } else if let Some(ab) = line.strip_prefix("# branch.ab ") {
            let mut it = ab.split(' ');
            let n = |s: Option<&str>| s.and_then(|s| s.get(1..)).and_then(|s| s.parse().ok());
            status.ahead = n(it.next());
            status.behind = n(it.next());
        } else if line.starts_with("1 ") {
            let f: Vec<&str> = line.split(' ').collect();
            status
                .changes
                .push(change(&f, own(&f[8..].join(" ")), None));
        } else if line.starts_with("2 ") {
            let f: Vec<&str> = line.split(' ').collect();
            let from = own(parts.get(i).copied().unwrap_or(""));
            i += 1;
            status
                .changes
                .push(change(&f, own(&f[9..].join(" ")), Some(from)));
        } else if line.starts_with("u ") {
            let f: Vec<&str> = line.split(' ').collect();
            status.changes.push(Change {
                path: own(&f[10..].join(" ")),
                index: "U".into(),
                worktree: "U".into(),
                from: None,
                submodule: None,
            });
        } else if let Some(p) = line.strip_prefix("? ") {
            status.changes.push(Change {
                path: own(p),
                index: "?".into(),
                worktree: "?".into(),
                from: None,
                submodule: None,
            });
        }
    }
    status
}

fn change(f: &[&str], path: String, from: Option<String>) -> Change {
    let xy: Vec<char> = f[1].chars().collect();
    Change {
        path,
        index: xy.first().map(char::to_string).unwrap_or_default(),
        worktree: xy.get(1).map(char::to_string).unwrap_or_default(),
        from,
        submodule: f[2].starts_with('S').then(|| f[2].to_owned()),
    }
}

/// Where a root is in its repository.
#[derive(Clone)]
struct Where {
    /// The root's path from the top: "" at the top, else ending in a slash.
    prefix: String,
    git_dir: PathBuf,
    top: PathBuf,
}

/// Git answers for the repository the root is in, which may be the root or
/// a folder above it. Paths are the root's: a folder inside a repository sees
/// the changes within it, named from it.
pub struct Git {
    root: PathBuf,
    found: Mutex<Option<Where>>,
    changed: broadcast::Sender<()>,
    watch: Mutex<GitWatch>,
    /// Bumped by every change, so a settled change announces once.
    pulse: Arc<AtomicU64>,
}

#[derive(Default)]
struct GitWatch {
    listeners: usize,
    watcher: Option<notify::RecommendedWatcher>,
}

/// Held by each listener; the watch stops when the last one goes.
pub struct GitListening {
    git: Arc<Git>,
}

impl Drop for GitListening {
    fn drop(&mut self) {
        let mut w = self.git.watch.lock().unwrap();
        w.listeners -= 1;
        if w.listeners == 0 {
            w.watcher = None;
        }
    }
}

impl Git {
    pub fn new(root: &Path) -> Arc<Self> {
        Arc::new(Git {
            root: root.to_path_buf(),
            found: Mutex::new(None),
            changed: broadcast::channel(16).0,
            watch: Mutex::new(GitWatch::default()),
            pulse: Arc::new(AtomicU64::new(0)),
        })
    }

    /// The root's place in its repository. Not being in a repository is not
    /// remembered: one may be made.
    async fn where_(&self) -> Result<Where> {
        if let Some(w) = self.found.lock().unwrap().clone() {
            return Ok(w);
        }
        let out = run(
            &self.root,
            &[
                "rev-parse",
                "--show-prefix",
                "--absolute-git-dir",
                "--show-toplevel",
            ],
        )
        .await?;
        let mut lines = out.split('\n');
        let prefix = lines.next().unwrap_or("").to_owned();
        let git_dir = lines
            .next()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| self.root.join(".git"));
        let top = lines
            .next()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| self.root.clone());
        let w = Where {
            prefix,
            git_dir,
            top,
        };
        *self.found.lock().unwrap() = Some(w.clone());
        Ok(w)
    }

    /// The repository's top folder and the root's path from it, or None
    /// where the root is in none.
    pub async fn top(&self) -> Option<(PathBuf, String)> {
        self.where_().await.ok().map(|w| (w.top, w.prefix))
    }

    fn abs(&self, path: &str) -> PathBuf {
        resolve(&self.root, Path::new(path))
    }

    fn rel(&self, path: &str) -> String {
        relative(&self.root, &self.abs(path)).display().to_string()
    }

    /// The name git knows the file an editor shows by, from the root: an
    /// editor shows what links lead to, where git keeps a link as the name it
    /// holds, so the links are followed. None for a file they lead out of the
    /// repository.
    async fn shown(&self, path: &str) -> Result<Option<String>> {
        let abs = self.abs(path);
        let Ok(real) = tokio::fs::canonicalize(&abs).await else {
            // Not on disk: named as given.
            return Ok(Some(self.rel(path)));
        };
        // Git works from the root with links resolved, as the system has it.
        let root = tokio::fs::canonicalize(&self.root).await?;
        let top = tokio::fs::canonicalize(self.where_().await?.top).await?;
        let in_repo = relative(&top, &real);
        if in_repo.starts_with("..") || in_repo.is_absolute() {
            return Ok(None);
        }
        Ok(Some(relative(&root, &real).display().to_string()))
    }

    pub async fn status(&self) -> Status {
        let Ok(w) = self.where_().await else {
            return Status::default();
        };
        let out = match run(&self.root, &STATUS_ARGS).await {
            Ok(out) => out,
            Err(e) => {
                return Status {
                    error: Some(e.to_string()),
                    ..Default::default()
                };
            }
        };
        let status = parse_status(&out, &w.prefix);
        // A file moved without git being told is a deletion and an untracked
        // file, and git finds the rename only among files it knows of. They
        // are made known in a copy of the index, which status is asked again
        // with; the repository's own index is left as it was. A folder
        // untracked as a whole is a repository of its own, never a rename.
        let untracked: Vec<String> = status
            .changes
            .iter()
            .filter(|c| c.index == "?" && !c.path.ends_with('/'))
            .map(|c| c.path.clone())
            .collect();
        if !untracked.is_empty()
            && untracked.len() <= 2000
            && status.changes.iter().any(|c| c.worktree == "D")
            && let Ok(s) = self.with_renames(&w, &untracked).await
        {
            return s;
        }
        status
    }

    async fn with_renames(&self, w: &Where, untracked: &[String]) -> Result<Status> {
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "hangar-code-index-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        tokio::fs::create_dir_all(&dir).await?;
        let result = async {
            let index = dir.join("index");
            // No index yet: the copy starts empty.
            let _ = tokio::fs::copy(w.git_dir.join("index"), &index).await;
            let index = index.display().to_string();
            let env = [("GIT_INDEX_FILE", index.as_str())];
            run_with(
                &self.root,
                &[
                    "add",
                    "--intent-to-add",
                    "--pathspec-from-file=-",
                    "--pathspec-file-nul",
                ],
                Some(&untracked.join("\0")),
                None,
                &env,
            )
            .await?;
            let out = run_with(&self.root, &STATUS_ARGS, None, None, &env).await?;
            let mut known = parse_status(&out, &w.prefix);
            // Known only to the copy, a new file is still untracked.
            let new: HashSet<&String> = untracked.iter().collect();
            for c in &mut known.changes {
                if c.index == "." && c.worktree == "A" && new.contains(&c.path) {
                    c.index = "?".into();
                    c.worktree = "?".into();
                }
            }
            Ok(known)
        }
        .await;
        let _ = tokio::fs::remove_dir_all(&dir).await;
        result
    }

    /// What status says of a path, renames included.
    async fn change(&self, rel: &str) -> Option<Change> {
        self.status()
            .await
            .changes
            .into_iter()
            .find(|c| c.path == rel)
    }

    /// A file as it was at HEAD, or at another commit, and as the working
    /// tree has it, or a later commit (`to`), for a diff view to compare. A
    /// file new since was empty there; one renamed since is compared with
    /// its old name, which is given as `from`.
    pub async fn diff(
        &self,
        path: &str,
        against: &str,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<Diff> {
        let rel = self.rel(path);
        revision(against)?;
        if let Some(to) = to {
            revision(to)?;
        }
        if is_dir(&self.root.join(&rel)).await {
            return self.submodule_diff(&rel, against, to).await;
        }
        let mut was = from.map(|f| self.rel(f)).unwrap_or_else(|| rel.clone());
        let mut head = String::new();
        match run(&self.root, &["show", &format!("{against}:./{was}")]).await {
            Ok(h) => head = h,
            Err(_) => {
                was = rel.clone();
                // Not in HEAD, unless under the name it was renamed from.
                let renamed = if against == "HEAD" {
                    self.change(&rel).await.and_then(|c| c.from)
                } else {
                    None
                };
                if let Some(renamed) = renamed
                    && let Ok(h) = run(&self.root, &["show", &format!("HEAD:./{renamed}")]).await
                {
                    head = h;
                    was = renamed;
                }
            }
        }
        let mut working = String::new();
        if let Some(to) = to {
            if let Ok(w) = run(&self.root, &["show", &format!("{to}:./{rel}")]).await {
                working = w;
            }
        } else {
            // A link is the name it holds, as git compares it.
            let p = self.root.join(&rel);
            if let Ok(m) = tokio::fs::symlink_metadata(&p).await {
                working = if m.file_type().is_symlink() {
                    tokio::fs::read_link(&p)
                        .await
                        .map(|t| t.display().to_string())
                        .unwrap_or_default()
                } else {
                    tokio::fs::read(&p)
                        .await
                        .map(|b| String::from_utf8_lossy(&b).into_owned())
                        .unwrap_or_default()
                };
            }
        }
        let from = (was != rel).then_some(was);
        // Bytes that are not text are not compared as text: the view shows
        // them its own way, from blob.
        if head.contains('\0') || working.contains('\0') {
            return Ok(Diff {
                path: rel,
                from,
                head: String::new(),
                working: String::new(),
                binary: Some(true),
            });
        }
        Ok(Diff {
            path: rel,
            from,
            head,
            working,
            binary: None,
        })
    }

    /// A submodule compared as git's own diff does: by the commit each side
    /// names, the working tree's being the one it has checked out.
    async fn submodule_diff(&self, rel: &str, against: &str, to: Option<&str>) -> Result<Diff> {
        let named = |s: String| format!("Subproject commit {}\n", s.trim());
        let at = |rev: String| async move {
            run(
                &self.root,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("{rev}:./{rel}"),
                ],
            )
            .await
            .map(named)
            .unwrap_or_default()
        };
        let working = match to {
            Some(to) => at(to.to_owned()).await,
            None => run(&self.root.join(rel), &["rev-parse", "HEAD"])
                .await
                .map(named)
                .unwrap_or_default(),
        };
        Ok(Diff {
            path: rel.to_owned(),
            from: None,
            head: at(against.to_owned()).await,
            working,
            binary: None,
        })
    }

    /// A file as a commit has it, base64, with its size; empty when the
    /// commit has no such file.
    pub async fn blob(&self, rev: &str, path: &str) -> Result<serde_json::Value> {
        revision(rev)?;
        let rel = self.rel(path);
        Ok(
            match run_bytes(&self.root, &["show", &format!("{rev}:./{rel}")]).await {
                Ok(buf) => serde_json::json!({ "size": buf.len(), "data": BASE64.encode(&buf) }),
                Err(_) => serde_json::json!({ "size": 0, "data": "" }),
            },
        )
    }

    /// Adds a file's changes to the index; a rename's, both its names.
    pub async fn stage(&self, path: &str) -> Result<()> {
        let rel = self.rel(path);
        let from = self.change(&rel).await.and_then(|c| c.from);
        let mut args = vec!["add", "--all", "--", rel.as_str()];
        args.extend(from.as_deref());
        run(&self.root, &args).await?;
        self.changed();
        Ok(())
    }

    pub async fn unstage(&self, path: &str) -> Result<()> {
        let rel = self.rel(path);
        let from = self.change(&rel).await.and_then(|c| c.from);
        let mut args = vec!["restore", "--staged", "--", rel.as_str()];
        args.extend(from.as_deref());
        run(&self.root, &args).await?;
        self.changed();
        Ok(())
    }

    /// Discards a file's changes in the working tree and index, as
    /// IntelliJ's Rollback does. An untracked file is deleted.
    pub async fn rollback(&self, path: &str) -> Result<()> {
        let rel = self.rel(path);
        let c = self.change(&rel).await;
        match c {
            Some(Change {
                from: Some(from), ..
            }) => {
                // A rename goes back: the old name restored, the new one gone.
                run(
                    &self.root,
                    &[
                        "restore",
                        "--staged",
                        "--worktree",
                        "--source=HEAD",
                        "--",
                        &from,
                    ],
                )
                .await?;
                run(
                    &self.root,
                    &["rm", "--cached", "--quiet", "--ignore-unmatch", "--", &rel],
                )
                .await?;
                let _ = tokio::fs::remove_file(self.root.join(&rel)).await;
            }
            Some(c) if c.index == "?" => {
                run(&self.root, &["clean", "-f", "--", &rel]).await?;
            }
            _ => {
                run(
                    &self.root,
                    &[
                        "restore",
                        "--staged",
                        "--worktree",
                        "--source=HEAD",
                        "--",
                        &rel,
                    ],
                )
                .await?;
            }
        }
        self.changed();
        Ok(())
    }

    /// The file an editor shows as HEAD or the index has it, or None where
    /// it has none.
    pub async fn show(&self, path: &str, head: bool) -> Option<String> {
        let rel = self.shown(path).await.ok()??;
        let rev = if head { "HEAD" } else { "" };
        run(&self.root, &["show", &format!("{rev}:./{rel}")])
            .await
            .ok()
    }

    /// Stages the content of the file an editor shows as given, whatever
    /// the working tree holds: how part of a file's changes is staged.
    pub async fn set_index(&self, path: &str, content: &str) -> Result<()> {
        let Some(rel) = self.shown(path).await? else {
            return Err(Error::msg(format!("{path} is not in the repository")));
        };
        let staged = run(&self.root, &["ls-files", "--stage", "--", &rel]).await?;
        let staged = staged.split(' ').next().unwrap_or("");
        let mode = if staged.len() == 6
            && staged.starts_with('1')
            && staged[1..].chars().all(|c| ('0'..='7').contains(&c))
        {
            staged
        } else {
            "100644"
        };
        let sha = run_with(
            &self.root,
            &["hash-object", "-w", "--stdin"],
            Some(content),
            None,
            &[],
        )
        .await?;
        run(
            &self.root,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("{mode},{},{rel}", sha.trim()),
            ],
        )
        .await?;
        self.changed();
        Ok(())
    }

    /// Records the index. Amending replaces the last commit, keeping its
    /// message when none is given.
    pub async fn commit(&self, message: &str, amend: bool) -> Result<()> {
        let has = !message.trim().is_empty();
        if !has && !amend {
            return Err(Error::msg("a commit needs a message"));
        }
        let mut args = vec!["commit"];
        if amend {
            args.push("--amend");
        }
        if has {
            args.extend(["-F", "-"]);
        } else {
            args.push("--no-edit");
        }
        run_with(&self.root, &args, has.then_some(message), None, &[]).await?;
        self.changed();
        Ok(())
    }

    /// The last commit's message, for amending it.
    pub async fn last_message(&self) -> String {
        run(&self.root, &["log", "-1", "--format=%B"])
            .await
            .map(|m| m.trim_end().to_owned())
            .unwrap_or_default()
    }

    /// Who last changed each line of the file an editor shows, as it is on
    /// disk.
    pub async fn blame(&self, path: &str) -> Result<Blame> {
        let Some(rel) = self.shown(path).await? else {
            return Err(Error::msg(format!("{path} is not in the repository")));
        };
        let out = run(
            &self.root,
            &["blame", "--porcelain", "--", &format!("./{rel}")],
        )
        .await?;
        let mut blame = Blame {
            commits: BTreeMap::new(),
            lines: Vec::new(),
        };
        let mut hash = String::new();
        for line in out.split('\n') {
            if line.starts_with('\t') {
                blame.lines.push(hash.clone());
                continue;
            }
            let mut words = line.split(' ');
            let first = words.next().unwrap_or("");
            if first.len() == 40
                && first
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                && words.next().is_some_and(|w| w.parse::<u64>().is_ok())
                && words.next().is_some_and(|w| w.parse::<u64>().is_ok())
            {
                hash = first.to_owned();
                blame.commits.entry(hash.clone()).or_default();
                continue;
            }
            let Some(c) = blame.commits.get_mut(&hash) else {
                continue;
            };
            if let Some(a) = line.strip_prefix("author ") {
                c.author = a.to_owned();
            } else if let Some(t) = line.strip_prefix("author-time ") {
                c.time = t.parse().unwrap_or(0.0);
            } else if let Some(s) = line.strip_prefix("summary ") {
                c.summary = s.to_owned();
            }
        }
        Ok(blame)
    }

    /// The history, newest first and each commit after its children: every
    /// branch's, remote ones and tags too, or one ref's. It is read a page at
    /// a time, skipping those read already.
    ///
    /// With a path, it is that file's or folder's history: the commits that
    /// changed it, each one's parents rewritten to the nearest that did, so
    /// the graph still joins up. A rename is where the history starts.
    pub async fn log(
        &self,
        rev: Option<&str>,
        skip: f64,
        max: f64,
        path: Option<&str>,
        filter: &LogFilter,
    ) -> Result<Log> {
        let n = (max as i64).clamp(1, 2000);
        let skip = format!("--skip={}", skip.floor().max(0.0) as i64);
        let count = format!("--max-count={}", n + 1);
        let mut args = vec!["log", "--topo-order"];
        if path.is_some() {
            args.push("--parents");
        }
        args.extend(["--decorate=full", &skip, &count]);
        // The filters: text in the message, the author, and how recent, as
        // IntelliJ's Log narrows by; each one argument, so never an option.
        let grep = filter.text.as_ref().map(|t| format!("--grep={t}"));
        let author = filter.author.as_ref().map(|a| format!("--author={a}"));
        let since = filter.since.as_ref().map(|s| format!("--since={s}"));
        if grep.is_some() || author.is_some() {
            args.extend(["--regexp-ignore-case", "--fixed-strings"]);
        }
        args.extend(grep.as_deref());
        args.extend(author.as_deref());
        args.extend(since.as_deref());
        args.push("--format=%H%x1f%P%x1f%an%x1f%ae%x1f%at%x1f%D%x1f%s%x1e");
        match rev {
            Some(r) => args.push(revision(r)?),
            None => args.extend(["--exclude=refs/stash", "--all"]),
        }
        args.push("--");
        let p = path.map(|p| format!("./{}", self.rel(p)));
        args.extend(p.as_deref());
        let out = match run(&self.root, &args).await {
            Ok(out) => out,
            // A repository with no commits has no history.
            Err(e)
                if rev.is_none() && {
                    let m = e.to_string();
                    m.contains("does not have any commits")
                        || m.contains("bad default revision")
                        || m.contains("unknown revision")
                } =>
            {
                return Ok(Log {
                    commits: Vec::new(),
                    more: false,
                });
            }
            Err(e) => return Err(e),
        };
        let mut commits: Vec<LogCommit> = out
            .split('\x1e')
            .map(|r| r.strip_prefix('\n').unwrap_or(r))
            .filter(|r| !r.is_empty())
            .map(|r| {
                let f: Vec<&str> = r.split('\x1f').collect();
                let g = |i: usize| f.get(i).copied().unwrap_or("");
                LogCommit {
                    hash: g(0).to_owned(),
                    parents: words(g(1)),
                    author: g(2).to_owned(),
                    email: g(3).to_owned(),
                    time: g(4).parse().unwrap_or(0.0),
                    refs: parse_refs(g(5)),
                    subject: g(6).to_owned(),
                }
            })
            .collect();
        let more = commits.len() as i64 > n;
        commits.truncate(n as usize);
        Ok(Log { commits, more })
    }

    /// Which of `paths` git ignores.
    pub async fn ignored(&self, paths: &[PathBuf]) -> HashSet<PathBuf> {
        if paths.is_empty() || self.where_().await.is_err() {
            return HashSet::new();
        }
        let rels: Vec<String> = paths
            .iter()
            .map(|p| relative(&self.root, p).display().to_string())
            .collect();
        let input = rels.join("\0") + "\0";
        // 1 is none ignored; anything else wrong is the same, for this.
        let out = exec(
            &self.root,
            &["check-ignore", "-z", "--stdin"],
            Some(input.as_bytes()),
            None,
            &[],
        )
        .await
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
        let hit: HashSet<&str> = out.split('\0').filter(|s| !s.is_empty()).collect();
        paths
            .iter()
            .zip(&rels)
            .filter(|(_, r)| hit.contains(r.as_str()))
            .map(|(p, _)| p.clone())
            .collect()
    }

    /// Adds a path to the .gitignore at the top of the repository, anchored
    /// there, a folder with its slash.
    pub async fn ignore(&self, path: &str, dir: bool) -> Result<()> {
        let top = self.where_().await?.top;
        let abs = self.abs(path);
        let real_abs = tokio::fs::canonicalize(&abs).await.unwrap_or(abs);
        let rel = relative(&tokio::fs::canonicalize(&top).await?, &real_abs);
        if rel.as_os_str().is_empty() || rel.starts_with("..") || rel.is_absolute() {
            return Err(Error::msg(format!("{path} is not in the repository")));
        }
        // Characters a pattern would read are escaped, so it names the path.
        let mut pattern = String::from("/");
        for c in rel.display().to_string().chars() {
            if "\\*?[]!# ".contains(c) {
                pattern.push('\\');
            }
            pattern.push(c);
        }
        if dir {
            pattern.push('/');
        }
        let file = top.join(".gitignore");
        let have = tokio::fs::read_to_string(&file).await.unwrap_or_default();
        if have.lines().any(|l| l.trim_end_matches('\r') == pattern) {
            return Ok(());
        }
        let mut add = String::new();
        if !have.is_empty() && !have.ends_with('\n') {
            add.push('\n');
        }
        add.push_str(&pattern);
        add.push('\n');
        use tokio::io::AsyncWriteExt;
        let mut f = tokio::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&file)
            .await?;
        f.write_all(add.as_bytes()).await?;
        self.changed();
        Ok(())
    }

    /// One commit in full, with the files it changed in this folder, from
    /// its first parent.
    pub async fn commit_info(&self, hash: &str) -> Result<CommitInfo> {
        if !(4..=64).contains(&hash.len())
            || !hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(Error::msg(format!("not a commit: {hash}")));
        }
        let out = run(
            &self.root,
            &[
                "show",
                "-s",
                "--format=%H%x1f%P%x1f%an%x1f%ae%x1f%at%x1f%cn%x1f%ct%x1f%B",
                hash,
            ],
        )
        .await?;
        let f: Vec<&str> = out.splitn(8, '\x1f').collect();
        let g = |i: usize| f.get(i).copied().unwrap_or("");
        let full = g(0).to_owned();
        let parents = words(g(1));
        let changed = match parents.first() {
            Some(p) => {
                run(
                    &self.root,
                    &[
                        "diff",
                        "-z",
                        "--name-status",
                        "--find-renames",
                        "--relative",
                        p,
                        &full,
                        "--",
                        ".",
                    ],
                )
                .await?
            }
            None => {
                run(
                    &self.root,
                    &[
                        "diff-tree",
                        "-z",
                        "-r",
                        "--root",
                        "--no-commit-id",
                        "--name-status",
                        "--find-renames",
                        "--relative",
                        &full,
                        "--",
                        ".",
                    ],
                )
                .await?
            }
        };
        Ok(CommitInfo {
            hash: full,
            parents,
            author: g(2).to_owned(),
            email: g(3).to_owned(),
            time: g(4).parse().unwrap_or(0.0),
            committer: g(5).to_owned(),
            committer_time: g(6).parse().unwrap_or(0.0),
            message: g(7).trim_end().to_owned(),
            files: name_status(&changed),
        })
    }

    /// The branch others are based on: the remote's default (origin/HEAD),
    /// else the first of origin/main, origin/master, main and master there
    /// is.
    async fn default_base(&self) -> Option<String> {
        if let Ok(b) = run(
            &self.root,
            &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
        )
        .await
        {
            return Some(b.trim().to_owned());
        }
        for b in ["origin/main", "origin/master", "main", "master"] {
            if run(
                &self.root,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("{b}^{{commit}}"),
                ],
            )
            .await
            .is_ok()
            {
                return Some(b.to_owned());
            }
        }
        None
    }

    /// The working tree compared with where it left `base` (the default
    /// base when none is given).
    pub async fn branch_changes(&self, base: Option<&str>) -> Result<BranchChanges> {
        let b = match base.filter(|b| !b.is_empty()) {
            Some(b) => b.to_owned(),
            None => self
                .default_base()
                .await
                .ok_or_else(|| Error::msg("there is no main branch to compare with"))?,
        };
        if b.starts_with('-') {
            return Err(Error::msg(format!("not a branch: {b}")));
        }
        let merge_base = run(&self.root, &["merge-base", "HEAD", &b])
            .await?
            .trim()
            .to_owned();
        let log = run(
            &self.root,
            &[
                "log",
                "--max-count=200",
                "--format=%h%x1f%s%x1f%an%x1f%ct",
                &format!("{merge_base}..HEAD"),
            ],
        )
        .await?;
        let commits = log
            .split('\n')
            .filter(|l| !l.is_empty())
            .map(|l| {
                let f: Vec<&str> = l.split('\x1f').collect();
                let g = |i: usize| f.get(i).copied().unwrap_or("").to_owned();
                BranchCommit {
                    hash: g(0),
                    subject: g(1),
                    author: g(2),
                    time: g(3).parse().unwrap_or(0.0),
                }
            })
            .collect();
        let mut changes = name_status(
            &run(
                &self.root,
                &[
                    "diff",
                    "-z",
                    "--name-status",
                    "--find-renames",
                    "--relative",
                    &merge_base,
                    "--",
                    ".",
                ],
            )
            .await?,
        );
        // Files git does not know of yet are the branch's too, and one moved
        // without git is a rename of the file the diff has deleted.
        for c in self.status().await.changes {
            if changes.iter().any(|x| x.path == c.path) {
                continue;
            }
            if c.index == "?" {
                changes.push(Named {
                    path: c.path,
                    status: "A".into(),
                    from: None,
                });
            } else if c.index == "." && c.worktree == "R" {
                let Some(from) = c.from else { continue };
                match changes
                    .iter()
                    .position(|x| x.path == from && x.status == "D")
                {
                    Some(gone) => {
                        changes[gone] = Named {
                            path: c.path,
                            status: "R".into(),
                            from: Some(from),
                        }
                    }
                    None => changes.push(Named {
                        path: c.path,
                        status: "A".into(),
                        from: None,
                    }),
                }
            }
        }
        changes.sort_by(|x, y| collate(&x.path, &y.path));
        Ok(BranchChanges {
            base: b,
            merge_base,
            commits,
            changes,
        })
    }

    pub async fn branches(&self) -> Result<Branches> {
        let mut local = Vec::new();
        // Most recently committed to first: with many branches, the ones in
        // use come first.
        let out = run(
            &self.root,
            &[
                "for-each-ref",
                "--sort=-committerdate",
                "--format=%(refname:short)%09%(upstream:short)%09%(upstream:track,nobracket)%09%(HEAD)",
                "refs/heads",
            ],
        )
        .await?;
        let mut current = None;
        for line in out.split('\n').filter(|l| !l.is_empty()) {
            let f: Vec<&str> = line.split('\t').collect();
            let g = |i: usize| f.get(i).copied().unwrap_or("");
            let count = |word: &str| -> i64 {
                g(2).split(", ")
                    .find_map(|p| p.strip_prefix(word))
                    .and_then(|n| n.trim().parse().ok())
                    .unwrap_or(0)
            };
            let upstream = (!g(1).is_empty()).then(|| g(1).to_owned());
            let b = Branch {
                name: g(0).to_owned(),
                ahead: upstream.as_ref().map(|_| count("ahead ")),
                behind: upstream.as_ref().map(|_| count("behind ")),
                upstream,
            };
            if g(3) == "*" {
                current = Some(b.name.clone());
            }
            local.push(b);
        }
        let remote = run(
            &self.root,
            &[
                "for-each-ref",
                "--sort=-committerdate",
                "--format=%(refname:short)",
                "refs/remotes",
            ],
        )
        .await?
        .split('\n')
        .filter(|r| !r.is_empty() && !r.ends_with("/HEAD") && r.contains('/'))
        .map(str::to_owned)
        .collect();
        Ok(Branches {
            current,
            local,
            remote,
        })
    }

    /// Switches to a branch. A remote branch with no local one of its name
    /// gets one, tracking it.
    pub async fn checkout(&self, name: &str) -> Result<()> {
        let b = self.branches().await?;
        if b.local.iter().any(|l| l.name == name) {
            run(&self.root, &["checkout", name]).await?;
        } else if b.remote.iter().any(|r| r == name) {
            let short = name.split_once('/').map(|(_, s)| s).unwrap_or(name);
            if b.local.iter().any(|l| l.name == short) {
                run(&self.root, &["checkout", short]).await?;
            } else {
                run(&self.root, &["checkout", "--track", name]).await?;
            }
        } else {
            return Err(Error::msg(format!("no branch {name}")));
        }
        self.changed();
        Ok(())
    }

    /// Makes a branch and switches to it: at the commit named, else where
    /// the current one is.
    pub async fn create_branch(&self, name: &str, at: Option<&str>) -> Result<()> {
        run(&self.root, &["check-ref-format", "--branch", name]).await?;
        match at {
            Some(at) => run(&self.root, &["checkout", "-b", name, revision(at)?]).await?,
            None => run(&self.root, &["checkout", "-b", name]).await?,
        };
        self.changed();
        Ok(())
    }

    /// Checks out a commit, no branch then current.
    pub async fn checkout_commit(&self, rev: &str) -> Result<()> {
        run(&self.root, &["checkout", "--detach", revision(rev)?]).await?;
        self.changed();
        Ok(())
    }

    /// Makes a commit's change again on the current branch.
    pub async fn cherry_pick(&self, rev: &str) -> Result<()> {
        let done = run(&self.root, &["cherry-pick", revision(rev)?]).await;
        self.changed();
        done.map(|_| ())
    }

    /// Makes a commit undoing a commit's change.
    pub async fn revert(&self, rev: &str) -> Result<()> {
        let done = run(&self.root, &["revert", "--no-edit", revision(rev)?]).await;
        self.changed();
        done.map(|_| ())
    }

    /// Moves the current branch to a commit: keeping what changed staged
    /// (soft), changed but not staged (mixed), or throwing it away (hard).
    pub async fn reset(&self, rev: &str, mode: &str) -> Result<()> {
        let mode = match mode {
            "soft" => "--soft",
            "mixed" => "--mixed",
            "hard" => "--hard",
            m => return Err(Error::msg(format!("no such reset: {m}"))),
        };
        run(&self.root, &["reset", mode, revision(rev)?]).await?;
        self.changed();
        Ok(())
    }

    /// Merges a branch into the current one.
    pub async fn merge(&self, branch: &str) -> Result<()> {
        let done = run(&self.root, &["merge", "--no-edit", revision(branch)?]).await;
        self.changed();
        done.map(|_| ())
    }

    /// Rebases the current branch onto another.
    pub async fn rebase(&self, onto: &str) -> Result<()> {
        let done = run(&self.root, &["rebase", revision(onto)?]).await;
        self.changed();
        done.map(|_| ())
    }

    pub async fn rename_branch(&self, from: &str, to: &str) -> Result<()> {
        run(&self.root, &["check-ref-format", "--branch", to]).await?;
        run(&self.root, &["branch", "-m", revision(from)?, to]).await?;
        self.changed();
        Ok(())
    }

    /// Deletes a branch: only one merged, unless forced.
    pub async fn delete_branch(&self, name: &str, force: bool) -> Result<()> {
        let flag = if force { "-D" } else { "-d" };
        run(&self.root, &["branch", flag, revision(name)?]).await?;
        self.changed();
        Ok(())
    }

    /// Sends the current branch to what it tracks, or else to origin (or the
    /// only remote) under its own name, which it then tracks.
    pub async fn push(&self) -> Result<()> {
        let b = self.branches().await?;
        let Some(current) = b.current else {
            return Err(Error::msg("not on a branch"));
        };
        if b.local
            .iter()
            .find(|l| l.name == current)
            .is_some_and(|l| l.upstream.is_some())
        {
            run_with(&self.root, &["push"], None, Some(NETWORK), &[]).await?;
        } else {
            let remotes = run(&self.root, &["remote"]).await?;
            let remotes: Vec<&str> = remotes.split('\n').filter(|r| !r.is_empty()).collect();
            let remote = if remotes.contains(&"origin") {
                "origin"
            } else {
                remotes
                    .first()
                    .copied()
                    .ok_or_else(|| Error::msg("the repository has no remote"))?
            };
            run_with(
                &self.root,
                &["push", "--set-upstream", remote, &current],
                None,
                Some(NETWORK),
                &[],
            )
            .await?;
        }
        self.changed();
        Ok(())
    }

    /// Takes what the branch tracks, where that fast-forwards; anything
    /// needing a merge is left for the terminal.
    pub async fn pull(&self) -> Result<()> {
        run_with(&self.root, &["pull", "--ff-only"], None, Some(NETWORK), &[]).await?;
        self.changed();
        Ok(())
    }

    /// Checks out a submodule the repository names but has not cloned, and
    /// the submodules within it.
    pub async fn init_submodule(&self, path: &str) -> Result<()> {
        let p = format!("./{}", self.rel(path));
        run_with(
            &self.root,
            &["submodule", "update", "--init", "--recursive", "--", &p],
            None,
            Some(NETWORK),
            &[],
        )
        .await?;
        self.changed();
        Ok(())
    }

    pub async fn fetch(&self) -> Result<()> {
        run_with(
            &self.root,
            &["fetch", "--all", "--prune"],
            None,
            Some(NETWORK),
            &[],
        )
        .await?;
        self.changed();
        Ok(())
    }

    /// Where the submodules this repository names within its root would be,
    /// from its .gitmodules, whether cloned or not.
    pub async fn submodules(&self) -> Vec<PathBuf> {
        let Some((top, prefix)) = self.top().await else {
            return Vec::new();
        };
        let modules = top.join(".gitmodules").display().to_string();
        // No .gitmodules, or none in it.
        let out = match exec(
            &top,
            &[
                "config",
                "--file",
                &modules,
                "-z",
                "--get-regexp",
                "^submodule\\..*\\.path$",
            ],
            None,
            None,
            &[],
        )
        .await
        {
            Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
            Err(_) => String::new(),
        };
        out.split('\0')
            .filter(|e| !e.is_empty())
            .map(|e| e.split_once('\n').map(|(_, p)| p).unwrap_or(e))
            .map(|p| normalize(Path::new(p)).display().to_string())
            .filter(|p| p.starts_with(&prefix) && *p != prefix && !p.starts_with('/'))
            .map(|p| self.root.join(&p[prefix.len()..]))
            .collect()
    }

    /// Calls for a status when what git would report may have changed: the
    /// index or HEAD moved. Changes to files themselves arrive through Files.
    pub fn listen(self: &Arc<Self>) -> (broadcast::Receiver<()>, GitListening) {
        let rx = self.changed.subscribe();
        let first = {
            let mut w = self.watch.lock().unwrap();
            w.listeners += 1;
            w.listeners == 1
        };
        if first {
            let git = self.clone();
            tokio::spawn(async move {
                // Not a repository: nothing to follow.
                let Ok(w) = git.where_().await else { return };
                let mut watch = git.watch.lock().unwrap();
                if watch.listeners > 0 && watch.watcher.is_none() {
                    watch.watcher = git.watch_git(&w.git_dir);
                }
            });
        }
        (rx, GitListening { git: self.clone() })
    }

    fn watch_git(&self, git_dir: &Path) -> Option<notify::RecommendedWatcher> {
        let (tx, mut rx) = mpsc::unbounded_channel::<()>();
        let mut watcher = notify::recommended_watcher(move |ev: notify::Result<notify::Event>| {
            // A lock file comes and goes around every write, which is seen
            // when the file it guards changes.
            if let Ok(ev) = ev
                && super::files::changes(&ev)
                && ev
                    .paths
                    .iter()
                    .any(|p| !p.extension().is_some_and(|x| x == "lock"))
            {
                let _ = tx.send(());
            }
        })
        .ok()?;
        for target in [
            git_dir.to_path_buf(),
            git_dir.join("refs/heads"),
            git_dir.join("refs/remotes"),
        ] {
            // Not a repository, or no branches yet.
            let _ = watcher.watch(&target, RecursiveMode::NonRecursive);
        }
        let changed = self.changed.clone();
        let pulse = self.pulse.clone();
        tokio::spawn(async move {
            while rx.recv().await.is_some() {
                announce(&changed, &pulse);
            }
        });
        Some(watcher)
    }

    /// Announces a change once things have settled.
    fn changed(&self) {
        announce(&self.changed, &self.pulse);
    }
}

/// Sends a change 150 ms after the last of a run of them.
fn announce(changed: &broadcast::Sender<()>, pulse: &Arc<AtomicU64>) {
    let n = pulse.fetch_add(1, Ordering::SeqCst) + 1;
    let (changed, pulse) = (changed.clone(), pulse.clone());
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        if pulse.load(Ordering::SeqCst) == n {
            let _ = changed.send(());
        }
    });
}

fn words(s: &str) -> Vec<String> {
    s.split(' ')
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

async fn is_dir(p: &Path) -> bool {
    tokio::fs::symlink_metadata(p)
        .await
        .is_ok_and(|m| m.is_dir())
}
