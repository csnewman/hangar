//! Find in Files as IntelliJ's pop-up (⌘⇧F): what to find, with match
//! case, whole words and a regular expression; where, every project, one of
//! them or a folder in one; an optional file mask; the matching lines as
//! they are found, and the chosen one in its file. Enter opens it, and
//! ⌘Enter keeps every match in the Find window to go through. Replace in
//! Files (⌘⇧R) is the same pop-up with a replacement, put in the chosen
//! match's file or in every file.

use std::ops::Range;
use std::time::Duration;

use gpui::{
    ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, HighlightStyle,
    IntoElement, KeyDownEvent, ParentElement, Render, ScrollStrategy, SharedString, Styled,
    StyledText, UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Sizable, h_flex, v_flex};
use serde_json::json;

use crate::find::{
    Match, OpenAt, Options, Row, Scope, Search, byte_ranges, rows, scope_name, search_text,
};
use crate::gitlog::chip;
use crate::palette::{self, Palette};
use crate::ui;
use crate::workspace::Workspace;

/// What the pop-up asks of the window holding it.
#[derive(Debug, Clone, PartialEq)]
pub enum Done {
    Open(OpenAt),
    /// Every match, kept in the Find window.
    ToWindow(Search),
    Close,
}

impl EventEmitter<Done> for FindPopup {}

/// Where to look, as the pop-up offers it: a folder is typed.
#[derive(Debug, Clone, PartialEq)]
enum Where {
    Projects,
    Project(String),
    Folder,
}

/// The chosen match's file, once read.
struct Preview {
    root: String,
    path: String,
    text: Option<Result<Option<String>, String>>,
}

pub struct FindPopup {
    workspace: Entity<Workspace>,
    query: Entity<InputState>,
    mask: Entity<InputState>,
    folder: Entity<InputState>,
    replacement: Entity<InputState>,
    /// Whether it replaces, as Replace in Files.
    replacing: bool,
    /// Replace All was pressed once, and asks to be pressed again.
    confirming: bool,
    /// What the last replacement did.
    replaced: Option<String>,
    options: Options,
    masked: bool,
    place: Where,
    /// The matching lines, each with its project's root.
    found: Vec<(String, Match)>,
    files: usize,
    truncated: bool,
    /// Projects still searching, and why any could not.
    pending: usize,
    errors: Vec<String>,
    /// Counts the searches started, so an older one's answer is dropped.
    generation: u64,
    selected: usize,
    preview: Option<Preview>,
    list: UniformListScrollHandle,
    _subs: Vec<gpui::Subscription>,
}

