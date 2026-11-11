//! The Problems tool window: every file's diagnostics the language servers
//! have published, errors first within each file, and the languages a
//! project has no server for and why. A problem opens its file at it.

use std::ops::Range;

use gpui::{
    Context, Entity, FontWeight, Hsla, IntoElement, ParentElement, Render, SharedString, Styled,
    Window, div, prelude::*, px, uniform_list,
};
use gpui_component::{h_flex, v_flex};
use lsp_types::{Diagnostic, DiagnosticSeverity};

use crate::find::OpenAt;
use crate::languages::Languages;
use crate::palette::{self, Palette};
use crate::workspace::Workspace;

/// One row of the window: a file's heading, or one of its problems.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    File {
        abs: String,
        errors: usize,
        warnings: usize,
        count: usize,
    },
    Problem {
        abs: String,
        d: Box<Diagnostic>,
    },
}

/// What the window shows: hints or not, the files folded, and one file
/// alone (the Current File tab).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Shown {
    pub hints: bool,
    pub folded: std::collections::HashSet<String>,
    pub only: Option<String>,
}

/// The rows for every file's diagnostics, the worst files first (most
/// errors, then most warnings, then by path), each file's problems by
/// severity then by place; a folded file's row alone.
pub fn rows<'a>(
    files: impl IntoIterator<Item = (&'a String, &'a Vec<Diagnostic>)>,
    shown: &Shown,
) -> Vec<Row> {
    let mut each: Vec<(String, Vec<Diagnostic>, usize, usize)> = files
        .into_iter()
        .filter(|(abs, _)| shown.only.as_ref().is_none_or(|o| o == *abs))
        .filter_map(|(abs, list)| {
            let mut list: Vec<Diagnostic> = list
                .iter()
                .filter(|d| shown.hints || d.severity != Some(DiagnosticSeverity::HINT))
                .cloned()
                .collect();
            if list.is_empty() {
                return None;
            }
            list.sort_by_key(|d| {
                (
                    rank(d.severity),
                    d.range.start.line,
                    d.range.start.character,
                )
            });
            let errors = list.iter().filter(|d| rank(d.severity) == 0).count();
            let warnings = list.iter().filter(|d| rank(d.severity) == 1).count();
            Some((abs.clone(), list, errors, warnings))
        })
        .collect();
    each.sort_by(|a, b| (b.2, b.3).cmp(&(a.2, a.3)).then_with(|| a.0.cmp(&b.0)));
    let mut out = Vec::new();
    for (abs, list, errors, warnings) in each {
        out.push(Row::File {
            abs: abs.clone(),
            errors,
            warnings,
            count: list.len(),
        });
        if shown.folded.contains(&abs) {
            continue;
        }
        out.extend(list.into_iter().map(|d| Row::Problem {
            abs: abs.clone(),
            d: Box::new(d),
        }));
    }
    out
}

/// What the rows add up to: errors, warnings, and files.
pub fn totals(rows: &[Row]) -> (usize, usize, usize) {
    rows.iter().fold((0, 0, 0), |(e, w, f), r| match r {
        Row::File {
            errors, warnings, ..
        } => (e + errors, w + warnings, f + 1),
        Row::Problem { .. } => (e, w, f),
    })
}

/// How many errors the servers have published, every file's.
pub fn errors_in<'a>(files: impl IntoIterator<Item = &'a Vec<Diagnostic>>) -> usize {
    files
        .into_iter()
        .flatten()
        .filter(|d| rank(d.severity) == 0)
        .count()
}

/// The problem F2 goes to from a place in a file, as IntelliJ's Next
/// Highlighted Error does: the first that starts after it, round to the
/// top; with back (⇧F2) the last that starts before it, round to the
/// bottom. Hints are passed over.
pub fn next_problem(
    list: &[Diagnostic],
    at: lsp_types::Position,
    back: bool,
) -> Option<&Diagnostic> {
    let mut shown: Vec<&Diagnostic> = list
        .iter()
        .filter(|d| d.severity != Some(DiagnosticSeverity::HINT))
        .collect();
    shown.sort_by_key(|d| (d.range.start.line, d.range.start.character));
    let start = |d: &&Diagnostic| d.range.start;
    if back {
        shown
            .iter()
            .rev()
            .find(|d| start(d) < at)
            .or(shown.last())
            .copied()
    } else {
        shown
            .iter()
            .find(|d| start(d) > at)
            .or(shown.first())
            .copied()
    }
}

