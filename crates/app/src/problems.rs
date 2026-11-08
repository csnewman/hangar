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

/// The rows for every file's diagnostics, files in path order, each
/// file's problems by severity then by place.
pub fn rows<'a>(files: impl IntoIterator<Item = (&'a String, &'a Vec<Diagnostic>)>) -> Vec<Row> {
    let mut out = Vec::new();
    for (abs, list) in files {
        if list.is_empty() {
            continue;
        }
        let mut list = list.clone();
        list.sort_by_key(|d| {
            (
                rank(d.severity),
                d.range.start.line,
                d.range.start.character,
            )
        });
        out.push(Row::File {
            abs: abs.clone(),
            errors: list.iter().filter(|d| rank(d.severity) == 0).count(),
            warnings: list.iter().filter(|d| rank(d.severity) == 1).count(),
            count: list.len(),
        });
        out.extend(list.into_iter().map(|d| Row::Problem {
            abs: abs.clone(),
            d: Box::new(d),
        }));
    }
    out
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
            _sub: sub,
        };
        v.refresh(cx);
        v
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.rows = rows(self.languages.read(cx).diagnostics());
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
                h_flex()
                    .h(px(ROW))
                    .w_full()
                    .gap(px(6.))
                    .px(px(8.))
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
        let body: gpui::AnyElement = if self.rows.is_empty() {
            div()
                .px(px(10.))
                .py(px(6.))
                .text_size(px(11.5))
                .text_color(p.muted)
                .child(SharedString::from(
                    "No problems in the files open. Language servers report on the files the editors show.",
                ))
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
            .child(notes)
            .child(v_flex().flex_1().min_h_0().child(body))
    }
}
