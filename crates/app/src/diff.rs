//! A file's diff, side by side: what it was (at HEAD, or a commit) on the
//! left, what it is (the working tree, or a later commit) on the right,
//! lines paired across a change and long runs of unchanged lines folded.
//! The Code service's git.diff gives both texts; the comparing is done
//! here, and done again as the service says the file or the repository
//! changes.

use std::collections::HashSet;
use std::ops::Range;

use gpui::{
    Context, Entity, FocusHandle, Focusable, FontWeight, Hsla, IntoElement, ParentElement, Render,
    SharedString, Styled, Window, div, prelude::*, px, uniform_list,
};
use gpui_component::{h_flex, v_flex};
use serde_json::{Value, json};
use similar::{DiffOp, TextDiff};

use crate::palette::{self, Palette};
use crate::workspace::{Event, Workspace};

/// How a row changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Same,
    /// Only on the left.
    Deleted,
    /// Only on the right.
    Added,
    /// On both sides, different.
    Changed,
}

/// A line on one side: its number, from 1, and its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Side {
    pub line: usize,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Pair {
        left: Option<Side>,
        right: Option<Side>,
        kind: Kind,
    },
    /// Unchanged lines not shown: how many, and the fold's ID (the left
    /// line number of the first) for opening it.
    Fold { hidden: usize, id: usize },
}

/// The lines unchanged lines keep shown either side of a change.
pub const CONTEXT: usize = 3;

fn lines(text: &str) -> Vec<&str> {
    text.lines()
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect()
}

fn side(lines: &[&str], i: usize) -> Side {
    Side {
        line: i + 1,
        text: lines[i].replace('\t', "    "),
    }
}

/// The rows of old against new, folding runs of unchanged lines but
/// context either side of a change, except the folds named in open. A
/// file the same on both sides is shown whole.
pub fn rows(old: &str, new: &str, context: usize, open: &HashSet<usize>) -> Vec<Row> {
    let (a, b) = (lines(old), lines(new));
    let diff = TextDiff::from_lines(old, new);
    let ops = diff.ops();
    let changed = ops.iter().any(|op| !matches!(op, DiffOp::Equal { .. }));
    let mut out = Vec::new();
    let pair = |out: &mut Vec<Row>, i: usize, j: usize| {
        out.push(Row::Pair {
            left: Some(side(&a, i)),
            right: Some(side(&b, j)),
            kind: Kind::Same,
        })
    };
    for (n, op) in ops.iter().enumerate() {
        match *op {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => {
                let lead = if n == 0 { 0 } else { context };
                let trail = if n + 1 == ops.len() { 0 } else { context };
                let hidden = len.saturating_sub(lead + trail);
                let id = old_index + lead + 1;
                if !changed || hidden < 2 || open.contains(&id) {
                    for k in 0..len {
                        pair(&mut out, old_index + k, new_index + k);
                    }
                    continue;
                }
                for k in 0..lead {
                    pair(&mut out, old_index + k, new_index + k);
                }
                out.push(Row::Fold { hidden, id });
                for k in len - trail..len {
                    pair(&mut out, old_index + k, new_index + k);
                }
            }
            DiffOp::Delete {
                old_index, old_len, ..
            } => {
                for k in 0..old_len {
                    out.push(Row::Pair {
                        left: Some(side(&a, old_index + k)),
                        right: None,
                        kind: Kind::Deleted,
                    });
                }
            }
            DiffOp::Insert {
                new_index, new_len, ..
            } => {
                for k in 0..new_len {
                    out.push(Row::Pair {
                        left: None,
                        right: Some(side(&b, new_index + k)),
                        kind: Kind::Added,
                    });
                }
            }
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                for k in 0..old_len.max(new_len) {
                    let left = (k < old_len).then(|| side(&a, old_index + k));
                    let right = (k < new_len).then(|| side(&b, new_index + k));
                    let kind = match (&left, &right) {
                        (Some(_), Some(_)) => Kind::Changed,
                        (Some(_), None) => Kind::Deleted,
                        _ => Kind::Added,
                    };
                    out.push(Row::Pair { left, right, kind });
                }
            }
        }
    }
    out
}

