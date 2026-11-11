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

/// One row of the window: a severity's heading (grouped by severity), a
/// file's heading, or one of its problems.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    Severity {
        rank: u8,
        count: usize,
    },
    File {
        abs: String,
        errors: usize,
        warnings: usize,
        count: usize,
        /// The severity it is listed under, when grouped.
        group: Option<u8>,
    },
    Problem {
        abs: String,
        d: Box<Diagnostic>,
    },
}

impl Row {
    /// What the row is folded by, in `Shown::folded`: a severity, a file,
    /// or a file under a severity. Problems do not fold.
    pub fn key(&self) -> Option<String> {
        match self {
            Row::Severity { rank, .. } => Some(format!("severity {rank}")),
            Row::File {
                abs,
                group: Some(r),
                ..
            } => Some(format!("{r} {abs}")),
            Row::File { abs, .. } => Some(abs.clone()),
            Row::Problem { .. } => None,
        }
    }
}

/// What the window shows: hints or not, the rows folded (by `Row::key`),
/// one file alone (the Current File tab), and IntelliJ's Group by
/// Severity and Sort by Name.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Shown {
    pub hints: bool,
    pub folded: std::collections::HashSet<String>,
    pub only: Option<String>,
    pub by_severity: bool,
    pub by_name: bool,
}

/// What a severity's heading says.
pub fn severity_name(rank: u8) -> &'static str {
    match rank {
        0 => "Errors",
        1 => "Warnings",
        2 => "Information",
        _ => "Hints",
    }
}

