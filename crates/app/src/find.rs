//! The Find tool window, where IntelliJ keeps Find in Files' results: text
//! searched for in the projects through the Code service's search.text
//! (ripgrep, else git grep, else reading the files), as you type, with
//! match case, whole words, regular expressions and a file mask. Results
//! are grouped by file; a match opens its file at the line. The search
//! itself starts in the pop-up (findpop.rs), which hands its results here.

use std::collections::HashSet;
use std::ops::Range;
use std::time::Duration;

use gpui::{
    Context, Entity, FocusHandle, Focusable, FontWeight, HighlightStyle, IntoElement, KeyDownEvent,
    ParentElement, Render, ScrollStrategy, SharedString, Styled, StyledText,
    UniformListScrollHandle, Window, div, prelude::*, px, relative, uniform_list,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{h_flex, v_flex};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::code::Code;
use crate::gitlog::chip;
use crate::palette::{self, Palette};
use crate::workspace::{Event, Folder, State, Workspace};

/// Match case, whole words, a regular expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Options {
    pub case: bool,
    pub word: bool,
    pub regex: bool,
}

/// Where a search looks: every project, one of them, or a folder in one,
/// from its root.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Scope {
    #[default]
    Projects,
    Project(String),
    Folder {
        root: String,
        dir: String,
    },
}

impl Scope {
    /// The projects searched, by root, each with the folder in it to search.
    pub fn targets(&self, roots: &[&str]) -> Vec<(String, Option<String>)> {
        roots
            .iter()
            .filter_map(|r| match self {
                Scope::Projects => Some((r.to_string(), None)),
                Scope::Project(root) if root == r => Some((r.to_string(), None)),
                Scope::Folder { root, dir } if root == r => {
                    Some((r.to_string(), Some(dir.clone())))
                }
                _ => None,
            })
            .collect()
    }

    /// The folder typed: from /, in the project holding it, else from the
    /// root of the project given.
    pub fn folder(typed: &str, roots: &[&str], current: Option<&str>) -> Result<Scope, String> {
        let typed = typed.trim().trim_end_matches('/');
        if typed.is_empty() {
            return Err("Type a folder to search in.".into());
        }
        if typed.starts_with('/') {
            let root = roots
                .iter()
                .filter(|r| {
                    let r = r.trim_end_matches('/');
                    typed == r || typed.starts_with(&format!("{r}/"))
                })
                .max_by_key(|r| r.len())
                .ok_or_else(|| format!("{typed} is in no project."))?;
            let dir = typed[root.trim_end_matches('/').len()..].trim_start_matches('/');
            return Ok(if dir.is_empty() {
                Scope::Project(root.to_string())
            } else {
                Scope::Folder {
                    root: root.to_string(),
                    dir: dir.to_string(),
                }
            });
        }
        let root = current
            .or(roots.first().copied())
            .ok_or("The environment has no projects.")?;
        Ok(Scope::Folder {
            root: root.to_string(),
            dir: typed.trim_start_matches("./").to_string(),
        })
    }
}

/// What to search for, and where.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Search {
    pub text: String,
    pub options: Options,
    /// Comma-separated globs, as IntelliJ's file mask: "*.rs, !*_test.go".
    pub mask: Option<String>,
    pub scope: Scope,
}

impl Search {
    /// search.text's parameters for a folder of a project, or the whole.
    pub fn params(&self, dir: Option<&str>) -> Value {
        let mut params = json!({
            "query": self.text,
            "regex": self.options.regex,
            "caseSensitive": self.options.case,
            "wholeWord": self.options.word,
        });
        if let Some(m) = self
            .mask
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
        {
            params["mask"] = m.into();
        }
        if let Some(d) = dir {
            params["dir"] = d.into();
        }
        params
    }

    /// Each project searched, with its connection and its call's parameters.
    pub fn calls(&self, projects: &[Folder]) -> Vec<(String, Code, Value)> {
        let roots: Vec<&str> = projects.iter().map(|f| f.root.as_str()).collect();
        self.scope
            .targets(&roots)
            .into_iter()
            .filter_map(|(root, dir)| {
                let code = projects.iter().find(|f| f.root == root)?.code.clone();
                Some((root, code, self.params(dir.as_deref())))
            })
            .collect()
    }
}

/// How a scope is named: "every project", a project's name, or a folder
/// in it.
pub fn scope_name(scope: &Scope, projects: &[Folder]) -> String {
    let name = |root: &str| {
        projects
            .iter()
            .find(|f| f.root == root)
            .map_or_else(|| root.to_string(), |f| f.name.clone())
    };
    match scope {
        Scope::Projects => "every project".into(),
        Scope::Project(root) => name(root),
        Scope::Folder { root, dir } => format!("{}/{dir}", name(root)),
    }
}

/// Searches a project, its matches or why it could not.
pub async fn search_text(code: Code, params: Value) -> Result<(Vec<Match>, bool), String> {
    let v = code.call("search.text", params).await?;
    let f: Found = serde_json::from_value(v).map_err(|e| e.to_string())?;
    Ok((f.matches, f.truncated))
}

/// A matching line, as search.text gives it: ranges are UTF-16 offsets
/// into text.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Match {
    pub path: String,
    pub line: u32,
    pub text: String,
    pub ranges: Vec<[usize; 2]>,
}

#[derive(Debug, Deserialize)]
struct Found {
    matches: Vec<Match>,
    truncated: bool,
}