/// The colour a problem of a severity is shown in.
pub fn severity_colour(s: Option<DiagnosticSeverity>, p: &Palette) -> Hsla {
    colour(s, p)
}

/// A severity's order: errors, warnings, information, hints. A server
/// giving none means an error.
fn rank(s: Option<DiagnosticSeverity>) -> u8 {
    match s {
        Some(DiagnosticSeverity::WARNING) => 1,
        Some(DiagnosticSeverity::INFORMATION) => 2,
        Some(DiagnosticSeverity::HINT) => 3,
        _ => 0,
    }
}

fn colour(s: Option<DiagnosticSeverity>, p: &Palette) -> Hsla {
    match rank(s) {
        0 => p.bad,
        1 => p.busy,
        _ => p.muted,
    }
}

pub struct ProblemsView {
    workspace: Entity<Workspace>,
    languages: Entity<Languages>,
    rows: Vec<Row>,
    selected: Option<usize>,
    /// What is shown, the Current File tab's file, and whether that tab is
    /// the one shown.
    shown: Shown,
    current: Option<String>,
    current_tab: bool,
    _sub: gpui::Subscription,
}

impl gpui::EventEmitter<OpenAt> for ProblemsView {}

impl ProblemsView {
    pub fn new(
        workspace: Entity<Workspace>,
        languages: Entity<Languages>,
        cx: &mut Context<Self>,
    ) -> Self {
        let sub = cx.observe(&languages, |v, _, cx| v.refresh(cx));
        let mut v = ProblemsView {
            workspace,
            languages,
            rows: vec![],
            selected: None,
            shown: Shown::default(),
            current: None,
            current_tab: false,
            _sub: sub,
        };
        v.refresh(cx);
        v
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The file the editors show, for the Current File tab.
    pub fn set_current(&mut self, abs: Option<String>, cx: &mut Context<Self>) {
        if self.current != abs {
            self.current = abs;
            self.refresh(cx);
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.shown.only = if self.current_tab {
            Some(self.current.clone().unwrap_or_default())
        } else {
            None
        };
        self.rows = rows(self.languages.read(cx).diagnostics(), &self.shown);
        self.selected = self.selected.filter(|i| *i < self.rows.len());
        cx.notify();
    }

    /// Opens the problem on row i.
    pub fn open(&mut self, i: usize, cx: &mut Context<Self>) {
        self.selected = Some(i);
        if let Some(Row::Problem { abs, d }) = self.rows.get(i) {
            let ws = self.workspace.read(cx);
            let (root, path) = match ws.project_of(abs) {
                Some(f) => (
                    f.root.clone(),
                    abs[f.root.trim_end_matches('/').len() + 1..].to_string(),
                ),
                None => ("/".to_string(), abs.clone()),
            };
            cx.emit(OpenAt {
                root,
                path,
                line: d.range.start.line,
                column: d.range.start.character,
            });
        }
        cx.notify();
    }

    fn row(&self, i: usize, p: &Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        match &self.rows[i] {
            Row::File {
                abs,
                errors,
                warnings,
                count,
            } => {
                let (dir, name) = abs.rsplit_once('/').unwrap_or(("", abs));
                let shown = self.workspace.read(cx).shown(dir);
                let errs = format!("{errors} error{}", if *errors == 1 { "" } else { "s" });
                let warns = format!(
                    "{warnings} warning{}",
                    if *warnings == 1 { "" } else { "s" }
                );
                let tally = match (errors, warnings) {
                    (0, 0) => format!("{count}"),
                    (_, 0) => errs,
                    (0, _) => warns,
                    _ => format!("{errs}, {warns}"),
                };
                let folded = self.shown.folded.contains(abs);
                let at = abs.clone();
                h_flex()
                    .id(("problem-file", i))
                    .h(px(ROW))
                    .w_full()
                    .gap(px(6.))
                    .px(px(8.))
                    .cursor_pointer()
                    .hover(|s| s.bg(p.surface_2))
                    .on_click(cx.listener(move |v, _, _, cx| {
                        if !v.shown.folded.remove(&at) {
                            v.shown.folded.insert(at.clone());
                        }
                        v.refresh(cx);
                    }))
                    .child(crate::project::chevron(!folded, p))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(name.to_string()),
                    )
                    .child(div().min_w_0().truncate().text_color(p.muted).child(shown))
                    .child(div().text_color(p.muted).child(tally))
                    .into_any_element()
            }
            Row::Problem { d, .. } => {
                let selected = self.selected == Some(i);
                let c = colour(d.severity, p);
                let message = d.message.lines().next().unwrap_or_default().to_string();
                let source = d.source.clone().unwrap_or_default();
                h_flex()
                    .id(("problem", i))
                    .h(px(ROW))
                    .w_full()
                    .gap(px(8.))
                    .pl(px(20.))
                    .pr(px(8.))
                    .cursor_pointer()
                    .when(selected, |e| e.bg(p.selection))
                    .when(!selected, |e| e.hover(|s| s.bg(p.surface_2)))
                    .child(div().flex_none().size(px(7.)).rounded(px(4.)).bg(c))
                    .child(div().min_w_0().truncate().child(message))
                    .when(!source.is_empty(), |e| {
                        e.child(div().flex_none().text_color(p.muted).child(source))
                    })
                    .child(div().flex_1())
                    .child(div().flex_none().text_color(p.muted).child(format!(
                        "{}:{}",
                        d.range.start.line + 1,
                        d.range.start.character + 1
                    )))
                    .tooltip({
                        let whole = d.message.clone();
                        move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(whole.clone()).build(window, cx)
                        }
                    })
                    .on_click(cx.listener(move |v, _, _, cx| v.open(i, cx)))
                    .into_any_element()
            }
        }
    }
}