/// The rows for every file's diagnostics, the worst files first (most
/// errors, then most warnings, then by path) or by path alone, each
/// file's problems by severity then by place; a folded row without what
/// is under it. Grouped by severity, each severity heads the files with
/// problems of it, and the Current File tab lists its problems straight
/// under each severity.
pub fn rows<'a>(
    files: impl IntoIterator<Item = (&'a String, &'a Vec<Diagnostic>)>,
    shown: &Shown,
) -> Vec<Row> {
    let each = shown_files(files, shown);
    let mut out = Vec::new();
    if shown.by_severity {
        for r in 0..4u8 {
            let under: Vec<(&String, Vec<&Diagnostic>)> = each
                .iter()
                .map(|(abs, list, ..)| {
                    let of: Vec<&Diagnostic> =
                        list.iter().filter(|d| rank(d.severity) == r).collect();
                    (abs, of)
                })
                .filter(|(_, of)| !of.is_empty())
                .collect();
            if under.is_empty() {
                continue;
            }
            let heading = Row::Severity {
                rank: r,
                count: under.iter().map(|(_, of)| of.len()).sum(),
            };
            let folded = heading.key().is_some_and(|k| shown.folded.contains(&k));
            out.push(heading);
            if folded {
                continue;
            }
            for (abs, of) in under {
                if shown.only.is_none() {
                    let file = Row::File {
                        abs: abs.clone(),
                        errors: if r == 0 { of.len() } else { 0 },
                        warnings: if r == 1 { of.len() } else { 0 },
                        count: of.len(),
                        group: Some(r),
                    };
                    let folded = file.key().is_some_and(|k| shown.folded.contains(&k));
                    out.push(file);
                    if folded {
                        continue;
                    }
                }
                out.extend(of.into_iter().map(|d| Row::Problem {
                    abs: abs.clone(),
                    d: Box::new(d.clone()),
                }));
            }
        }
        return out;
    }
    for (abs, list, errors, warnings) in each {
        out.push(Row::File {
            abs: abs.clone(),
            errors,
            warnings,
            count: list.len(),
            group: None,
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

/// The files with problems shown, in the order listed, each with its
/// problems sorted, its errors and its warnings.
fn shown_files<'a>(
    files: impl IntoIterator<Item = (&'a String, &'a Vec<Diagnostic>)>,
    shown: &Shown,
) -> Vec<(String, Vec<Diagnostic>, usize, usize)> {
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
    if shown.by_name {
        each.sort_by(|a, b| a.0.cmp(&b.0));
    } else {
        each.sort_by(|a, b| (b.2, b.3).cmp(&(a.2, a.3)).then_with(|| a.0.cmp(&b.0)));
    }
    each
}

/// What is shown adds up to, folded or not: errors, warnings, and files.
pub fn totals<'a>(
    files: impl IntoIterator<Item = (&'a String, &'a Vec<Diagnostic>)>,
    shown: &Shown,
) -> (usize, usize, usize) {
    shown_files(files, shown)
        .iter()
        .fold((0, 0, 0), |(e, w, f), (_, _, errors, warnings)| {
            (e + errors, w + warnings, f + 1)
        })
}

/// The keys of every row that folds, for Collapse All.
pub fn foldable<'a>(
    files: impl IntoIterator<Item = (&'a String, &'a Vec<Diagnostic>)>,
    shown: &Shown,
) -> std::collections::HashSet<String> {
    let open = Shown {
        folded: Default::default(),
        ..shown.clone()
    };
    rows(files, &open).iter().filter_map(Row::key).collect()
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
    colour_of(rank(s), p)
}

fn colour_of(rank: u8, p: &Palette) -> Hsla {
    match rank {
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
    totals: (usize, usize, usize),
    _sub: gpui::Subscription,
}

/// Asks for a problem's file opened at it with the server's fixes listed
/// there, as IntelliJ's Show Quick Fixes.
#[derive(Debug, Clone)]
pub struct QuickFix(pub OpenAt);

impl gpui::EventEmitter<OpenAt> for ProblemsView {}
impl gpui::EventEmitter<QuickFix> for ProblemsView {}

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
            totals: (0, 0, 0),
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
        let files = self.languages.read(cx).diagnostics();
        self.rows = rows(files, &self.shown);
        self.totals = totals(files, &self.shown);
        self.selected = self.selected.filter(|i| *i < self.rows.len());
        cx.notify();
    }

    /// Folds a row, or unfolds it.
    fn fold(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.shown.folded.remove(&key) {
            self.shown.folded.insert(key);
        }
        self.refresh(cx);
    }

    /// IntelliJ's Expand All and Collapse All.
    fn fold_all(&mut self, fold: bool, cx: &mut Context<Self>) {
        self.shown.folded = if fold {
            foldable(self.languages.read(cx).diagnostics(), &self.shown)
        } else {
            Default::default()
        };
        self.refresh(cx);
    }

    /// Where a row opens: a problem at it, a file at its first problem.
    fn place(&self, i: usize, cx: &Context<Self>) -> Option<OpenAt> {
        let (abs, d) = match self.rows.get(i)? {
            Row::Problem { abs, d } => (abs, Some(d.as_ref())),
            Row::File { abs, .. } => (abs, None),
            Row::Severity { .. } => return None,
        };
        let d = d.or_else(|| {
            self.rows[i + 1..].iter().find_map(|r| match r {
                Row::Problem { abs: a, d } if a == abs => Some(d.as_ref()),
                _ => None,
            })
        });
        let ws = self.workspace.read(cx);
        let (root, path) = match ws.project_of(abs) {
            Some(f) => (
                f.root.clone(),
                abs[f.root.trim_end_matches('/').len() + 1..].to_string(),
            ),
            None => ("/".to_string(), abs.clone()),
        };
        Some(OpenAt {
            root,
            path,
            line: d.map_or(0, |d| d.range.start.line),
            column: d.map_or(0, |d| d.range.start.character),
        })
    }

    /// Opens the problem on row i.
    pub fn open(&mut self, i: usize, cx: &mut Context<Self>) {
        self.selected = Some(i);
        if let Some(at) = self.place(i, cx) {
            cx.emit(at);
        }
        cx.notify();
    }

    /// Opens the problem on row i with the server's fixes for it listed.
    fn quick_fix(&mut self, i: usize, cx: &mut Context<Self>) {
        self.selected = Some(i);
        if let Some(at) = self.place(i, cx) {
            cx.emit(QuickFix(at));
        }
        cx.notify();
    }

    /// A row's menu, as IntelliJ's: a problem's Jump to Source, Show Quick
    /// Fixes and Copy Problem Description; a file's Jump to Source and Copy
    /// Path; and Expand All and Collapse All.
    fn menu(
        me: gpui::WeakEntity<Self>,
        i: usize,
        row: &Row,
        menu: gpui_component::menu::PopupMenu,
    ) -> gpui_component::menu::PopupMenu {
        use gpui_component::menu::PopupMenuItem;
        let on = |f: fn(&mut Self, usize, &mut Context<Self>)| {
            let me = me.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                let _ = me.update(cx, |v, cx| f(v, i, cx));
            }
        };
        let copy = |text: String| {
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()))
            }
        };
        let menu = match row {
            Row::Problem { d, .. } => menu
                .item(PopupMenuItem::new("Jump to Source").on_click(on(Self::open)))
                .item(PopupMenuItem::new("Show Quick Fixes").on_click(on(Self::quick_fix)))
                .separator()
                .item(
                    PopupMenuItem::new("Copy Problem Description")
                        .on_click(copy(d.message.clone())),
                ),
            Row::File { abs, .. } => menu
                .item(PopupMenuItem::new("Jump to Source").on_click(on(Self::open)))
                .item(PopupMenuItem::new("Copy Path").on_click(copy(abs.clone()))),
            Row::Severity { .. } => menu,
        };
        let all = |fold: bool| {
            let me = me.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                let _ = me.update(cx, |v, cx| v.fold_all(fold, cx));
            }
        };
        let menu = if matches!(row, Row::Severity { .. }) {
            menu
        } else {
            menu.separator()
        };
        menu.item(PopupMenuItem::new("Expand All").on_click(all(false)))
            .item(PopupMenuItem::new("Collapse All").on_click(all(true)))
    }

    /// The Options menu: IntelliJ's Group by Severity and Sort by Name,
    /// then Expand All and Collapse All.
    fn options(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        use gpui_component::menu::PopupMenuItem;
        let me = cx.entity().downgrade();
        let (grouped, named) = (self.shown.by_severity, self.shown.by_name);
        crate::gitlog::filter_button("problems-options", "Options".into(), move |menu, _| {
            let set = |f: fn(&mut Self, &mut Context<Self>)| {
                let me = me.clone();
                move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                    let _ = me.update(cx, f);
                }
            };
            menu.item(
                PopupMenuItem::new("Group by Severity")
                    .checked(grouped)
                    .on_click(set(|v, cx| {
                        v.shown.by_severity = !v.shown.by_severity;
                        v.refresh(cx)
                    })),
            )
            .item(
                PopupMenuItem::new("Sort by Name")
                    .checked(named)
                    .on_click(set(|v, cx| {
                        v.shown.by_name = !v.shown.by_name;
                        v.refresh(cx)
                    })),
            )
            .separator()
            .item(PopupMenuItem::new("Expand All").on_click(set(|v, cx| v.fold_all(false, cx))))
            .item(PopupMenuItem::new("Collapse All").on_click(set(|v, cx| v.fold_all(true, cx))))
        })
    }

    fn row(&self, i: usize, p: &Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        use gpui_component::menu::ContextMenuExt;
        let me = cx.entity().downgrade();
        let row = self.rows[i].clone();
        let el = self.row_body(i, p, cx);
        el.context_menu(move |menu, _, cx| {
            crate::ui::modal(cx);
            Self::menu(me.clone(), i, &row, menu)
        })
        .into_any_element()
    }

    fn row_body(&self, i: usize, p: &Palette, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let key = self.rows[i].key();
        let folded = key.as_ref().is_some_and(|k| self.shown.folded.contains(k));
        let fold =
            move |v: &mut Self, _: &gpui::ClickEvent, _: &mut Window, cx: &mut Context<Self>| {
                if let Some(k) = key.clone() {
                    v.fold(k, cx)
                }
            };
        match &self.rows[i] {
            Row::Severity { rank, count } => h_flex()
                .id(("problem-severity", i))
                .h(px(ROW))
                .w_full()
                .gap(px(6.))
                .px(px(8.))
                .cursor_pointer()
                .hover(|s| s.bg(p.surface_2))
                .on_click(cx.listener(fold))
                .child(crate::project::chevron(!folded, p))
                .child(
                    div()
                        .flex_none()
                        .size(px(7.))
                        .rounded(px(4.))
                        .bg(colour_of(*rank, p)),
                )
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(severity_name(*rank)),
                )
                .child(div().text_color(p.muted).child(count.to_string())),
            Row::File {
                abs,
                errors,
                warnings,
                count,
                group,
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
                let tally = if group.is_some() {
                    format!("{count}")
                } else {
                    tally
                };
                h_flex()
                    .id(("problem-file", i))
                    .h(px(ROW))
                    .w_full()
                    .gap(px(6.))
                    .pr(px(8.))
                    .pl(px(if group.is_some() { 20. } else { 8. }))
                    .cursor_pointer()
                    .hover(|s| s.bg(p.surface_2))
                    .on_click(cx.listener(fold))
                    .child(crate::project::chevron(!folded, p))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(name.to_string()),
                    )
                    .child(div().min_w_0().truncate().text_color(p.muted).child(shown))
                    .child(div().text_color(p.muted).child(tally))
            }
            Row::Problem { d, .. } => {
                let selected = self.selected == Some(i);
                let c = colour(d.severity, p);
                let message = d.message.lines().next().unwrap_or_default().to_string();
                let source = d.source.clone().unwrap_or_default();
                let depth = match (self.shown.by_severity, self.shown.only.is_some()) {
                    (true, false) => 2.,
                    (false, _) | (true, true) => 1.,
                };
                h_flex()
                    .id(("problem", i))
                    .h(px(ROW))
                    .w_full()
                    .gap(px(8.))
                    .pl(px(8. + 12. * depth))
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
        let (errors, warnings, files) = self.totals;
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
            .child(self.options(cx))
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
