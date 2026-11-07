use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use regex::{Captures, Regex, RegexBuilder};
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, BufReader};

use super::child;
use super::error::{Error, Result};
use super::paths::normalize;

/// The most files listed for Go to File, and the most matches one search
/// returns.
const MAX_FILES: usize = 100_000;
const MAX_MATCHES: usize = 2000;
/// A match's line is cut to this many characters around the match.
const MAX_LINE: usize = 400;
/// Files larger than this are not searched by the fallback search.
const MAX_SEARCH_SIZE: u64 = 2 << 20;
const SEARCH_TIMEOUT: Duration = Duration::from_secs(20);

/// Directories never listed or searched without git or ripgrep to say what
/// is ignored: they churn, are generated, or are someone else's.
const SKIP: [&str; 8] = [
    ".git",
    "node_modules",
    ".cache",
    "target",
    "dist",
    "__pycache__",
    ".venv",
    "vendor",
];

pub struct TextQuery {
    pub query: String,
    pub regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
    /// Comma-separated globs, as IntelliJ's file mask: "*.go, !*_test.go".
    pub mask: Option<String>,
    /// A folder to search in, from the root; the root by default.
    pub dir: Option<String>,
}

#[derive(Serialize)]
pub struct Match {
    path: String,
    /// 1-based.
    line: u64,
    text: String,
    /// Each match in text, as [start, end) offsets in UTF-16 code units, as
    /// the browser's strings count them.
    ranges: Vec<[usize; 2]>,
}

#[derive(Serialize)]
pub struct TextResult {
    matches: Vec<Match>,
    truncated: bool,
}

#[derive(Serialize)]
pub struct FileList {
    files: Vec<String>,
    truncated: bool,
}

#[derive(Serialize)]
pub struct Replaced {
    files: Vec<String>,
    replaced: usize,
}

async fn have_ripgrep() -> bool {
    static HAVE: OnceLock<bool> = OnceLock::new();
    if let Some(h) = HAVE.get() {
        return *h;
    }
    let ok = child::run(child::command("rg", &["--version"]), None, None)
        .await
        .is_ok_and(|o| o.status == Some(0));
    *HAVE.get_or_init(|| ok)
}

/// Runs a command for its output, failing on anything but success.
async fn exec(cmd: &str, args: &[&str], cwd: &Path) -> std::result::Result<String, Option<i32>> {
    let mut c = child::command(cmd, args);
    c.current_dir(cwd);
    match child::run(c, None, Some(SEARCH_TIMEOUT)).await {
        Ok(o) if o.status == Some(0) => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
        Ok(o) => Err(o.status),
        Err(_) => Err(None),
    }
}

/// Finds files by name and text in files under a root, leaving out what git
/// (or ripgrep) is told to ignore.
pub struct Search {
    root: PathBuf,
}

impl Search {
    pub fn new(root: &Path) -> Self {
        Search {
            root: root.to_path_buf(),
        }
    }

    /// The files under the root, relative to it: git's tracked and untracked
    /// but not ignored files in a repository, ripgrep's otherwise, or a walk
    /// that skips the usual generated directories.
    pub async fn files(&self) -> FileList {
        let mut list: Option<Vec<String>> = exec(
            "git",
            &[
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
            ],
            &self.root,
        )
        .await
        .ok()
        .map(|out| {
            let mut v: Vec<String> = out
                .split('\0')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect();
            v.sort();
            v.dedup();
            v
        });
        if list.is_none() && have_ripgrep().await {
            list = exec(
                "rg",
                &["--files", "--hidden", "--glob", "!.git", "-0"],
                &self.root,
            )
            .await
            .ok()
            .map(|out| {
                out.split('\0')
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect()
            });
        }
        let mut list = match list {
            Some(l) => l,
            None => self.walk().await,
        };
        list.sort();
        let truncated = list.len() > MAX_FILES;
        list.truncate(MAX_FILES);
        FileList {
            files: list,
            truncated,
        }
    }

    async fn walk(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![String::new()];
        while let Some(rel) = stack.pop() {
            if out.len() > MAX_FILES {
                break;
            }
            let Ok(mut rd) = tokio::fs::read_dir(self.root.join(&rel)).await else {
                continue;
            };
            let mut subdirs = Vec::new();
            while let Ok(Some(e)) = rd.next_entry().await {
                let name = e.file_name().to_string_lossy().into_owned();
                let p = if rel.is_empty() {
                    name.clone()
                } else {
                    format!("{rel}/{name}")
                };
                let Ok(ft) = e.file_type().await else {
                    continue;
                };
                if ft.is_dir() {
                    if !SKIP.contains(&name.as_str()) {
                        subdirs.push(p);
                    }
                } else if ft.is_file() || ft.is_symlink() {
                    out.push(p);
                }
            }
            stack.extend(subdirs.into_iter().rev());
        }
        out
    }

