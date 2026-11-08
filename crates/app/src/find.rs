//! The Find tool window, IntelliJ's Find in Files: text searched for in
//! every project through the Code service's search.text (ripgrep, else git
//! grep, else reading the files), as you type, with match case, whole
//! words, regular expressions and a file mask. Results are grouped by
//! file; a match opens its file at the line.

use std::collections::HashSet;
use std::ops::Range;
use std::time::Duration;

use gpui::{
    Context, Entity, FocusHandle, Focusable, FontWeight, HighlightStyle, IntoElement,
    ParentElement, Render, SharedString, Styled, StyledText, Window, div, prelude::*, px,
    uniform_list,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{h_flex, v_flex};
use serde::Deserialize;
use serde_json::json;

use crate::gitlog::chip;
use crate::palette::{self, Palette};
use crate::workspace::{Event, State, Workspace};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Options {
    case: bool,
    word: bool,
    regex: bool,
}

pub struct FindView {
    workspace: Entity<Workspace>,
    query: Entity<InputState>,
    mask: Entity<InputState>,
    options: Options,
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
            query,
            mask,
            options: Options::default(),
            rows: vec![],
            files: 0,
            matches: 0,
            truncated: false,
            pending: 0,
            errors: vec![],
            generation: 0,
            selected: None,
            _subs: subs,
        }
    }

    pub fn query(&self) -> &Entity<InputState> {
        &self.query
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
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

    /// Searches every project for the query, as it is now.
    pub fn search(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        let text = self.query.read(cx).value().to_string();
        let mask = self.mask.read(cx).value().trim().to_string();
        self.rows.clear();
        self.files = 0;
        self.matches = 0;
        self.truncated = false;
        self.errors.clear();
        self.selected = None;
        self.pending = 0;
        if text.is_empty() {
            cx.notify();
            return;
        }
        let projects: Vec<(String, crate::code::Code)> = self
            .workspace
            .read(cx)
            .projects()
            .iter()
            .map(|f| (f.root.clone(), f.code.clone()))
            .collect();
        let mut params = json!({
            "query": text,
            "regex": self.options.regex,
            "caseSensitive": self.options.case,
            "wholeWord": self.options.word,
        });
        if !mask.is_empty() {
            params["mask"] = mask.into();
        }
        self.pending = projects.len();
        let calls: Vec<_> = projects
            .iter()
            .map(|(root, code)| (root.clone(), code.call("search.text", params.clone())))
            .collect();
        cx.spawn(async move |this, cx| {
            let mut found: Vec<(String, Vec<Match>)> = Vec::new();
            for (root, call) in calls {
                let result = call
                    .await
                    .and_then(|v| serde_json::from_value::<Found>(v).map_err(|e| e.to_string()));
                let keep = this.update(cx, |v, cx| {
                    if v.generation != generation {
                        return false;
                    }
                    v.pending -= 1;
                    match result {
                        Ok(f) => {
                            v.truncated |= f.truncated;
                            found.push((root, f.matches));
                            v.rows = rows(&found);
                            v.files = v
                                .rows
                                .iter()
                                .filter(|r| matches!(r, Row::File { .. }))
                                .count();
                            v.matches = v.rows.len() - v.files;
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
        self.selected = Some(i);
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
                h_flex()
                    .h(px(ROW))
                    .w_full()
                    .gap(px(6.))
                    .px(px(8.))
                    .mt(px(2.))
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
                    .when(selected, |d| d.bg(p.sidebar_active))
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
                    .on_click(cx.listener(move |v, _, _, cx| v.open(i, cx)))
                    .into_any_element()
            }
        }
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
            .child(div().w(px(200.)).flex_none().child(Input::new(&self.mask)))
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
            State::Failed(e) => note(format!("Could not reach the Code service: {e}"), &p),
            State::Ready if !self.errors.is_empty() && self.rows.is_empty() => {
                note(self.errors.join("\n"), &p)
            }
            State::Ready if self.rows.is_empty() => note(
                if self.query.read(cx).value().is_empty() {
                    "Type to search the environment's projects."
                } else if self.pending > 0 {
                    ""
                } else {
                    "Nothing found."
                },
                &p,
            ),
            State::Ready => uniform_list(
                "find-results",
                self.rows.len(),
                cx.processor(|v, range: Range<usize>, _, cx| {
                    let p = palette::of(cx);
                    range.map(|i| v.row(i, &p, cx)).collect::<Vec<_>>()
                }),
            )
            .flex_1()
            .into_any_element(),
        };
        v_flex()
            .size_full()
            .min_h_0()
            .text_size(px(12.))
            .child(bar)
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