/// One row of the results: a directory's heading (grouped by directory),
/// a file's heading, or one of its lines.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    Dir {
        root: String,
        /// From the project's root, "" for the root itself.
        dir: String,
        count: usize,
    },
    File {
        root: String,
        path: String,
        count: usize,
    },
    Line {
        root: String,
        m: Match,
    },
}

/// Groups each project's matches by file, in the projects' order, leaving
/// out a file a project before it found: a project can hold another, a
/// clone within its folder.
pub fn rows(projects: &[(String, Vec<Match>)]) -> Vec<Row> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (root, matches) in projects {
        let mut i = 0;
        while i < matches.len() {
            let path = &matches[i].path;
            let n = matches[i..].iter().take_while(|m| &m.path == path).count();
            let abs = format!("{}/{}", root.trim_end_matches('/'), path);
            if seen.insert(abs) {
                out.push(Row::File {
                    root: root.clone(),
                    path: path.clone(),
                    count: n,
                });
                for m in &matches[i..i + n] {
                    out.push(Row::Line {
                        root: root.clone(),
                        m: m.clone(),
                    });
                }
            }
            i += n;
        }
    }
    out
}

/// The rows grouped by directory, as IntelliJ's Group by Directory: each
/// project's directories by name, each heading its files, a file a
/// project before it found left out as `rows` does.
pub fn rows_by_dir(projects: &[(String, Vec<Match>)]) -> Vec<Row> {
    let flat = rows(projects);
    // Each file's rows, by project and directory.
    let mut groups: Vec<((String, String), Vec<Row>, usize)> = Vec::new();
    let mut i = 0;
    while i < flat.len() {
        let Row::File {
            root, path, count, ..
        } = &flat[i]
        else {
            i += 1;
            continue;
        };
        let dir = path.rsplit_once('/').map_or("", |(d, _)| d).to_string();
        let key = (root.clone(), dir);
        let n = 1 + flat[i + 1..]
            .iter()
            .take_while(|r| matches!(r, Row::Line { .. }))
            .count();
        let at = match groups.iter().position(|(k, ..)| *k == key) {
            Some(at) => at,
            None => {
                groups.push((key, vec![], 0));
                groups.len() - 1
            }
        };
        groups[at].1.extend(flat[i..i + n].iter().cloned());
        groups[at].2 += count;
        i += n;
    }
    let order: Vec<&String> = projects.iter().map(|(r, _)| r).collect();
    groups.sort_by(|a, b| {
        let at = |r: &String| order.iter().position(|o| *o == r);
        (at(&a.0.0), &a.0.1).cmp(&(at(&b.0.0), &b.0.1))
    });
    let mut out = Vec::new();
    for ((root, dir), rows, count) in groups {
        out.push(Row::Dir { root, dir, count });
        out.extend(rows);
    }
    out
}

/// What a row is folded by: a file by its path, a directory by its own
/// with a slash after, by root. Lines do not fold.
pub fn fold_key(row: &Row) -> Option<(String, String)> {
    match row {
        Row::Dir { root, dir, .. } => Some((root.clone(), format!("{dir}/"))),
        Row::File { root, path, .. } => Some((root.clone(), path.clone())),
        Row::Line { .. } => None,
    }
}

/// The rows shown, by index: a folded directory's or file's row without
/// what is under it.
pub fn visible(rows: &[Row], folded: &HashSet<(String, String)>) -> Vec<usize> {
    let mut out = vec![];
    let (mut hiding_dir, mut hiding) = (false, false);
    for (i, r) in rows.iter().enumerate() {
        let shut = fold_key(r).is_some_and(|k| folded.contains(&k));
        match r {
            Row::Dir { .. } => {
                (hiding_dir, hiding) = (shut, false);
                out.push(i);
            }
            Row::File { .. } if !hiding_dir => {
                hiding = shut;
                out.push(i);
            }
            Row::Line { .. } if !hiding_dir && !hiding => out.push(i),
            _ => {}
        }
    }
    out
}

/// The directory row a row is under, when grouped: itself, for a
/// directory's.
pub fn dir_of(rows: &[Row], i: usize) -> Option<usize> {
    (0..=i.min(rows.len().saturating_sub(1)))
        .rev()
        .find(|j| matches!(rows[*j], Row::Dir { .. }))
}

/// The match after a row (the first, after none), or before it (the last,
/// before none); None past the end. Folded files' matches count: IntelliJ's
/// Next Occurrence goes through every one.
pub fn next_match(rows: &[Row], from: Option<usize>, forward: bool) -> Option<usize> {
    let is_line = |i: &usize| matches!(rows[*i], Row::Line { .. });
    if forward {
        (from.map_or(0, |f| f + 1)..rows.len()).find(is_line)
    } else {
        (0..from.unwrap_or(rows.len()).min(rows.len()))
            .rev()
            .find(is_line)
    }
}

/// The file row a row is under: itself, for a file's; none for a
/// directory's.
pub fn file_of(rows: &[Row], i: usize) -> Option<usize> {
    if matches!(rows.get(i), Some(Row::Dir { .. })) {
        return None;
    }
    (0..=i.min(rows.len().saturating_sub(1)))
        .rev()
        .find(|j| matches!(rows[*j], Row::File { .. }))
}