const ROW: f32 = 20.;

impl Render for ProblemsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let unavailable = self.languages.read(cx).unavailable();
        let ws = self.workspace.read(cx);
        let mut notes = v_flex().flex_none().px(px(8.)).pt(px(4.)).gap(px(2.));
        for (root, lang, why) in &unavailable {
            notes = notes.child(
                div()
                    .text_size(px(11.))
                    .text_color(p.muted)
                    .child(format!("No {lang} server in {}: {why}", ws.shown(root))),
            );
        }
        let (errors, warnings, files) = totals(&self.rows);
        let tab = |id: &'static str, label: &'static str, on: bool, cx: &mut Context<Self>| {
            crate::gitlog::chip(id, label.to_string(), on, &p).on_click(cx.listener(
                move |v, _, _, cx| {
                    v.current_tab = id == "problems-current";
                    v.refresh(cx);
                },
            ))
        };
        let current_tab = self.current_tab;
        let hints = self.shown.hints;
        let bar = h_flex()
            .flex_none()
            .gap(px(4.))
            .px(px(8.))
            .h(px(26.))
            .child(tab("problems-current", "Current File", current_tab, cx))
            .child(tab("problems-all", "All Files", !current_tab, cx))
            .child(div().w(px(1.)).h(px(14.)).mx(px(4.)).bg(p.border))
            .child(
                crate::gitlog::chip("problems-hints", "Show Hints".to_string(), hints, &p)
                    .on_click(cx.listener(|v, _, _, cx| {
                        v.shown.hints = !v.shown.hints;
                        v.refresh(cx);
                    })),
            )
            .child(div().flex_1())
            .child(div().text_size(px(11.)).text_color(p.muted).child(format!(
                "{errors} error{}, {warnings} warning{} in {files} file{}",
                if errors == 1 { "" } else { "s" },
                if warnings == 1 { "" } else { "s" },
                if files == 1 { "" } else { "s" }
            )));
        let body: gpui::AnyElement = if self.rows.is_empty() {
            div()
                .px(px(10.))
                .py(px(6.))
                .text_size(px(11.5))
                .text_color(p.muted)
                .child(SharedString::from(if current_tab {
                    "No problems in the file shown."
                } else {
                    "No problems in the files open. Language servers report on the files the editors show."
                }))
                .into_any_element()
        } else {
            uniform_list(
                "problems",
                self.rows.len(),
                cx.processor(|v, range: Range<usize>, _, cx| {
                    let p = palette::of(cx);
                    range.map(|i| v.row(i, &p, cx)).collect::<Vec<_>>()
                }),
            )
            .flex_1()
            .into_any_element()
        };
        v_flex()
            .size_full()
            .min_h_0()
            .text_size(px(12.))
            .child(bar)
            .child(notes)
            .child(v_flex().flex_1().min_h_0().child(body))
    }
}