    /// Lines matching a query: with ripgrep where the image has it, else by
    /// reading each file Go to File would list.
    pub async fn text(&self, q: &TextQuery) -> Result<TextResult> {
        if q.query.is_empty() {
            return Ok(TextResult {
                matches: Vec::new(),
                truncated: false,
            });
        }
        let re = to_regex(q)?;
        // A folder outside the root is refused before anything is read.
        scope(q.dir.as_deref())?;
        if have_ripgrep().await
            && let Ok(r) = self.ripgrep(q, &re).await
        {
            return Ok(r);
        }
        self.scan(q, &re).await
    }

    fn rg_args(q: &TextQuery, first: &[&str]) -> Result<Vec<String>> {
        let mut args: Vec<String> = first.iter().map(|s| s.to_string()).collect();
        if !q.regex {
            args.push("--fixed-strings".into());
        }
        if !q.case_sensitive {
            args.push("--ignore-case".into());
        }
        if q.whole_word {
            args.push("--word-regexp".into());
        }
        for g in globs(q.mask.as_deref()) {
            args.push("--glob".into());
            args.push(g);
        }
        args.push("--".into());
        args.push(q.query.clone());
        args.push(scope(q.dir.as_deref())?);
        Ok(args)
    }

    async fn ripgrep(&self, q: &TextQuery, re: &Regex) -> Result<TextResult> {
        let max_columns = (MAX_LINE * 4).to_string();
        let args = Self::rg_args(
            q,
            &[
                "--json",
                "--hidden",
                "--glob",
                "!.git",
                "--max-columns",
                &max_columns,
                "--max-filesize",
                "8M",
            ],
        )?;
        let mut c = child::command("rg", &args);
        c.current_dir(&self.root)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .stdin(std::process::Stdio::null());
        let mut proc = c.spawn().map_err(|e| Error::msg(e.to_string()))?;
        let stdout = proc.stdout.take().expect("piped stdout");
        let mut lines = BufReader::new(stdout).lines();
        let mut matches = Vec::new();
        let mut truncated = false;
        let deadline = Instant::now() + SEARCH_TIMEOUT;
        loop {
            let next = tokio::time::timeout_at(deadline.into(), lines.next_line()).await;
            let line = match next {
                Err(_) => {
                    truncated = true;
                    break;
                }
                Ok(Ok(Some(l))) => l,
                Ok(_) => break,
            };
            if matches.len() >= MAX_MATCHES {
                truncated = true;
                break;
            }
            let Ok(m) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if m["type"] != "match" {
                continue;
            }
            let d = &m["data"];
            let path = d["path"]["text"].as_str().unwrap_or("");
            let path = path.strip_prefix("./").unwrap_or(path);
            let Some(text) = d["lines"]["text"].as_str() else {
                continue;
            };
            if path.is_empty() {
                continue;
            }
            let text = text.strip_suffix('\n').unwrap_or(text);
            let text = text.strip_suffix('\r').unwrap_or(text);
            matches.push(cut(path, d["line_number"].as_u64().unwrap_or(0), text, re));
        }
        let _ = proc.start_kill();
        let code = proc.wait().await.ok().and_then(|s| s.code());
        // 1 is no matches; 2 is an error, such as a bad pattern, unless some
        // files simply could not be read.
        if code == Some(2) && matches.is_empty() && !truncated {
            return Err(Error::msg("ripgrep failed"));
        }
        Ok(TextResult { matches, truncated })
    }

    /// The files a query looks in, without ripgrep: Go to File's, in its
    /// folder and its mask.
    async fn in_scope(&self, q: &TextQuery) -> Result<Vec<String>> {
        let files = self.files().await.files;
        let include = matcher(q.mask.as_deref());
        let dir = scope(q.dir.as_deref())?;
        Ok(files
            .into_iter()
            .filter(|f| include(f) && (dir == "." || f.starts_with(&format!("{dir}/"))))
            .collect())
    }