/// UTF-16 ranges in text as byte ranges, for highlighting.
pub fn byte_ranges(text: &str, ranges: &[[usize; 2]]) -> Vec<Range<usize>> {
    let to_byte = |unit: usize| {
        let mut u = 0;
        for (b, c) in text.char_indices() {
            if u >= unit {
                return b;
            }
            u += c.len_utf16();
        }
        text.len()
    };
    ranges
        .iter()
        .map(|[s, e]| to_byte(*s)..to_byte(*e))
        .filter(|r| r.start < r.end)
        .collect()
}

/// A file's text as fs.read gives it: none for a file that is not text.
type Read = Result<Option<String>, String>;

/// Each project's matching lines, by its root.
pub type Lines = Vec<(String, Vec<Match>)>;

/// Asks for a file to be opened at a place in it: 0-based, the column in
/// UTF-16 units.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAt {
    pub root: String,
    pub path: String,
    pub line: u32,
    pub column: u32,
}

impl gpui::EventEmitter<OpenAt> for FindView {}

/// One tab of the window: a search's matches or a language server's
/// usages, with what is folded and chosen in them. A pinned tab is kept
/// when another search starts, as IntelliJ's pinned Find tabs are.
#[derive(Default)]
struct Results {
    /// What the rows are when they are a language server's answer rather
    /// than a search: "Usages of refresh".
    title: Option<String>,
    /// The search the rows answer, for the bar as the tab is shown again.
    search: Option<Search>,
    pinned: bool,
    /// Each project's matching lines, and the rows made of them, grouped
    /// by directory or not.
    found: Lines,
    by_dir: bool,
    rows: Vec<Row>,
    files: usize,
    matches: usize,
    truncated: bool,
    /// Projects still searching, and the errors of those that failed.
    pending: usize,
    errors: Vec<String>,
    /// Which search the tab shows, so an older one's answer is dropped.
    generation: u64,
    selected: Option<usize>,
    /// The files and directories folded (`fold_key`), and the rows shown
    /// for it.
    folded: HashSet<(String, String)>,
    shown: Vec<usize>,
    /// The selected match's file: its root, path, and text once read.
    preview: Option<(String, String, Option<Read>)>,
}

impl Results {
    /// Takes each project's lines as the rows, grouped by directory or not.
    fn set(&mut self, found: Lines, by_dir: bool) {
        self.found = found;
        self.regroup(by_dir);
    }

    /// Makes the rows again, grouped by directory or not, keeping the match
    /// chosen.
    fn regroup(&mut self, by_dir: bool) {
        let chosen = self.selected.and_then(|i| self.rows.get(i).cloned());
        self.by_dir = by_dir;
        self.rows = if by_dir {
            rows_by_dir(&self.found)
        } else {
            rows(&self.found)
        };
        self.files = self
            .rows
            .iter()
            .filter(|r| matches!(r, Row::File { .. }))
            .count();
        self.matches = self
            .rows
            .iter()
            .filter(|r| matches!(r, Row::Line { .. }))
            .count();
        self.selected = chosen.and_then(|c| self.rows.iter().position(|r| *r == c));
        self.shown = visible(&self.rows, &self.folded);
    }

    /// What the tab is called: the server's title, else the text searched.
    fn name(&self) -> String {
        match (&self.title, &self.search) {
            (Some(t), _) => t.clone(),
            (None, Some(s)) if !s.text.is_empty() => format!("'{}'", s.text),
            _ => "Find".into(),
        }
    }
}

pub struct FindView {
    workspace: Entity<Workspace>,
    query: Entity<InputState>,
    mask: Entity<InputState>,
    options: Options,
    scope: Scope,
    /// The tab shown, the window's tabs (the shown one's place holding an
    /// empty stand-in while it is here), and which place it is.
    cur: Results,
    tabs: Vec<Results>,
    active: usize,
    /// Counts the searches started, each tab's latest being its generation.
    next: u64,
    /// IntelliJ's Group by Directory.
    by_dir: bool,
    list: UniformListScrollHandle,
    list_focus: FocusHandle,
    /// Whether the selected match's file is shown beside the rows.
    previewing: bool,
    _subs: Vec<gpui::Subscription>,
}

impl FindView {
    pub fn new(workspace: Entity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Find in files"));
        let mask =
            cx.new(|cx| InputState::new(window, cx).placeholder("File mask: *.rs, !*_test.go"));
        let subs = vec![
            cx.subscribe(&query, |v, _, e: &InputEvent, cx| match e {
                InputEvent::Change => v.search_soon(cx),
                InputEvent::PressEnter { .. } => v.search(cx),
                _ => {}
            }),
            cx.subscribe(&mask, |v, _, e: &InputEvent, cx| match e {
                InputEvent::Change => v.search_soon(cx),
                InputEvent::PressEnter { .. } => v.search(cx),
                _ => {}
            }),
            cx.subscribe(&workspace, |v, _, e: &Event, cx| {
                if e == &Event::Projects && v.cur.title.is_none() && !v.cur.pinned {
                    v.search(cx);
                }
            }),
        ];
        FindView {
            workspace,
            query,
            mask,
            options: Options::default(),
            scope: Scope::Projects,
            cur: Results::default(),
            tabs: vec![Results::default()],
            active: 0,
            next: 0,
            by_dir: false,
            list: UniformListScrollHandle::new(),
            list_focus: cx.focus_handle(),
            previewing: true,
            _subs: subs,
        }
    }

    /// The rows shown, for the rows or the folded files having changed.
    fn reshown(&mut self) {
        self.cur.shown = visible(&self.cur.rows, &self.cur.folded);
    }