/// The lines added and deleted.
pub fn counts(old: &str, new: &str) -> (usize, usize) {
    let mut added = 0;
    let mut deleted = 0;
    for op in TextDiff::from_lines(old, new).ops() {
        match *op {
            DiffOp::Delete { old_len, .. } => deleted += old_len,
            DiffOp::Insert { new_len, .. } => added += new_len,
            DiffOp::Replace {
                old_len, new_len, ..
            } => {
                deleted += old_len;
                added += new_len;
            }
            DiffOp::Equal { .. } => {}
        }
    }
    (added, deleted)
}

/// What a diff compares: a file in a project, as it was at against
/// (under the name from, if it was renamed) beside the working tree's, or
/// to's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub root: String,
    pub path: String,
    pub against: String,
    pub from: Option<String>,
    pub to: Option<String>,
    /// What the file is compared with when not its history.
    pub other: Option<Other>,
}

/// What a file is compared with, outside git: another file, or text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Other {
    /// A file, by the folder its connection is in (a project's root, or /)
    /// and its absolute path.
    File { root: String, abs: String },
    /// Text, as the clipboard held it.
    Text(String),
}

impl Spec {
    /// A file's changes in the working tree.
    pub fn working(root: &str, path: &str) -> Spec {
        Spec {
            root: root.into(),
            path: path.into(),
            against: "HEAD".into(),
            from: None,
            to: None,
            other: None,
        }
    }

    /// The working tree's file against a branch or a commit.
    pub fn against(root: &str, path: &str, rev: &str) -> Spec {
        Spec {
            against: rev.into(),
            ..Spec::working(root, path)
        }
    }

    /// A file compared with another, or with text: the other on the left.
    pub fn compare(root: &str, path: &str, other: Other) -> Spec {
        Spec {
            other: Some(other),
            ..Spec::working(root, path)
        }
    }

    /// The file's absolute path.
    pub fn abs(&self) -> String {
        format!("{}/{}", self.root.trim_end_matches('/'), self.path)
    }
}

/// Asks for a diff to be shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShowDiff(pub Spec);

enum Loaded {
    Reading,
    Failed(String),
    Binary,
    Texts { old: String, new: String },
}

pub struct DiffView {
    workspace: Entity<Workspace>,
    spec: Spec,
    loaded: Loaded,
    rows: Vec<Row>,
    counts: (usize, usize),
    open: HashSet<usize>,
    focus: FocusHandle,
    _sub: gpui::Subscription,
}

impl DiffView {
    pub fn new(workspace: Entity<Workspace>, spec: Spec, cx: &mut Context<Self>) -> Self {
        let sub = cx.subscribe(&workspace, |v, _, e: &Event, cx| {
            // A project connected after the diff was asked for.
            if matches!(e, Event::Projects) && matches!(v.loaded, Loaded::Failed(_)) {
                return v.load(cx);
            }
            let Event::Heard {
                root,
                event,
                params,
            } = e
            else {
                return;
            };
            if root != &v.spec.root {
                return;
            }
            let dir = v
                .spec
                .abs()
                .rsplit_once('/')
                .map(|(d, _)| d.to_string())
                .unwrap_or_default();
            let touched = match event.as_str() {
                "git.changed" => true,
                "fs.changed" => {
                    v.spec.to.is_none()
                        && params["dirs"]
                            .as_array()
                            .is_some_and(|ds| ds.iter().any(|d| d.as_str() == Some(dir.as_str())))
                }
                _ => false,
            };
            if touched {
                v.load(cx);
            }
        });
        let mut v = DiffView {
            workspace,
            spec,
            loaded: Loaded::Reading,
            rows: vec![],
            counts: (0, 0),
            open: HashSet::new(),
            focus: cx.focus_handle(),
            _sub: sub,
        };
        v.load(cx);
        v
    }

    pub fn spec(&self) -> &Spec {
        &self.spec
    }

    /// The tab's name.
    pub fn title(&self) -> String {
        let name = self.spec.path.rsplit('/').next().unwrap_or(&self.spec.path);
        match &self.spec.other {
            None => format!("Diff: {name}"),
            Some(Other::File { abs, .. }) => {
                format!("{} vs {name}", abs.rsplit('/').next().unwrap_or(abs))
            }
            Some(Other::Text(_)) => format!("Clipboard vs {name}"),
        }
    }