    /// Replaces every match of a query with `replacement`, in the files given
    /// or every file it matches in: $1 and the like are a regular
    /// expression's groups, as IntelliJ's replace has them. Returns the files
    /// changed and how many matches were replaced.
    pub async fn replace(
        &self,
        q: &TextQuery,
        replacement: &str,
        only: Option<Vec<String>>,
    ) -> Result<Replaced> {
        if q.query.is_empty() {
            return Ok(Replaced {
                files: Vec::new(),
                replaced: 0,
            });
        }
        let re = to_regex(q)?;
        let files = match only {
            Some(f) => f,
            None => self.matching(q, &re).await?,
        };
        let mut changed = Vec::new();
        let mut replaced = 0;
        for f in files {
            if f.starts_with('/') || normalize(Path::new(&f)).starts_with("..") {
                continue;
            }
            let abs = self.root.join(&f);
            let Some(text) = read_text(&abs).await else {
                continue;
            };
            let mut n = 0;
            let next = re.replace_all(&text, |c: &Captures| {
                n += 1;
                if q.regex {
                    expand(replacement, c)
                } else {
                    replacement.to_owned()
                }
            });
            if n == 0 || next == text {
                continue;
            }
            let mode = tokio::fs::metadata(&abs).await?.permissions().mode();
            tokio::fs::write(&abs, next.as_bytes()).await?;
            let _ = tokio::fs::set_permissions(&abs, std::fs::Permissions::from_mode(mode)).await;
            changed.push(f);
            replaced += n;
        }
        Ok(Replaced {
            files: changed,
            replaced,
        })
    }

    /// The files a query matches in, all of them.
    async fn matching(&self, q: &TextQuery, re: &Regex) -> Result<Vec<String>> {
        if have_ripgrep().await {
            let args = Self::rg_args(
                q,
                &[
                    "--files-with-matches",
                    "--hidden",
                    "--glob",
                    "!.git",
                    "--max-filesize",
                    "8M",
                    "-0",
                ],
            )?;
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            match exec("rg", &args, &self.root).await {
                Ok(out) => {
                    return Ok(out
                        .split('\0')
                        .filter(|s| !s.is_empty())
                        .map(|p| p.strip_prefix("./").unwrap_or(p).to_owned())
                        .collect());
                }
                // 1 is no matches.
                Err(Some(1)) => return Ok(Vec::new()),
                Err(_) => {}
            }
        }
        let mut out = Vec::new();
        for f in self.in_scope(q).await? {
            if read_text(&self.root.join(&f))
                .await
                .is_some_and(|t| re.is_match(&t))
            {
                out.push(f);
            }
        }
        Ok(out)
    }

    async fn scan(&self, q: &TextQuery, re: &Regex) -> Result<TextResult> {
        let files = self.in_scope(q).await?;
        let mut matches = Vec::new();
        let deadline = Instant::now() + SEARCH_TIMEOUT;
        for f in files {
            if matches.len() >= MAX_MATCHES || Instant::now() > deadline {
                return Ok(TextResult {
                    matches,
                    truncated: true,
                });
            }
            let Some(text) = read_text(&self.root.join(&f)).await else {
                continue;
            };
            for (i, line) in text.split('\n').enumerate() {
                let line = line.strip_suffix('\r').unwrap_or(line);
                if !re.is_match(line) {
                    continue;
                }
                matches.push(cut(&f, i as u64 + 1, line, re));
                if matches.len() >= MAX_MATCHES {
                    return Ok(TextResult {
                        matches,
                        truncated: true,
                    });
                }
            }
        }
        Ok(TextResult {
            matches,
            truncated: false,
        })
    }
}

/// A folder to search in as the searches name it: "." for the root. One
/// outside the root is refused.
fn scope(dir: Option<&str>) -> Result<String> {
    let Some(dir) = dir.filter(|d| !d.is_empty()) else {
        return Ok(".".into());
    };
    let d = Path::new(dir)
        .components()
        .fold(PathBuf::new(), |mut p, c| {
            match c {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir if p.file_name().is_some() => {
                    p.pop();
                }
                c => p.push(c.as_os_str()),
            }
            p
        })
        .display()
        .to_string();
    let d = d.trim_end_matches('/');
    if d.starts_with('/') || d == ".." || d.starts_with("../") {
        return Err(Error::msg(format!("not a folder in this one: {dir}")));
    }
    Ok(if d.is_empty() {
        ".".into()
    } else {
        d.to_owned()
    })
}

/// The query as a regular expression, used to find where in each line it
/// matched.
fn to_regex(q: &TextQuery) -> Result<Regex> {
    let mut src = if q.regex {
        q.query.clone()
    } else {
        regex::escape(&q.query)
    };
    if q.whole_word {
        src = format!(r"\b(?:{src})\b");
    }
    RegexBuilder::new(&src)
        .case_insensitive(!q.case_sensitive)
        .build()
        .map_err(|e| Error::msg(format!("not a valid expression: {e}")))
}