impl FindPopup {
    /// Opens on the search last made, if any, as IntelliJ does.
    pub fn new(
        workspace: Entity<Workspace>,
        last: Option<Search>,
        replacing: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let last = last.unwrap_or_default();
        let query = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Text to find")
                .default_value(last.text.clone())
        });
        let mask = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("*.rs, !*_test.go")
                .default_value(last.mask.clone().unwrap_or_default())
        });
        let (place, folder) = match &last.scope {
            Scope::Projects => (Where::Projects, String::new()),
            Scope::Project(root) => (Where::Project(root.clone()), String::new()),
            Scope::Folder { root, dir } => (
                Where::Folder,
                format!("{}/{dir}", root.trim_end_matches('/')),
            ),
        };
        let folder = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("A folder: src, or /home/dev/project/src")
                .default_value(folder)
        });
        let replacement = cx.new(|cx| InputState::new(window, cx).placeholder("Replace with"));
        let soon = |v: &mut Self, _: Entity<InputState>, e: &InputEvent, cx: &mut Context<Self>| {
            if let InputEvent::Change = e {
                v.confirming = false;
                v.replaced = None;
                v.search_soon(cx);
            }
        };
        let subs = vec![
            cx.subscribe(&query, soon),
            cx.subscribe(&mask, soon),
            cx.subscribe(&folder, soon),
            cx.subscribe(&replacement, |v, _, e: &InputEvent, cx| {
                if let InputEvent::Change = e {
                    v.confirming = false;
                    cx.notify();
                }
            }),
        ];
        let mut popup = FindPopup {
            workspace,
            query,
            mask,
            folder,
            replacement,
            replacing,
            confirming: false,
            replaced: None,
            options: last.options,
            masked: last.mask.is_some_and(|m| !m.trim().is_empty()),
            place,
            found: vec![],
            files: 0,
            truncated: false,
            pending: 0,
            errors: vec![],
            generation: 0,
            selected: 0,
            preview: None,
            list: UniformListScrollHandle::new(),
            _subs: subs,
        };
        popup.search(cx);
        popup
    }

    /// Searches for text, as if it had been typed.
    pub fn set_query(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.query
            .update(cx, |q, cx| q.set_value(text.to_string(), window, cx));
        self.search(cx);
    }

    pub fn found(&self) -> &[(String, Match)] {
        &self.found
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The search the pop-up stands for, or why it cannot be made.
    pub fn current(&self, cx: &gpui::App) -> Result<Search, String> {
        let ws = self.workspace.read(cx);
        let roots: Vec<&str> = ws.projects().iter().map(|f| f.root.as_str()).collect();
        let scope = match &self.place {
            Where::Projects => Scope::Projects,
            Where::Project(root) => Scope::Project(root.clone()),
            Where::Folder => Scope::folder(&self.folder.read(cx).value(), &roots, None)?,
        };
        Ok(Search {
            text: self.query.read(cx).value().to_string(),
            options: self.options,
            mask: self
                .masked
                .then(|| self.mask.read(cx).value().trim().to_string()),
            scope,
        })
    }

    /// Searches once typing pauses.
    fn search_soon(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(200))
                .await;
            let _ = this.update(cx, |v, cx| {
                if v.generation == generation {
                    v.search(cx);
                }
            });
        })
        .detach();
    }

    /// Searches for what the pop-up says, as it is now.
    pub fn search(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        self.found.clear();
        self.files = 0;
        self.truncated = false;
        self.errors.clear();
        self.pending = 0;
        self.selected = 0;
        self.preview = None;
        let search = match self.current(cx) {
            Ok(s) if !s.text.is_empty() => s,
            Ok(_) => {
                cx.notify();
                return;
            }
            Err(e) => {
                self.errors.push(e);
                cx.notify();
                return;
            }
        };
        let calls: Vec<_> = search
            .calls(self.workspace.read(cx).projects())
            .into_iter()
            .map(|(root, code, params)| (root, search_text(code, params)))
            .collect();
        self.pending = calls.len();
        cx.spawn(async move |this, cx| {
            let mut per: Vec<(String, Vec<Match>)> = Vec::new();
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
                            per.push((root, matches));
                            let rows = rows(&per);
                            v.files = rows
                                .iter()
                                .filter(|r| matches!(r, Row::File { .. }))
                                .count();
                            v.found = rows
                                .into_iter()
                                .filter_map(|r| match r {
                                    Row::Line { root, m } => Some((root, m)),
                                    Row::File { .. } => None,
                                })
                                .collect();
                            v.show_selected(cx);
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

    /// Reads the chosen match's file for its preview, unless it is shown.
    fn show_selected(&mut self, cx: &mut Context<Self>) {
        let Some((root, m)) = self.found.get(self.selected) else {
            self.preview = None;
            return;
        };
        if self
            .preview
            .as_ref()
            .is_some_and(|p| &p.root == root && p.path == m.path)
        {
            return;
        }
        let (root, path) = (root.clone(), m.path.clone());
        self.preview = Some(Preview {
            root: root.clone(),
            path: path.clone(),
            text: None,
        });
        let Some(code) = self.workspace.read(cx).code(&root) else {
            return;
        };
        let call = code.call("fs.read", json!({ "path": path }));
        cx.spawn(async move |this, cx| {
            let text = call
                .await
                .map(|v| v.get("text").and_then(|t| t.as_str()).map(str::to_string));
            let _ = this.update(cx, |v, cx| {
                if let Some(p) = &mut v.preview
                    && p.root == root
                    && p.path == path
                {
                    p.text = Some(text);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub fn select(&mut self, i: usize, cx: &mut Context<Self>) {
        if i < self.found.len() {
            self.selected = i;
            self.list.scroll_to_item(i, ScrollStrategy::Center);
            self.show_selected(cx);
            cx.notify();
        }
    }

    /// Opens the match on row i.
    pub fn open(&mut self, i: usize, cx: &mut Context<Self>) {
        if let Some((root, m)) = self.found.get(i) {
            cx.emit(Done::Open(OpenAt {
                root: root.clone(),
                path: m.path.clone(),
                line: m.line.saturating_sub(1),
                column: m.ranges.first().map_or(0, |r| r[0] as u32),
            }));
        }
    }

    /// Keeps every match in the Find window.
    pub fn to_window(&mut self, cx: &mut Context<Self>) {
        if let Ok(s) = self.current(cx)
            && !s.text.is_empty()
        {
            cx.emit(Done::ToWindow(s));
        }
    }

    fn set_place(&mut self, place: Where, window: &mut Window, cx: &mut Context<Self>) {
        let folder = place == Where::Folder;
        self.place = place;
        if folder {
            window.focus(&self.folder.focus_handle(cx), cx);
        }
        self.search(cx);
    }

    /// Shows or hides the replacement, as Replace in Files or Find in Files.
    pub fn set_replacing(&mut self, replacing: bool, cx: &mut Context<Self>) {
        self.replacing = replacing;
        self.confirming = false;
        cx.notify();
    }

    pub fn replacing(&self) -> bool {
        self.replacing
    }

    /// How many matches there are: a line's row may hold several.
    fn occurrences(&self) -> usize {
        self.found.iter().map(|(_, m)| m.ranges.len().max(1)).sum()
    }

    /// Replaces the matches with the replacement: in one project's file, or
    /// everywhere the search looks. Then searches again, to show what is
    /// left.
    pub fn replace(&mut self, only: Option<(String, String)>, cx: &mut Context<Self>) {
        self.confirming = false;
        let Ok(search) = self.current(cx) else {
            return;
        };
        if search.text.is_empty() {
            return;
        }
        let replacement = self.replacement.read(cx).value().to_string();
        let calls: Vec<_> = search
            .calls(self.workspace.read(cx).projects())
            .into_iter()
            .filter(|(root, _, _)| only.as_ref().is_none_or(|(r, _)| r == root))
            .map(|(_, code, mut params)| {
                params["replacement"] = replacement.clone().into();
                if let Some((_, path)) = &only {
                    params["paths"] = json!([path]);
                }
                code.call("search.replace", params)
            })
            .collect();
        cx.spawn(async move |this, cx| {
            let (mut n, mut files, mut errors) = (0, 0, vec![]);
            for call in calls {
                match call.await {
                    Ok(v) => {
                        n += v["replaced"].as_u64().unwrap_or(0);
                        files += v["files"].as_array().map_or(0, |f| f.len());
                    }
                    Err(e) => errors.push(e),
                }
            }
            let _ = this.update(cx, |v, cx| {
                v.replaced = Some(if !errors.is_empty() {
                    errors.join("\n")
                } else if n == 0 {
                    "Nothing was replaced.".into()
                } else {
                    format!(
                        "Replaced {n} match{} in {files} file{}.",
                        if n == 1 { "" } else { "es" },
                        if files == 1 { "" } else { "s" }
                    )
                });
                v.search(cx);
            });
        })
        .detach();
    }

    fn toggle(&mut self, which: fn(&mut Options) -> &mut bool, cx: &mut Context<Self>) {
        let o = which(&mut self.options);
        *o = !*o;
        self.search(cx);
    }

    fn key(&mut self, e: &KeyDownEvent, cx: &mut Context<Self>) {
        let k = &e.keystroke;
        let n = self.found.len();
        match k.key.as_str() {
            "down" if n > 0 => self.select((self.selected + 1) % n, cx),
            "up" if n > 0 => self.select((self.selected + n - 1) % n, cx),
            "enter" if k.modifiers.platform || k.modifiers.control => self.to_window(cx),
            "enter" => self.open(self.selected, cx),
            "escape" => cx.emit(Done::Close),
            _ => return,
        }
        cx.stop_propagation();
    }

    fn row(
        &self,
        i: usize,
        several: bool,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let (root, m) = &self.found[i];
        let selected = i == self.selected;
        let lead = m.text.len() - m.text.trim_start().len();
        let text = m.text[lead..].trim_end().replace('\t', "    ");
        let ranges: Vec<Range<usize>> = byte_ranges(&m.text, &m.ranges)
            .into_iter()
            .filter(|r| r.start >= lead && r.end <= lead + text.len())
            .map(|r| r.start - lead..r.end - lead)
            .collect();
        let styled = StyledText::new(SharedString::from(text))
            .with_highlights(ranges.into_iter().map(|r| (r, hit(p))));
        let name = m.path.rsplit('/').next().unwrap_or(&m.path).to_string();
        let project = several
            .then(|| {
                self.workspace
                    .read(cx)
                    .projects()
                    .iter()
                    .find(|f| &f.root == root)
                    .map(|f| format!("{} · ", f.name))
            })
            .flatten()
            .unwrap_or_default();
        h_flex()
            .id(("find-pop-row", i))
            .h(px(ROW))
            .w_full()
            .gap(px(10.))
            .px(px(10.))
            .rounded(px(4.))
            .cursor_pointer()
            .when(selected, |d| d.bg(p.accent_soft))
            .when(!selected, |d| d.hover(|s| s.bg(p.surface_2)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .font_family(MONO)
                    .text_size(px(11.5))
                    .child(styled),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(11.))
                    .text_color(p.muted)
                    .child(format!("{project}{name} {}", m.line)),
            )
            .on_click(cx.listener(move |v, e: &ClickEvent, _, cx| {
                if e.click_count() >= 2 {
                    v.open(i, cx);
                } else {
                    v.select(i, cx);
                }
            }))
            .into_any_element()
    }

    /// The lines around the chosen match, it marked.
    fn preview(&self, p: &Palette) -> gpui::AnyElement {
        let frame = v_flex()
            .flex_none()
            .h(px(PREVIEW_LINES as f32 * PREVIEW_ROW + 26.))
            .border_t_1()
            .border_color(p.border)
            .bg(p.bg);
        let (Some(pv), Some((_, m))) = (&self.preview, self.found.get(self.selected)) else {
            return frame.into_any_element();
        };
        let head = div()
            .px(px(10.))
            .py(px(4.))
            .text_size(px(11.))
            .text_color(p.muted)
            .truncate()
            .child(pv.path.clone());
        let body = match &pv.text {
            None => note("Reading…", p),
            Some(Err(e)) => note(e.clone(), p),
            Some(Ok(None)) => note("The file cannot be shown.", p),
            Some(Ok(Some(text))) => {
                let lines: Vec<&str> = text.lines().collect();
                let at = m.line as usize;
                let half = PREVIEW_LINES / 2;
                let start = at.saturating_sub(half).max(1);
                let end = (start + PREVIEW_LINES - 1).min(lines.len());
                let mut col = v_flex().font_family(MONO).text_size(px(11.));
                for n in start..=end {
                    let line = lines[n - 1];
                    let here = n == at;
                    let styled = if here && line == m.text && !line.contains('\t') {
                        StyledText::new(SharedString::from(line.to_string())).with_highlights(
                            byte_ranges(line, &m.ranges)
                                .into_iter()
                                .map(|r| (r, hit(p))),
                        )
                    } else {
                        StyledText::new(SharedString::from(line.replace('\t', "    ")))
                    };
                    col = col.child(
                        h_flex()
                            .h(px(PREVIEW_ROW))
                            .when(here, |d| d.bg(p.busy.opacity(0.12)))
                            .child(
                                div()
                                    .w(px(40.))
                                    .flex_none()
                                    .pr(px(10.))
                                    .text_right()
                                    .text_color(p.idle)
                                    .child(n.to_string()),
                            )
                            .child(div().whitespace_nowrap().child(styled)),
                    );
                }
                col.into_any_element()
            }
        };
        frame
            .child(head)
            .child(div().flex_1().min_h_0().overflow_hidden().child(body))
            .into_any_element()
    }
}

const ROW: f32 = 22.;
const PREVIEW_ROW: f32 = 17.;
const PREVIEW_LINES: usize = 9;
const MONO: &str = if cfg!(target_os = "macos") {
    "Menlo"
} else if cfg!(target_os = "windows") {
    "Consolas"
} else {
    "DejaVu Sans Mono"
};

fn hit(p: &Palette) -> HighlightStyle {
    HighlightStyle {
        background_color: Some(p.busy.opacity(0.35)),
        font_weight: Some(FontWeight::SEMIBOLD),
        ..Default::default()
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

impl Focusable for FindPopup {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        self.query.read(cx).focus_handle(cx)
    }
}

impl Render for FindPopup {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let o = self.options;
        let projects: Vec<(String, String)> = self
            .workspace
            .read(cx)
            .projects()
            .iter()
            .map(|f| (f.root.clone(), f.name.clone()))
            .collect();
        let several = projects.len() > 1;
        let toggle = |id: &'static str, label: &'static str, on: bool, tip: &'static str| {
            chip(id, label.to_string(), on, &p).tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(tip).build(window, cx)
            })
        };
        let n = self.found.len();
        let status = if self.query.read(cx).value().is_empty() {
            String::new()
        } else if self.pending > 0 && n == 0 {
            "Searching…".into()
        } else {
            format!(
                "{n}{} match{} in {} file{}",
                if self.truncated { "+" } else { "" },
                if n == 1 { "" } else { "es" },
                self.files,
                if self.files == 1 { "" } else { "s" },
            )
        };

        let mut places = h_flex().gap(px(4.)).child(
            chip(
                "find-in-projects",
                "In Projects".into(),
                self.place == Where::Projects,
                &p,
            )
            .on_click(cx.listener(|v, _, window, cx| v.set_place(Where::Projects, window, cx))),
        );
        if several {
            for (i, (root, name)) in projects.iter().enumerate() {
                let on = self.place == Where::Project(root.clone());
                let root = root.clone();
                places = places.child(chip(("find-in-project", i), name.clone(), on, &p).on_click(
                    cx.listener(move |v, _, window, cx| {
                        v.set_place(Where::Project(root.clone()), window, cx)
                    }),
                ));
            }
        }
        places = places.child(
            chip(
                "find-in-folder",
                "Directory".into(),
                self.place == Where::Folder,
                &p,
            )
            .on_click(cx.listener(|v, _, window, cx| v.set_place(Where::Folder, window, cx))),
        );

        let head = h_flex()
            .gap(px(10.))
            .px(px(10.))
            .pt(px(9.))
            .pb(px(6.))
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(if self.replacing {
                        "Replace in Files"
                    } else {
                        "Find in Files"
                    }),
            )
            .child(places)
            .child(div().flex_1())
            .child(
                toggle(
                    "find-masked",
                    "File mask",
                    self.masked,
                    "Only the files matching the mask",
                )
                .on_click(cx.listener(|v, _, _, cx| {
                    v.masked = !v.masked;
                    v.search(cx);
                })),
            )
            .child(
                div()
                    .w(px(150.))
                    .flex_none()
                    .child(Input::new(&self.mask).small().disabled(!self.masked)),
            );
        let query = h_flex()
            .gap(px(6.))
            .px(px(10.))
            .pb(px(6.))
            .child(div().flex_1().min_w_0().child(Input::new(&self.query)))
            .child(
                toggle("find-pop-case", "Cc", o.case, "Match case")
                    .on_click(cx.listener(|v, _, _, cx| v.toggle(|o| &mut o.case, cx))),
            )
            .child(
                toggle("find-pop-word", "W", o.word, "Whole words")
                    .on_click(cx.listener(|v, _, _, cx| v.toggle(|o| &mut o.word, cx))),
            )
            .child(
                toggle("find-pop-regex", ".*", o.regex, "Regular expression")
                    .on_click(cx.listener(|v, _, _, cx| v.toggle(|o| &mut o.regex, cx))),
            )
            .child(
                toggle(
                    "find-pop-replace",
                    "Replace",
                    self.replacing,
                    "Replace in Files",
                )
                .on_click(cx.listener(|v, _, _, cx| {
                    let on = !v.replacing;
                    v.set_replacing(on, cx);
                })),
            );
        let current = self.found.get(self.selected).cloned();
        let files = self.files;
        let occurrences = self.occurrences();
        let replace_row = self.replacing.then(|| {
            let in_file = match &current {
                Some((root, m)) if self.pending == 0 => {
                    let only = (root.clone(), m.path.clone());
                    ui::button("find-replace-file", "Replace in File", &p)
                        .on_click(cx.listener(move |v, _, _, cx| v.replace(Some(only.clone()), cx)))
                }
                _ => ui::disabled("find-replace-file", "Replace in File", &p),
            };
            let all = if n == 0 || self.pending > 0 {
                ui::disabled("find-replace-all", "Replace All", &p)
            } else if self.confirming {
                let label = if self.truncated {
                    "Replace every match?".to_string()
                } else {
                    format!(
                        "Replace {occurrences} match{} in {files} file{}?",
                        if occurrences == 1 { "" } else { "es" },
                        if files == 1 { "" } else { "s" }
                    )
                };
                ui::styled_button("find-replace-all", label, p.bad, p.bad, p.on_accent, p.bad)
                    .on_click(cx.listener(|v, _, _, cx| v.replace(None, cx)))
            } else {
                ui::button("find-replace-all", "Replace All", &p).on_click(cx.listener(
                    |v, _, _, cx| {
                        v.confirming = true;
                        cx.notify();
                    },
                ))
            };
            h_flex()
                .gap(px(6.))
                .px(px(10.))
                .pb(px(6.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(&self.replacement)),
                )
                .child(in_file)
                .child(all)
        });
        let summary = h_flex()
            .px(px(12.))
            .pb(px(4.))
            .gap(px(8.))
            .text_size(px(11.))
            .text_color(p.muted)
            .child(status)
            .when_some(self.replaced.clone(), |d, r| d.child(r))
            .when(self.place != Where::Projects, |d| {
                let ws = self.workspace.read(cx);
                let name = match self.current(cx) {
                    Ok(s) => format!("in {}", scope_name(&s.scope, ws.projects())),
                    Err(_) => String::new(),
                };
                d.child(name)
            });

        let list: gpui::AnyElement = if !self.errors.is_empty() && n == 0 {
            note(self.errors.join("\n"), &p)
        } else if n == 0 {
            note(
                if self.query.read(cx).value().is_empty() {
                    "Type the text to find."
                } else if self.pending > 0 {
                    ""
                } else {
                    "Nothing found."
                },
                &p,
            )
        } else {
            uniform_list(
                "find-pop-results",
                n,
                cx.processor(move |v, range: Range<usize>, _, cx| {
                    let p = palette::of(cx);
                    range.map(|i| v.row(i, several, &p, cx)).collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.list)
            .px(px(4.))
            .size_full()
            .into_any_element()
        };

        let foot = h_flex()
            .gap(px(8.))
            .px(px(10.))
            .py(px(6.))
            .border_t_1()
            .border_color(p.border)
            .text_size(px(11.))
            .text_color(p.muted)
            .child(format!(
                "↵ open · {}↵ open in the Find window",
                if cfg!(target_os = "macos") {
                    "⌘"
                } else {
                    "Ctrl+"
                }
            ))
            .child(div().flex_1())
            .child(if n > 0 {
                ui::button("find-pop-window", "Open in Find Window", &p)
                    .on_click(cx.listener(|v, _, _, cx| v.to_window(cx)))
            } else {
                ui::disabled("find-pop-window", "Open in Find Window", &p)
            });

        v_flex()
            .id("find-popup")
            .w(px(780.))
            .h(px(560.))
            .rounded(px(10.))
            .border_1()
            .border_color(p.border_strong)
            .bg(p.surface)
            .shadow_lg()
            .overflow_hidden()
            .text_size(px(12.5))
            // Clicks in the pop-up stay in it, not reaching the shade
            // behind, which closes it.
            .occlude()
            .debug_selector(|| "find-popup".into())
            .capture_key_down(cx.listener(|v, e: &KeyDownEvent, _, cx| v.key(e, cx)))
            .child(head)
            .when(self.place == Where::Folder, |d| {
                d.child(
                    div()
                        .px(px(10.))
                        .pb(px(6.))
                        .child(Input::new(&self.folder).small()),
                )
            })
            .child(query)
            .children(replace_row)
            .child(summary)
            .child(div().flex_1().min_h_0().child(list))
            .child(self.preview(&p))
            .child(foot)
    }
}