    /// A connection that reaches a folder: its project's, or for /, the
    /// editor folder's, which reaches the whole machine.
    fn code_for(&self, root: &str, cx: &gpui::App) -> Option<crate::code::Code> {
        let ws = self.workspace.read(cx);
        ws.code(root)
            .or_else(|| (root == "/").then(|| ws.primary()).flatten())
    }

    /// Compares the file with another or with text: both read as they are.
    fn load_compared(&mut self, other: Other, cx: &mut Context<Self>) {
        let Some(code) = self.code_for(&self.spec.root, cx) else {
            self.loaded = Loaded::Failed("The project is not connected.".into());
            return;
        };
        let right = code.call("fs.read", json!({ "path": self.spec.abs() }));
        let left = match &other {
            Other::Text(_) => None,
            Other::File { root, abs } => match self.code_for(root, cx) {
                Some(c) => Some(c.call("fs.read", json!({ "path": abs }))),
                None => {
                    self.loaded = Loaded::Failed("Its project is not connected.".into());
                    return;
                }
            },
        };
        cx.spawn(async move |this, cx| {
            let right = right.await;
            let left = match (left, other) {
                (Some(call), _) => call.await,
                (None, Other::Text(t)) => Ok(json!({ "text": t })),
                (None, Other::File { .. }) => Err("nothing to compare with".into()),
            };
            let _ = this.update(cx, |v, cx| {
                v.loaded = match (left, right) {
                    (Err(e), _) | (_, Err(e)) => Loaded::Failed(e),
                    (Ok(l), Ok(r)) => match (l["text"].as_str(), r["text"].as_str()) {
                        (Some(old), Some(new)) => Loaded::Texts {
                            old: old.to_string(),
                            new: new.to_string(),
                        },
                        _ => Loaded::Binary,
                    },
                };
                v.relayout();
                cx.notify();
            });
        })
        .detach();
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        if let Some(other) = self.spec.other.clone() {
            return self.load_compared(other, cx);
        }
        let Some(code) = self.workspace.read(cx).code(&self.spec.root) else {
            self.loaded = Loaded::Failed("The project is not connected.".into());
            return;
        };
        let mut params = json!({ "path": self.spec.path, "against": self.spec.against });
        if let Some(from) = &self.spec.from {
            params["from"] = Value::String(from.clone());
        }
        if let Some(to) = &self.spec.to {
            params["to"] = Value::String(to.clone());
        }
        let call = code.call("git.diff", params);
        cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |v, cx| {
                v.loaded = match result {
                    Err(e) => Loaded::Failed(e),
                    Ok(d) if d["binary"].as_bool() == Some(true) => Loaded::Binary,
                    Ok(d) => Loaded::Texts {
                        old: d["head"].as_str().unwrap_or_default().to_string(),
                        new: d["working"].as_str().unwrap_or_default().to_string(),
                    },
                };
                v.relayout();
                cx.notify();
            });
        })
        .detach();
    }

    fn relayout(&mut self) {
        if let Loaded::Texts { old, new } = &self.loaded {
            self.rows = rows(old, new, CONTEXT, &self.open);
            self.counts = counts(old, new);
        } else {
            self.rows.clear();
        }
    }

    fn unfold(&mut self, id: usize, cx: &mut Context<Self>) {
        self.open.insert(id);
        self.relayout();
        cx.notify();
    }

    fn row(&self, i: usize, p: &Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        match &self.rows[i] {
            Row::Fold { hidden, id } => {
                let id = *id;
                h_flex()
                    .id(("fold", i))
                    .h(px(ROW))
                    .w_full()
                    .px(px(8.))
                    .bg(p.surface_2)
                    .text_color(p.muted)
                    .cursor_pointer()
                    .hover(|s| s.text_color(p.accent))
                    .child(format!(
                        "⋯ {hidden} unchanged line{}",
                        if *hidden == 1 { "" } else { "s" }
                    ))
                    .on_click(cx.listener(move |v, _, _, cx| v.unfold(id, cx)))
                    .into_any_element()
            }
            Row::Pair { left, right, kind } => {
                let (lbg, rbg) = match kind {
                    Kind::Same => (None, None),
                    Kind::Deleted => (Some(p.bad.opacity(0.14)), None),
                    Kind::Added => (None, Some(p.good.opacity(0.14))),
                    Kind::Changed => (Some(p.bad.opacity(0.14)), Some(p.good.opacity(0.14))),
                };
                h_flex()
                    .h(px(ROW))
                    .w_full()
                    .child(
                        half(left.as_ref(), lbg, p)
                            .border_r_1()
                            .border_color(p.border),
                    )
                    .child(half(right.as_ref(), rbg, p))
                    .into_any_element()
            }
        }
    }
}