    /// Selects row i: scrolled to, its file previewed.
    pub fn select(&mut self, i: usize, cx: &mut Context<Self>) {
        if i >= self.cur.rows.len() {
            return;
        }
        self.cur.selected = Some(i);
        if let Some(at) = self.cur.shown.iter().position(|r| *r == i) {
            self.list.scroll_to_item(at, ScrollStrategy::Center);
        }
        self.read_preview(cx);
        cx.notify();
    }

    /// Reads the selected match's file for its preview, unless it is shown.
    fn read_preview(&mut self, cx: &mut Context<Self>) {
        let Some(Row::Line { root, m }) = self.cur.selected.and_then(|i| self.cur.rows.get(i))
        else {
            return;
        };
        if !self.previewing
            || self
                .cur
                .preview
                .as_ref()
                .is_some_and(|(r, p, _)| r == root && p == &m.path)
        {
            return;
        }
        let (root, path) = (root.clone(), m.path.clone());
        self.cur.preview = Some((root.clone(), path.clone(), None));
        let Some(code) = self.workspace.read(cx).code(&root) else {
            return;
        };
        let call = code.call("fs.read", json!({ "path": path }));
        cx.spawn(async move |this, cx| {
            let text = call
                .await
                .map(|v| v.get("text").and_then(|t| t.as_str()).map(str::to_string));
            let _ = this.update(cx, |v, cx| {
                if let Some((r, p, t)) = &mut v.cur.preview
                    && *r == root
                    && *p == path
                {
                    *t = Some(text);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Folds a directory's or file's rows away, or shows them.
    fn fold(&mut self, at: usize, fold: bool, cx: &mut Context<Self>) {
        let Some(key) = self.cur.rows.get(at).and_then(fold_key) else {
            return;
        };
        if fold {
            self.cur.folded.insert(key);
            let rows = &self.cur.rows;
            let under = |s: usize| file_of(rows, s) == Some(at) || dir_of(rows, s) == Some(at);
            if self.cur.selected.is_some_and(under) {
                self.cur.selected = Some(at);
            }
        } else {
            self.cur.folded.remove(&key);
        }
        self.reshown();
        cx.notify();
    }

    /// Opens the next match, or the one before (IntelliJ's Next and
    /// Previous Occurrence), unfolding its file; false when there is none.
    pub fn step(&mut self, forward: bool, cx: &mut Context<Self>) -> bool {
        let Some(i) = next_match(&self.cur.rows, self.cur.selected, forward) else {
            return false;
        };
        if let Some(f) = file_of(&self.cur.rows, i) {
            self.fold(f, false, cx);
        }
        if let Some(d) = dir_of(&self.cur.rows, i) {
            self.fold(d, false, cx);
        }
        self.open(i, cx);
        true
    }

    /// The list's keys: up and down through the rows shown, Enter opening a
    /// match or folding a file, left folding and right unfolding.
    fn key(&mut self, e: &KeyDownEvent, cx: &mut Context<Self>) {
        let at = self
            .cur
            .selected
            .and_then(|s| self.cur.shown.iter().position(|r| *r == s));
        match e.keystroke.key.as_str() {
            "down" => {
                let next = at.map_or(0, |a| (a + 1).min(self.cur.shown.len().saturating_sub(1)));
                if let Some(&i) = self.cur.shown.get(next) {
                    self.select(i, cx);
                }
            }
            "up" => {
                let next = at.map_or(0, |a| a.saturating_sub(1));
                if let Some(&i) = self.cur.shown.get(next) {
                    self.select(i, cx);
                }
            }
            "enter" => match self.cur.selected.map(|s| (s, self.cur.rows.get(s))) {
                Some((s, Some(Row::Line { .. }))) => self.open(s, cx),
                Some((s, Some(row))) => {
                    let folded = fold_key(row).is_some_and(|k| self.cur.folded.contains(&k));
                    self.fold(s, !folded, cx);
                }
                _ => return,
            },
            // Left folds what is open, else goes up to what holds it.
            "left" => match self.cur.selected {
                Some(s) => {
                    let row = &self.cur.rows[s];
                    let open = fold_key(row).is_some_and(|k| !self.cur.folded.contains(&k));
                    let up = match row {
                        Row::Line { .. } => file_of(&self.cur.rows, s),
                        Row::File { .. } => dir_of(&self.cur.rows, s),
                        Row::Dir { .. } => None,
                    };
                    match (open, up) {
                        (true, _) => self.fold(s, true, cx),
                        (false, Some(u)) => self.select(u, cx),
                        (false, None) => return,
                    }
                }
                None => return,
            },
            "right" => match self.cur.selected {
                Some(s) if fold_key(&self.cur.rows[s]).is_some() => self.fold(s, false, cx),
                _ => return,
            },
            _ => return,
        }
        cx.stop_propagation();
    }

    pub fn query(&self) -> &Entity<InputState> {
        &self.query
    }

    pub fn rows(&self) -> &[Row] {
        &self.cur.rows
    }

    /// Lists a language server's usages of a symbol: each project's lines,
    /// by root, under title.
    pub fn show_usages(&mut self, title: String, found: Lines, cx: &mut Context<Self>) {
        self.fresh();
        self.renew();
        self.clear();
        self.cur.title = Some(title);
        self.cur.search = None;
        self.cur.set(found, self.by_dir);
        self.select_first(cx);
        cx.notify();
    }

    /// A new generation for the tab shown, so its older search's answers
    /// are dropped.
    fn renew(&mut self) -> u64 {
        self.next += 1;
        self.cur.generation = self.next;
        self.next
    }

    /// Empties the tab shown for a new search.
    fn clear(&mut self) {
        let keep = (self.cur.generation, self.cur.pinned);
        self.cur = Results {
            generation: keep.0,
            pinned: keep.1,
            by_dir: self.by_dir,
            ..Default::default()
        };
    }

    /// Makes the tab shown one a new search may take: itself, unless it is
    /// pinned; else the tab not pinned, else a new one.
    fn fresh(&mut self) {
        if !self.cur.pinned {
            return;
        }
        let free = (0..self.tabs.len()).find(|&j| j != self.active && !self.tabs[j].pinned);
        let j = free.unwrap_or_else(|| {
            self.tabs.push(Results::default());
            self.tabs.len() - 1
        });
        self.switch(j);
    }

    /// Shows tab j, its stand-in taking its place in the list.
    fn switch(&mut self, j: usize) {
        if j == self.active || j >= self.tabs.len() {
            return;
        }
        std::mem::swap(&mut self.cur, &mut self.tabs[self.active]);
        self.active = j;
        std::mem::swap(&mut self.cur, &mut self.tabs[j]);
        if self.cur.by_dir != self.by_dir {
            self.cur.regroup(self.by_dir);
        }
    }

    /// Shows tab j, its search back in the bar.
    fn show_tab(&mut self, j: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.switch(j);
        if let Some(s) = self.cur.search.clone() {
            self.options = s.options;
            self.scope = s.scope;
            let mask = s.mask.unwrap_or_default();
            self.query
                .update(cx, |q, cx| q.set_value(s.text, window, cx));
            self.mask.update(cx, |m, cx| m.set_value(mask, window, cx));
        }
        if let Some(at) = self
            .cur
            .selected
            .and_then(|s| self.cur.shown.iter().position(|r| *r == s))
        {
            self.list.scroll_to_item(at, ScrollStrategy::Center);
        }
        cx.notify();
    }

    /// Closes tab j; the last closed leaves an empty one.
    fn close_tab(&mut self, j: usize, window: &mut Window, cx: &mut Context<Self>) {
        if j >= self.tabs.len() {
            return;
        }
        if self.tabs.len() == 1 {
            self.renew();
            self.clear();
            self.cur.pinned = false;
        } else if j == self.active {
            self.tabs.remove(j);
            let k = j.min(self.tabs.len() - 1);
            self.cur = std::mem::take(&mut self.tabs[k]);
            self.active = k;
            self.show_tab(k, window, cx);
        } else {
            self.tabs.remove(j);
            if j < self.active {
                self.active -= 1;
            }
        }
        cx.notify();
    }

    /// Pins the tab shown, or unpins it.
    fn pin(&mut self, cx: &mut Context<Self>) {
        self.cur.pinned = !self.cur.pinned;
        cx.notify();
    }

    /// IntelliJ's Group by Directory, for every tab.
    fn group(&mut self, cx: &mut Context<Self>) {
        self.by_dir = !self.by_dir;
        self.cur.regroup(self.by_dir);
        cx.notify();
    }

    /// Selects the first match, as IntelliJ does when results come, for
    /// the preview to show it.
    fn select_first(&mut self, cx: &mut Context<Self>) {
        if self.cur.selected.is_none()
            && let Some(i) = next_match(&self.cur.rows, None, true)
        {
            self.select(i, cx);
        }
    }

    /// Lists a search's matches here, as the pop-up's Open in Find Window.
    pub fn show(&mut self, s: Search, window: &mut Window, cx: &mut Context<Self>) {
        self.fresh();
        self.cur.title = None;
        self.options = s.options;
        self.scope = s.scope;
        let mask = s.mask.unwrap_or_default();
        self.query
            .update(cx, |q, cx| q.set_value(s.text, window, cx));
        self.mask.update(cx, |m, cx| m.set_value(mask, window, cx));
        self.search(cx);
    }

    /// The search as it stands.
    fn current(&self, cx: &gpui::App) -> Search {
        Search {
            text: self.query.read(cx).value().to_string(),
            options: self.options,
            mask: Some(self.mask.read(cx).value().trim().to_string()),
            scope: self.scope.clone(),
        }
    }

    /// Searches once typing pauses, unless the bar holds the tab's own
    /// search (shown again).
    fn search_soon(&mut self, cx: &mut Context<Self>) {
        if self.cur.title.is_none() && self.cur.search.as_ref() == Some(&self.current(cx)) {
            return;
        }
        self.fresh();
        let generation = self.renew();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            let _ = this.update(cx, |v, cx| {
                if v.cur.generation == generation {
                    v.search(cx);
                }
            });
        })
        .detach();
    }

    /// Searches for the query, as it is now.
    pub fn search(&mut self, cx: &mut Context<Self>) {
        self.fresh();
        let generation = self.renew();
        let search = self.current(cx);
        self.clear();
        self.cur.search = Some(search.clone());
        if search.text.is_empty() {
            cx.notify();
            return;
        }
        let calls = search.calls(self.workspace.read(cx).projects());
        self.cur.pending = calls.len();
        let calls: Vec<_> = calls
            .into_iter()
            .map(|(root, code, params)| (root, search_text(code, params)))
            .collect();
        cx.spawn(async move |this, cx| {
            let mut found: Vec<(String, Vec<Match>)> = Vec::new();
            for (root, call) in calls {
                let result = call.await;
                let keep = this.update(cx, |v, cx| {
                    // The search's tab may not be the one shown: the user can switch.
                    let shown = v.cur.generation == generation;
                    let by_dir = v.by_dir;
                    let Some(r) = std::iter::once(&mut v.cur)
                        .chain(v.tabs.iter_mut())
                        .find(|t| t.generation == generation)
                    else {
                        return false;
                    };
                    r.pending -= 1;
                    match result {
                        Ok((matches, truncated)) => {
                            r.truncated |= truncated;
                            found.push((root, matches));
                            r.set(found.clone(), by_dir);
                            if shown {
                                v.select_first(cx);
                            }
                        }
                        Err(e) => r.errors.push(e),
                    }
                    cx.notify();
                    true
                });
                if !matches!(keep, Ok(true)) {
                    return;
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn toggle(&mut self, which: fn(&mut Options) -> &mut bool, cx: &mut Context<Self>) {
        let o = which(&mut self.options);
        *o = !*o;
        self.search(cx);
    }

    /// Opens the match on row i.
    pub fn open(&mut self, i: usize, cx: &mut Context<Self>) {
        self.select(i, cx);
        if let Some(Row::Line { root, m }) = self.cur.rows.get(i) {
            cx.emit(OpenAt {
                root: root.clone(),
                path: m.path.clone(),
                line: m.line.saturating_sub(1),
                column: m.ranges.first().map_or(0, |r| r[0] as u32),
            });
        }
        cx.notify();
    }

    /// The project's name, when there are several to tell apart.
    fn project_name(&self, root: &str, cx: &gpui::App) -> Option<String> {
        let ws = self.workspace.read(cx);
        (ws.projects().len() > 1)
            .then(|| ws.projects().iter().find(|f| f.root == root))
            .flatten()
            .map(|f| f.name.clone())
    }

    fn row(&self, i: usize, p: &Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        let grouped = self.cur.by_dir;
        match &self.cur.rows[i] {
            Row::Dir { root, dir, count } => {
                let folded =
                    fold_key(&self.cur.rows[i]).is_some_and(|k| self.cur.folded.contains(&k));
                let selected = self.cur.selected == Some(i);
                let root = root.trim_end_matches('/');
                let name = self.workspace.read(cx).shown(&if dir.is_empty() {
                    root.to_string()
                } else {
                    format!("{root}/{dir}")
                });
                h_flex()
                    .id(("find-dir", i))
                    .h(px(ROW))
                    .w_full()
                    .gap(px(6.))
                    .px(px(8.))
                    .cursor_pointer()
                    .when(selected, |d| d.bg(p.selection))
                    .when(!selected, |d| d.hover(|s| s.bg(p.surface_2)))
                    .on_click(cx.listener(move |v, _, window, cx| {
                        window.focus(&v.list_focus, cx);
                        v.select(i, cx);
                        v.fold(i, !folded, cx);
                    }))
                    .child(crate::project::chevron(!folded, p))
                    .child(
                        gpui::svg()
                            .path(crate::assets::icon("folder"))
                            .size(px(13.))
                            .flex_none()
                            .text_color(p.muted),
                    )
                    .child(div().min_w_0().truncate().child(name))
                    .child(div().text_color(p.muted).child(count.to_string()))
                    .into_any_element()
            }
            Row::File { root, path, count } => {
                let (name, dir) = match path.rsplit_once('/') {
                    Some((d, n)) => (n.to_string(), d.to_string()),
                    None => (path.clone(), String::new()),
                };
                let project = self.project_name(root, cx).unwrap_or_default();
                let several = !project.is_empty();
                let dir = if grouped { String::new() } else { dir };
                let folded = self.cur.folded.contains(&(root.clone(), path.clone()));
                let selected = self.cur.selected == Some(i);
                h_flex()
                    .id(("find-file", i))
                    .h(px(ROW))
                    .w_full()
                    .gap(px(6.))
                    .pr(px(8.))
                    .pl(px(if grouped { 20. } else { 8. }))
                    .cursor_pointer()
                    .when(selected, |d| d.bg(p.selection))
                    .when(!selected, |d| d.hover(|s| s.bg(p.surface_2)))
                    .on_click(cx.listener(move |v, _, window, cx| {
                        window.focus(&v.list_focus, cx);
                        v.select(i, cx);
                        v.fold(i, !folded, cx);
                    }))
                    .child(crate::project::chevron(!folded, p))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(name))
                    .child(div().min_w_0().truncate().text_color(p.muted).child(
                        if several && !grouped {
                            if dir.is_empty() {
                                project
                            } else {
                                format!("{project}/{dir}")
                            }
                        } else {
                            dir
                        },
                    ))
                    .child(div().text_color(p.muted).child(count.to_string()))
                    .into_any_element()
            }
            Row::Line { m, .. } => {
                let selected = self.cur.selected == Some(i);
                let lead = m.text.len() - m.text.trim_start().len();
                let text = m.text[lead..].trim_end().replace('\t', "    ");
                let ranges: Vec<Range<usize>> = byte_ranges(&m.text, &m.ranges)
                    .into_iter()
                    .filter(|r| r.start >= lead && r.end <= lead + text.len())
                    .map(|r| r.start - lead..r.end - lead)
                    .collect();
                let highlight = HighlightStyle {
                    background_color: Some(p.wait.opacity(0.3)),
                    font_weight: Some(FontWeight::SEMIBOLD),
                    ..Default::default()
                };
                let styled = StyledText::new(SharedString::from(text.clone()))
                    .with_highlights(ranges.into_iter().map(|r| (r, highlight)));
                h_flex()
                    .id(("find-line", i))
                    .h(px(ROW))
                    .w_full()
                    .gap(px(8.))
                    .pl(px(if grouped { 32. } else { 20. }))
                    .pr(px(8.))
                    .cursor_pointer()
                    .when(selected, |d| d.bg(p.selection))
                    .when(!selected, |d| d.hover(|s| s.bg(p.surface_2)))
                    .child(
                        div()
                            .w(px(36.))
                            .flex_none()
                            .text_right()
                            .text_color(p.muted)
                            .child(m.line.to_string()),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .font_family(mono())
                            .text_size(px(11.5))
                            .child(styled),
                    )
                    .on_click(cx.listener(move |v, _, window, cx| {
                        window.focus(&v.list_focus, cx);
                        v.open(i, cx)
                    }))
                    .into_any_element()
            }
        }
    }

    /// IntelliJ's Group by Directory, as a toggle.
    fn group_chip(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        chip("find-group", "Folders".into(), self.by_dir, p)
            .tooltip(|window, cx| {
                gpui_component::tooltip::Tooltip::new("Group by directory").build(window, cx)
            })
            .on_click(cx.listener(|v, _, _, cx| v.group(cx)))
    }

    /// Pins the tab, as IntelliJ's Pin Tab: a new search then opens a tab
    /// of its own and these results stay.
    fn pin_chip(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let pinned = self.cur.pinned;
        chip("find-pin", "Pin".into(), pinned, p)
            .tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(if pinned {
                    "Unpin the tab: the next search replaces these results"
                } else {
                    "Pin the tab: the next search opens another"
                })
                .build(window, cx)
            })
            .on_click(cx.listener(|v, _, _, cx| v.pin(cx)))
    }

    /// The window's tabs, each a search's or the server's results, pinned
    /// ones marked; a click shows one, its cross closes it.
    fn tab_strip(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let mut strip = h_flex()
            .id("find-tabs")
            .flex_none()
            .h(px(26.))
            .px(px(4.))
            .gap(px(2.))
            .overflow_x_scroll()
            .border_b_1()
            .border_color(p.border);
        for j in 0..self.tabs.len() {
            let on = j == self.active;
            let tab = if on { &self.cur } else { &self.tabs[j] };
            strip = strip.child(
                h_flex()
                    .id(("find-tab", j))
                    .debug_selector(move || format!("find-tab-{j}"))
                    .flex_none()
                    .h(px(22.))
                    .px(px(8.))
                    .gap(px(5.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .when(on, |d| d.bg(p.selection))
                    .when(!on, |d| d.hover(|s| s.bg(p.surface_2)))
                    .when(tab.pinned, |d| {
                        d.child(
                            gpui::svg()
                                .path(crate::assets::icon("pin"))
                                .size(px(11.))
                                .text_color(p.muted),
                        )
                    })
                    .child(div().max_w(px(220.)).truncate().child(tab.name()))
                    .child(
                        div()
                            .id(("find-tab-close", j))
                            .px(px(2.))
                            .text_color(p.muted)
                            .hover(|s| s.text_color(p.text))
                            .child("×")
                            .on_click(cx.listener(move |v, _, window, cx| {
                                cx.stop_propagation();
                                v.close_tab(j, window, cx);
                            })),
                    )
                    .on_click(cx.listener(move |v, _, window, cx| v.show_tab(j, window, cx))),
            );
        }
        strip
    }

    /// The selected match's file around it, beside the rows.
    fn preview_pane(&self, p: &Palette) -> gpui::AnyElement {
        let frame = v_flex()
            .w(relative(0.45))
            .flex_none()
            .h_full()
            .border_l_1()
            .border_color(p.border)
            .bg(p.bg);
        let selected = self.cur.selected.and_then(|i| match self.cur.rows.get(i) {
            Some(Row::Line { m, .. }) => Some(m),
            _ => None,
        });
        let (Some((_, path, text)), Some(m)) = (&self.cur.preview, selected) else {
            return frame
                .child(note("Select a match to see it in its file.", p))
                .into_any_element();
        };
        let body = match text {
            None => note("Reading…", p),
            Some(Err(e)) => note(e.clone(), p),
            Some(Ok(None)) => note("The file cannot be shown.", p),
            Some(Ok(Some(text))) => crate::findpop::lines_around(text, m, 41, p),
        };
        frame
            .child(
                div()
                    .px(px(10.))
                    .py(px(4.))
                    .text_size(px(11.))
                    .text_color(p.muted)
                    .truncate()
                    .child(path.clone()),
            )
            .child(div().flex_1().min_h_0().overflow_hidden().child(body))
            .into_any_element()
    }
}

const ROW: f32 = 20.;

fn mono() -> &'static str {
    if cfg!(target_os = "macos") {
        "Menlo"
    } else if cfg!(target_os = "windows") {
        "Consolas"
    } else {
        "DejaVu Sans Mono"
    }
}

impl Focusable for FindView {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        self.query.read(cx).focus_handle(cx)
    }
}

impl Render for FindView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let o = self.options;
        let toggle = |id: &'static str, label: &'static str, on: bool, tip: &'static str| {
            chip(id, label.to_string(), on, &p).tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(tip).build(window, cx)
            })
        };
        let status = if self.query.read(cx).value().is_empty() {
            String::new()
        } else if self.cur.pending > 0 && self.cur.rows.is_empty() {
            "Searching…".into()
        } else {
            let files = if self.cur.files == 1 { "file" } else { "files" };
            let matches = if self.cur.matches == 1 {
                "match"
            } else {
                "matches"
            };
            let mut s = format!(
                "{} {matches} in {} {files}",
                self.cur.matches, self.cur.files
            );
            if self.cur.truncated {
                s.push_str(", the first found");
            }
            s
        };
        let bar = h_flex()
            .flex_none()
            .gap(px(6.))
            .px(px(8.))
            .py(px(5.))
            .child(
                div()
                    .flex_1()
                    .min_w(px(160.))
                    .child(Input::new(&self.query)),
            )
            .child(
                toggle("find-case", "Cc", o.case, "Match case")
                    .on_click(cx.listener(|v, _, _, cx| v.toggle(|o| &mut o.case, cx))),
            )
            .child(
                toggle("find-word", "W", o.word, "Whole words")
                    .on_click(cx.listener(|v, _, _, cx| v.toggle(|o| &mut o.word, cx))),
            )
            .child(
                toggle("find-regex", ".*", o.regex, "Regular expression")
                    .on_click(cx.listener(|v, _, _, cx| v.toggle(|o| &mut o.regex, cx))),
            )
            .when(self.scope != Scope::Projects, |d| {
                let name = scope_name(&self.scope, self.workspace.read(cx).projects());
                d.child(
                    chip("find-scope", format!("in {name} ✕"), true, &p)
                        .tooltip(|window, cx| {
                            gpui_component::tooltip::Tooltip::new("Search every project")
                                .build(window, cx)
                        })
                        .on_click(cx.listener(|v, _, _, cx| {
                            v.scope = Scope::Projects;
                            v.search(cx);
                        })),
                )
            })
            .child(div().w(px(200.)).flex_none().child(Input::new(&self.mask)))
            .child(
                toggle(
                    "find-preview",
                    "Preview",
                    self.previewing,
                    "Show the match in its file",
                )
                .on_click(cx.listener(|v, _, _, cx| {
                    v.previewing = !v.previewing;
                    v.read_preview(cx);
                    cx.notify();
                })),
            )
            .child(self.group_chip(&p, cx))
            .child(self.pin_chip(&p, cx))
            .child(
                div()
                    .w(px(190.))
                    .flex_none()
                    .truncate()
                    .text_size(px(11.))
                    .text_color(p.muted)
                    .child(status),
            );
        let state = self.workspace.read(cx).state().clone();
        let body: gpui::AnyElement = match state {
            State::Connecting => note("Connecting…", &p),
            State::Reconnecting(why) => note(State::reconnecting(&why), &p),
            State::Failed(e) => note(format!("Could not reach the Code service: {e}"), &p),
            State::Ready if !self.cur.errors.is_empty() && self.cur.rows.is_empty() => {
                note(self.cur.errors.join("\n"), &p)
            }
            State::Ready if self.cur.rows.is_empty() => note(
                if self.cur.title.is_some() {
                    "No usages found."
                } else if self.query.read(cx).value().is_empty() {
                    "Type to search the environment's projects."
                } else if self.cur.pending > 0 {
                    ""
                } else {
                    "Nothing found."
                },
                &p,
            ),
            State::Ready => h_flex()
                .flex_1()
                .min_h_0()
                .items_stretch()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .track_focus(&self.list_focus)
                        .on_key_down(cx.listener(|v, e: &KeyDownEvent, _, cx| v.key(e, cx)))
                        .child(
                            uniform_list(
                                "find-results",
                                self.cur.shown.len(),
                                cx.processor(|v, range: Range<usize>, _, cx| {
                                    let p = palette::of(cx);
                                    range
                                        .filter_map(|at| v.cur.shown.get(at).copied())
                                        .map(|i| v.row(i, &p, cx))
                                        .collect::<Vec<_>>()
                                }),
                            )
                            .track_scroll(&self.list)
                            .flex_1(),
                        ),
                )
                .when(self.previewing, |d| d.child(self.preview_pane(&p)))
                .into_any_element(),
        };
        v_flex()
            .size_full()
            .min_h_0()
            .text_size(px(12.))
            .when(self.tabs.len() > 1, |d| d.child(self.tab_strip(&p, cx)))
            .child(match self.cur.title.clone() {
                // A language server's answer: what it is, and the way back
                // to searching.
                Some(title) => {
                    h_flex()
                        .flex_none()
                        .gap(px(8.))
                        .px(px(10.))
                        .py(px(6.))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                        .child(div().text_color(p.muted).child(format!(
                            "{} in {} file{}",
                            self.cur.matches,
                            self.cur.files,
                            if self.cur.files == 1 { "" } else { "s" }
                        )))
                        .child(div().flex_1())
                        .child(self.group_chip(&p, cx))
                        .child(self.pin_chip(&p, cx))
                        .child(chip("find-back", "Search ✕".into(), false, &p).on_click(
                            cx.listener(|v, _, _, cx| {
                                v.fresh();
                                v.cur.title = None;
                                v.search(cx);
                            }),
                        ))
                        .into_any_element()
                }
                None => bar.into_any_element(),
            })
            .child(v_flex().flex_1().min_h_0().child(body))
    }
}

fn note(text: impl Into<SharedString>, p: &Palette) -> gpui::AnyElement {
    div()
        .px(px(10.))
        .py(px(6.))
        .text_size(px(11.5))
        .text_color(p.muted)
        .child(text.into())
        .into_any_element()
}
