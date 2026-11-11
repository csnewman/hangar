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

/// One row of the results: a file's heading, or one of its lines.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
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

/// The rows shown, by index: a folded file's row without its lines.
pub fn visible(rows: &[Row], folded: &HashSet<(String, String)>) -> Vec<usize> {
    let mut out = vec![];
    let mut hiding = false;
    for (i, r) in rows.iter().enumerate() {
        match r {
            Row::File { root, path, .. } => {
                hiding = folded.contains(&(root.clone(), path.clone()));
                out.push(i);
            }
            Row::Line { .. } if !hiding => out.push(i),
            Row::Line { .. } => {}
        }
    }
    out
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

/// The file row a row is under: itself, for a file's.
pub fn file_of(rows: &[Row], i: usize) -> Option<usize> {
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

pub struct FindView {
    workspace: Entity<Workspace>,
    /// What the rows are when they are a language server's answer rather
    /// than a search: "Usages of refresh".
    title: Option<String>,
    query: Entity<InputState>,
    mask: Entity<InputState>,
    options: Options,
    scope: Scope,
    rows: Vec<Row>,
    files: usize,
    matches: usize,
    truncated: bool,
    /// Projects still searching, and the errors of those that failed.
    pending: usize,
    errors: Vec<String>,
    /// Counts the searches started, so an older one's answer is dropped.
    generation: u64,
    selected: Option<usize>,
    /// The files folded, by root and path, and the rows shown for it.
    folded: HashSet<(String, String)>,
    shown: Vec<usize>,
    list: UniformListScrollHandle,
    list_focus: FocusHandle,
    /// Whether the selected match's file is shown beside the rows, and
    /// that file: its root, path, and text once read.
    previewing: bool,
    preview: Option<(String, String, Option<Read>)>,
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
                if e == &Event::Projects {
                    v.search(cx);
                }
            }),
        ];
        FindView {
            workspace,
            title: None,
            query,
            mask,
            options: Options::default(),
            scope: Scope::Projects,
            rows: vec![],
            files: 0,
            matches: 0,
            truncated: false,
            pending: 0,
            errors: vec![],
            generation: 0,
            selected: None,
            folded: HashSet::new(),
            shown: vec![],
            list: UniformListScrollHandle::new(),
            list_focus: cx.focus_handle(),
            previewing: true,
            preview: None,
            _subs: subs,
        }
    }

    /// The rows shown, for the rows or the folded files having changed.
    fn reshown(&mut self) {
        self.shown = visible(&self.rows, &self.folded);
    }

    /// Selects row i: scrolled to, its file previewed.
    pub fn select(&mut self, i: usize, cx: &mut Context<Self>) {
        if i >= self.rows.len() {
            return;
        }
        self.selected = Some(i);
        if let Some(at) = self.shown.iter().position(|r| *r == i) {
            self.list.scroll_to_item(at, ScrollStrategy::Center);
        }
        self.read_preview(cx);
        cx.notify();
    }

    /// Reads the selected match's file for its preview, unless it is shown.
    fn read_preview(&mut self, cx: &mut Context<Self>) {
        let Some(Row::Line { root, m }) = self.selected.and_then(|i| self.rows.get(i)) else {
            return;
        };
        if !self.previewing
            || self
                .preview
                .as_ref()
                .is_some_and(|(r, p, _)| r == root && p == &m.path)
        {
            return;
        }
        let (root, path) = (root.clone(), m.path.clone());
        self.preview = Some((root.clone(), path.clone(), None));
        let Some(code) = self.workspace.read(cx).code(&root) else {
            return;
        };
        let call = code.call("fs.read", json!({ "path": path }));
        cx.spawn(async move |this, cx| {
            let text = call
                .await
                .map(|v| v.get("text").and_then(|t| t.as_str()).map(str::to_string));
            let _ = this.update(cx, |v, cx| {
                if let Some((r, p, t)) = &mut v.preview
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

    /// Folds a file's lines away, or shows them.
    fn fold(&mut self, file: usize, fold: bool, cx: &mut Context<Self>) {
        let Some(Row::File { root, path, .. }) = self.rows.get(file) else {
            return;
        };
        let key = (root.clone(), path.clone());
        if fold {
            self.folded.insert(key);
            if self
                .selected
                .is_some_and(|s| file_of(&self.rows, s) == Some(file))
            {
                self.selected = Some(file);
            }
        } else {
            self.folded.remove(&key);
        }
        self.reshown();
        cx.notify();
    }

    /// Opens the next match, or the one before (IntelliJ's Next and
    /// Previous Occurrence), unfolding its file; false when there is none.
    pub fn step(&mut self, forward: bool, cx: &mut Context<Self>) -> bool {
        let Some(i) = next_match(&self.rows, self.selected, forward) else {
            return false;
        };
        if let Some(f) = file_of(&self.rows, i) {
            self.fold(f, false, cx);
        }
        self.open(i, cx);
        true
    }

    /// The list's keys: up and down through the rows shown, Enter opening a
    /// match or folding a file, left folding and right unfolding.
    fn key(&mut self, e: &KeyDownEvent, cx: &mut Context<Self>) {
        let at = self
            .selected
            .and_then(|s| self.shown.iter().position(|r| *r == s));
        match e.keystroke.key.as_str() {
            "down" => {
                let next = at.map_or(0, |a| (a + 1).min(self.shown.len().saturating_sub(1)));
                if let Some(&i) = self.shown.get(next) {
                    self.select(i, cx);
                }
            }
            "up" => {
                let next = at.map_or(0, |a| a.saturating_sub(1));
                if let Some(&i) = self.shown.get(next) {
                    self.select(i, cx);
                }
            }
            "enter" => match self.selected.map(|s| (s, self.rows.get(s))) {
                Some((s, Some(Row::Line { .. }))) => self.open(s, cx),
                Some((s, Some(Row::File { root, path, .. }))) => {
                    let folded = self.folded.contains(&(root.clone(), path.clone()));
                    self.fold(s, !folded, cx);
                }
                _ => return,
            },
            "left" => match self.selected {
                Some(s) if matches!(self.rows.get(s), Some(Row::File { .. })) => {
                    self.fold(s, true, cx)
                }
                Some(s) => {
                    if let Some(f) = file_of(&self.rows, s) {
                        self.select(f, cx)
                    }
                }
                None => return,
            },
            "right" => match self.selected {
                Some(s) if matches!(self.rows.get(s), Some(Row::File { .. })) => {
                    self.fold(s, false, cx)
                }
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
        &self.rows
    }

    /// Lists a language server's usages of a symbol: each project's lines,
    /// by root, under title.
    pub fn show_usages(&mut self, title: String, found: Lines, cx: &mut Context<Self>) {
        self.generation += 1;
        self.title = Some(title);
        self.rows = rows(&found);
        self.files = self
            .rows
            .iter()
            .filter(|r| matches!(r, Row::File { .. }))
            .count();
        self.matches = self.rows.len() - self.files;
        self.truncated = false;
        self.pending = 0;
        self.errors.clear();
        self.selected = None;
        self.folded.clear();
        self.preview = None;
        self.reshown();
        self.select_first(cx);
        cx.notify();
    }

    /// Selects the first match, as IntelliJ does when results come, for
    /// the preview to show it.
    fn select_first(&mut self, cx: &mut Context<Self>) {
        if self.selected.is_none()
            && let Some(i) = next_match(&self.rows, None, true)
        {
            self.select(i, cx);
        }
    }

    /// Lists a search's matches here, as the pop-up's Open in Find Window.
    pub fn show(&mut self, s: Search, window: &mut Window, cx: &mut Context<Self>) {
        self.title = None;
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

    /// Searches once typing pauses.
    fn search_soon(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            let _ = this.update(cx, |v, cx| {
                if v.generation == generation {
                    v.search(cx);
                }
            });
        })
        .detach();
    }

    /// Searches for the query, as it is now.
    pub fn search(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        let search = self.current(cx);
        self.rows.clear();
        self.files = 0;
        self.matches = 0;
        self.truncated = false;
        self.errors.clear();
        self.selected = None;
        self.folded.clear();
        self.preview = None;
        self.reshown();
        self.pending = 0;
        if search.text.is_empty() {
            cx.notify();
            return;
        }
        let calls = search.calls(self.workspace.read(cx).projects());
        self.pending = calls.len();
        let calls: Vec<_> = calls
            .into_iter()
            .map(|(root, code, params)| (root, search_text(code, params)))
            .collect();
        cx.spawn(async move |this, cx| {
            let mut found: Vec<(String, Vec<Match>)> = Vec::new();
            for (root, call) in calls {
                let result = call.await;
                let keep = this.update(cx, |v, cx| {
                    if v.generation != generation {
                        return false;
                    }
                    v.pending -= 1;
                    match result {
                        Ok((matches, truncated)) => {
                            v.truncated |= truncated;
                            found.push((root, matches));
                            v.rows = rows(&found);
                            v.files = v
                                .rows
                                .iter()
                                .filter(|r| matches!(r, Row::File { .. }))
                                .count();
                            v.matches = v.rows.len() - v.files;
                            v.reshown();
                            v.select_first(cx);
                        }
                        Err(e) => v.errors.push(e),
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
        if let Some(Row::Line { root, m }) = self.rows.get(i) {
            cx.emit(OpenAt {
                root: root.clone(),
                path: m.path.clone(),
                line: m.line.saturating_sub(1),
                column: m.ranges.first().map_or(0, |r| r[0] as u32),
            });
        }
        cx.notify();
    }

    fn row(&self, i: usize, p: &Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        match &self.rows[i] {
            Row::File { root, path, count } => {
                let (name, dir) = match path.rsplit_once('/') {
                    Some((d, n)) => (n.to_string(), d.to_string()),
                    None => (path.clone(), String::new()),
                };
                let project = self
                    .workspace
                    .read(cx)
                    .projects()
                    .iter()
                    .find(|f| &f.root == root)
                    .map(|f| f.name.clone())
                    .unwrap_or_default();
                let several = self.workspace.read(cx).projects().len() > 1;
                let folded = self.folded.contains(&(root.clone(), path.clone()));
                let selected = self.selected == Some(i);
                h_flex()
                    .id(("find-file", i))
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
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(name))
                    .child(div().min_w_0().truncate().text_color(p.muted).child(
                        if several && !project.is_empty() {
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
                let selected = self.selected == Some(i);
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
                    .pl(px(20.))
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

    /// The selected match's file around it, beside the rows.
    fn preview_pane(&self, p: &Palette) -> gpui::AnyElement {
        let frame = v_flex()
            .w(relative(0.45))
            .flex_none()
            .h_full()
            .border_l_1()
            .border_color(p.border)
            .bg(p.bg);
        let selected = self.selected.and_then(|i| match self.rows.get(i) {
            Some(Row::Line { m, .. }) => Some(m),
            _ => None,
        });
        let (Some((_, path, text)), Some(m)) = (&self.preview, selected) else {
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
        } else if self.pending > 0 && self.rows.is_empty() {
            "Searching…".into()
        } else {
            let files = if self.files == 1 { "file" } else { "files" };
            let matches = if self.matches == 1 {
                "match"
            } else {
                "matches"
            };
            let mut s = format!("{} {matches} in {} {files}", self.matches, self.files);
            if self.truncated {
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
            State::Ready if !self.errors.is_empty() && self.rows.is_empty() => {
                note(self.errors.join("\n"), &p)
            }
            State::Ready if self.rows.is_empty() => note(
                if self.title.is_some() {
                    "No usages found."
                } else if self.query.read(cx).value().is_empty() {
                    "Type to search the environment's projects."
                } else if self.pending > 0 {
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
                                self.shown.len(),
                                cx.processor(|v, range: Range<usize>, _, cx| {
                                    let p = palette::of(cx);
                                    range
                                        .filter_map(|at| v.shown.get(at).copied())
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
            .child(match self.title.clone() {
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
                            self.matches,
                            self.files,
                            if self.files == 1 { "" } else { "s" }
                        )))
                        .child(div().flex_1())
                        .child(chip("find-back", "Search ✕".into(), false, &p).on_click(
                            cx.listener(|v, _, _, cx| {
                                v.title = None;
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