const ROW: f32 = 18.;

fn half(s: Option<&Side>, bg: Option<Hsla>, p: &Palette) -> gpui::Div {
    let d = h_flex().flex_1().min_w_0().h_full().overflow_hidden();
    let Some(s) = s else {
        return d.bg(p.surface_2.opacity(0.6));
    };
    d.when_some(bg, |d, bg| d.bg(bg))
        .child(
            div()
                .w(px(44.))
                .flex_none()
                .pr(px(8.))
                .text_right()
                .text_color(p.muted)
                .child(s.line.to_string()),
        )
        .child(
            div()
                .whitespace_nowrap()
                .child(SharedString::from(s.text.clone())),
        )
}

impl Focusable for DiffView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for DiffView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let (before, after) = match (&self.spec.other, &self.spec.to, self.spec.against.as_str()) {
            (Some(Other::Text(_)), ..) => ("Clipboard".to_string(), String::new()),
            (Some(Other::File { .. }), ..) => (String::new(), String::new()),
            (None, Some(to), against) => (short(against), short(to)),
            (None, None, "HEAD") => ("HEAD".to_string(), "Working tree".to_string()),
            (None, None, against) => (short(against), "Working tree".to_string()),
        };
        let left_name = match &self.spec.other {
            Some(Other::File { abs, .. }) => abs.clone(),
            Some(Other::Text(_)) => String::new(),
            None => self
                .spec
                .from
                .clone()
                .unwrap_or_else(|| self.spec.path.clone()),
        };
        let head = h_flex()
            .flex_none()
            .h(px(24.))
            .border_b_1()
            .border_color(p.border)
            .text_size(px(11.))
            .text_color(p.muted)
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .px(px(8.))
                    .gap(px(6.))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(before))
                    .child(div().truncate().child(left_name)),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .px(px(8.))
                    .gap(px(6.))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(after))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(self.spec.path.clone()),
                    )
                    .child(
                        div()
                            .text_color(p.good)
                            .child(format!("+{}", self.counts.0)),
                    )
                    .child(div().text_color(p.bad).child(format!("−{}", self.counts.1))),
            );
        let body = match &self.loaded {
            Loaded::Reading => note("Reading…", &p).into_any_element(),
            Loaded::Failed(e) => {
                note(format!("Could not read the diff: {e}"), &p).into_any_element()
            }
            Loaded::Binary => {
                note("A binary file: its contents are not compared.", &p).into_any_element()
            }
            Loaded::Texts { .. } => uniform_list(
                "diff-rows",
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
            .track_focus(&self.focus)
            .size_full()
            .bg(p.bg)
            .font_family(mono())
            .text_size(px(12.))
            .child(head)
            .child(body)
    }
}

/// A revision as a header names it: a commit by its first seven.
fn short(rev: &str) -> String {
    if let Some(base) = rev.strip_suffix('^') {
        return format!("{}^", short(base));
    }
    if rev.len() > 12 && rev.bytes().all(|b| b.is_ascii_hexdigit()) {
        rev[..7].to_string()
    } else {
        rev.to_string()
    }
}

/// The monospace face: the platform's own.
pub fn mono() -> &'static str {
    if cfg!(target_os = "macos") {
        "Menlo"
    } else if cfg!(target_os = "windows") {
        "Consolas"
    } else {
        "DejaVu Sans Mono"
    }
}

fn note(text: impl Into<SharedString>, p: &Palette) -> impl IntoElement {
    div()
        .p(px(12.))
        .text_size(px(12.))
        .text_color(p.muted)
        .child(text.into())
}