/// A match with its line shortened to what is around the first match, and
/// the matches' places in what is left.
fn cut(path: &str, line: u64, full: &str, re: &Regex) -> Match {
    let units: Vec<u16> = full.encode_utf16().collect();
    let at = |byte: usize| full[..byte].encode_utf16().count();
    let mut ranges: Vec<[usize; 2]> = Vec::new();
    for m in re.find_iter(full) {
        if m.as_str().is_empty() {
            continue;
        }
        ranges.push([at(m.start()), at(m.end())]);
        if ranges.len() > 50 {
            break;
        }
    }
    let mut text: &[u16] = &units;
    let mut shift = 0;
    if text.len() > MAX_LINE {
        let first = ranges.first().map(|r| r[0]).unwrap_or(0);
        shift = first.saturating_sub(80).min(text.len() - MAX_LINE);
        text = &text[shift..shift + MAX_LINE];
    }
    let lead = text
        .iter()
        .take_while(|&&u| char::from_u32(u as u32).is_some_and(char::is_whitespace))
        .count();
    let kept = text.len() - lead;
    Match {
        path: path.to_owned(),
        line,
        text: String::from_utf16_lossy(&text[lead..]),
        ranges: ranges
            .into_iter()
            .filter_map(|[s, e]| {
                let (s, e) = (s.checked_sub(shift + lead)?, e.checked_sub(shift + lead)?);
                (e <= kept).then_some([s, e])
            })
            .collect(),
    }
}

/// A file mask as ripgrep's globs: each comma-separated pattern, "!"
/// leaving files out.
fn globs(mask: Option<&str>) -> Vec<String> {
    mask.unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|g| !g.is_empty())
        .map(str::to_owned)
        .collect()
}

/// A file mask as a test of a path, for the fallback search.
fn matcher(mask: Option<&str>) -> impl Fn(&str) -> bool {
    let gs = globs(mask);
    let to_re = |g: &str| {
        let mut src = String::from("(^|/)");
        let mut chars = g.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '*' if chars.peek() == Some(&'*') => {
                    chars.next();
                    src.push_str(".*");
                }
                '*' => src.push_str("[^/]*"),
                '?' => src.push_str("[^/]"),
                c => src.push_str(&regex::escape(&c.to_string())),
            }
        }
        src.push('$');
        Regex::new(&src).ok()
    };
    let inc: Vec<Regex> = gs
        .iter()
        .filter(|g| !g.starts_with('!'))
        .filter_map(|g| to_re(g))
        .collect();
    let exc: Vec<Regex> = gs
        .iter()
        .filter_map(|g| g.strip_prefix('!'))
        .filter_map(to_re)
        .collect();
    let any = !gs.is_empty();
    move |p: &str| {
        !any || ((inc.is_empty() || inc.iter().any(|r| r.is_match(p)))
            && !exc.iter().any(|r| r.is_match(p)))
    }
}

/// A replacement with a match's groups put in it: $1 to $99, $& for the
/// whole match, $$ for a dollar.
fn expand(replacement: &str, c: &Captures) -> String {
    let mut out = String::new();
    let b = replacement.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'$' && i + 1 < b.len() {
            let next = b[i + 1];
            if next == b'$' {
                out.push('$');
                i += 2;
                continue;
            }
            if next == b'&' {
                out.push_str(c.get(0).map_or("", |m| m.as_str()));
                i += 2;
                continue;
            }
            if next.is_ascii_digit() {
                let two = i + 2 < b.len() && b[i + 2].is_ascii_digit();
                let digits = &replacement[i + 1..i + if two { 3 } else { 2 }];
                let n: usize = digits.parse().unwrap_or(usize::MAX);
                if n < c.len() {
                    out.push_str(c.get(n).map_or("", |m| m.as_str()));
                    i += 1 + digits.len();
                    continue;
                }
                out.push_str(&replacement[i..i + 1 + digits.len()]);
                i += 1 + digits.len();
                continue;
            }
        }
        let ch = replacement[i..].chars().next().expect("in bounds");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// A file's text, or None for one too large or not text.
async fn read_text(abs: &Path) -> Option<String> {
    let m = tokio::fs::metadata(abs).await.ok()?;
    if !m.is_file() || m.len() > MAX_SEARCH_SIZE {
        return None;
    }
    let buf = tokio::fs::read(abs).await.ok()?;
    if buf[..buf.len().min(8000)].contains(&0) {
        return None;
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}
