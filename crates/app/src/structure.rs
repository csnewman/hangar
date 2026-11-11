//! The Structure tool window: the outline of the file shown, as its
//! language server gives it (textDocument/documentSymbol), or a Markdown
//! file's headings read from its text, read again as the file is edited.
//! As IntelliJ's: a symbol clicked puts the caret on it, the symbol the
//! caret is in is marked and kept in view, symbols with others in them
//! fold, and the outline can be sorted by name.

use std::collections::HashSet;
use std::ops::Range;
use std::time::Duration;

use gpui::{
    Context, Entity, FontWeight, Hsla, IntoElement, ParentElement, Render, ScrollStrategy, Styled,
    UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};
use gpui_component::input::Rope;
use gpui_component::{h_flex, v_flex};
use serde_json::{Value, json};

use crate::docedit::DocEditor;
use crate::languages::{Languages, file_uri};
use crate::palette::{self, Palette};

/// A symbol in the outline, at its depth: its name, what it is, what its
/// server adds (a function's signature), where its name is (0-based, the
/// column in UTF-16 units) and the lines it spans, its body included.
#[derive(Debug, Clone, PartialEq)]
pub struct Symbol {
    pub depth: usize,
    pub name: String,
    pub kind: &'static str,
    pub detail: Option<String>,
    pub line: u32,
    pub column: u32,
    pub lines: Range<u32>,
    /// Its own and its parents' kinds and names, which name it across
    /// reads.
    pub path: String,
}

/// A documentSymbol answer as the outline's rows: nested DocumentSymbols
/// in order, each under its parent, or flat SymbolInformation, each under
/// the one whose range holds it.
pub fn outline(answer: &Value) -> Vec<Symbol> {
    let mut out = vec![];
    let Some(list) = answer.as_array() else {
        return out;
    };
    if list.iter().all(|s| s["selectionRange"].is_object()) {
        for s in list {
            walk(s, 0, "", &mut out);
        }
        return out;
    }
    let mut flat: Vec<Symbol> = list.iter().map(|s| one(s, 0, "")).collect();
    flat.sort_by_key(|s| (s.lines.start, std::cmp::Reverse(s.lines.end), s.column));
    // The symbols holding the one placed, outermost first.
    let mut holding: Vec<usize> = vec![];
    for mut s in flat {
        while let Some(&top) = holding.last() {
            let t: &Symbol = &out[top];
            if t.lines.start <= s.lines.start && s.lines.end <= t.lines.end {
                break;
            }
            holding.pop();
        }
        s.depth = holding.len();
        if let Some(&top) = holding.last() {
            s.path = format!("{}{}", out[top].path, s.path);
        }
        out.push(s);
        holding.push(out.len() - 1);
    }
    out
}

fn walk(s: &Value, depth: usize, parent: &str, out: &mut Vec<Symbol>) {
    let symbol = one(s, depth, parent);
    let path = symbol.path.clone();
    out.push(symbol);
    for c in s["children"].as_array().into_iter().flatten() {
        walk(c, depth + 1, &path, out);
    }
}

/// One symbol, at a depth under a parent's path.
fn one(s: &Value, depth: usize, parent: &str) -> Symbol {
    // DocumentSymbol names its place by selectionRange and spans range,
    // SymbolInformation has its location alone.
    let (start, range) = if s["selectionRange"].is_object() {
        (&s["selectionRange"]["start"], &s["range"])
    } else {
        (&s["location"]["range"]["start"], &s["location"]["range"])
    };
    let kind = kind(s["kind"].as_u64().unwrap_or(0));
    let name = s["name"].as_str().unwrap_or_default();
    // rust-analyzer names an impl block "impl Ledger", which its kind
    // already says.
    let name = name
        .strip_prefix(kind)
        .and_then(|n| n.strip_prefix(' '))
        .filter(|_| !kind.is_empty())
        .unwrap_or(name);
    let line = |v: &Value| v["line"].as_u64().unwrap_or(0) as u32;
    let first = line(&range["start"]);
    Symbol {
        depth,
        name: name.to_string(),
        kind,
        detail: s["detail"]
            .as_str()
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(str::to_string),
        line: line(start),
        column: start["character"].as_u64().unwrap_or(0) as u32,
        lines: first..line(&range["end"]).max(first) + 1,
        path: format!("{parent}/{kind}:{name}"),
    }
}

/// LSP's SymbolKind as a word.
fn kind(k: u64) -> &'static str {
    match k {
        1 => "file",
        2..=4 => "mod",
        5 => "class",
        6 | 12 => "fn",
        7 | 8 => "field",
        9 => "new",
        10 => "enum",
        11 => "trait",
        13 => "var",
        14 => "const",
        15 => "string",
        // rust-analyzer gives an impl block as Object.
        19 => "impl",
        22 => "variant",
        23 => "struct",
        26 => "type",
        _ => "",
    }
}

/// A Markdown file's headings as its outline, each under the one before
/// it of a higher level; fenced code is passed over.
pub fn markdown(text: &str) -> Vec<Symbol> {
    let mut out: Vec<Symbol> = vec![];
    // The headings open above the current line: their level and row.
    let mut open: Vec<(usize, usize)> = vec![];
    let mut fence: Option<&str> = None;
    let lines: Vec<&str> = text.lines().collect();
    for (n, raw) in lines.iter().enumerate() {
        let line = raw.trim_start();
        let indent = raw.len() - line.len();
        if let Some(f) = fence {
            if line.starts_with(f) {
                fence = None;
            }
            continue;
        }
        if indent < 4 && (line.starts_with("```") || line.starts_with("~~~")) {
            fence = Some(&line[..3]);
            continue;
        }
        let level = line.chars().take_while(|c| *c == '#').count();
        let rest = &line[level..];
        if indent >= 4 || !(1..=6).contains(&level) || !(rest.is_empty() || rest.starts_with(' ')) {
            continue;
        }
        let name = rest.trim().trim_end_matches('#').trim_end();
        while open.last().is_some_and(|(l, _)| *l >= level) {
            let (_, row) = open.pop().unwrap_or_default();
            out[row].lines.end = n as u32;
        }
        let path = match open.last() {
            Some((_, row)) => format!("{}/{name}", out[*row].path),
            None => format!("/{name}"),
        };
        out.push(Symbol {
            depth: open.len(),
            name: name.to_string(),
            kind: "heading",
            detail: None,
            line: n as u32,
            column: (indent + level) as u32,
            lines: n as u32..n as u32 + 1,
            path,
        });
        open.push((level, out.len() - 1));
    }
    for (_, row) in open {
        out[row].lines.end = lines.len() as u32;
    }
    out
}

/// Whether a file's outline comes from its text: Markdown's.
pub fn is_markdown(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".md", ".markdown", ".mdx"]
        .iter()
        .any(|e| lower.ends_with(e))
}

/// The symbol a line is in: the innermost spanning it.
pub fn at_line(symbols: &[Symbol], line: u32) -> Option<usize> {
    symbols
        .iter()
        .enumerate()
        .filter(|(_, s)| s.lines.contains(&line))
        .max_by_key(|(i, s)| (s.depth, *i))
        .map(|(i, _)| i)
}

/// Whether a symbol has others in it.
pub fn has_children(symbols: &[Symbol], i: usize) -> bool {
    symbols
        .get(i + 1)
        .is_some_and(|n| n.depth > symbols[i].depth)
}

/// The rows shown, by index: each symbol under its parent, those in a
/// folded one left out; sorted, each level by name.
pub fn visible(symbols: &[Symbol], folded: &HashSet<String>, sorted: bool) -> Vec<usize> {
    // The symbols directly in one (none: the file), in order.
    let children = |parent: Option<usize>| -> Vec<usize> {
        let (from, depth) = match parent {
            Some(p) => (p + 1, symbols[p].depth + 1),
            None => (0, 0),
        };
        let mut out = vec![];
        for (i, s) in symbols.iter().enumerate().skip(from) {
            if s.depth < depth {
                break;
            }
            if s.depth == depth {
                out.push(i);
            }
        }
        out
    };
    let mut out = vec![];
    let mut stack: Vec<usize> = {
        let mut top = children(None);
        if sorted {
            top.sort_by_key(|i| symbols[*i].name.to_lowercase());
        }
        top.into_iter().rev().collect()
    };
    while let Some(i) = stack.pop() {
        out.push(i);
        if folded.contains(&symbols[i].path) {
            continue;
        }
        let mut kids = children(Some(i));
        if sorted {
            kids.sort_by_key(|k| symbols[*k].name.to_lowercase());
        }
        stack.extend(kids.into_iter().rev());
    }
    out
}

/// Where the outline is.
#[derive(Debug, Clone, PartialEq)]
enum State {
    NoFile,
    Reading,
    Read,
    Failed(String),
}

pub struct StructureView {
    languages: Entity<Languages>,
    doc: Option<Entity<DocEditor>>,
    /// The text the outline was last read for, so it is read again only
    /// when the text changes.
    read: Option<Rope>,
    symbols: Vec<Symbol>,
    state: State,
    /// Counts the reads started, so an older answer is dropped.
    generation: u64,
    /// The symbols folded, by path; whether the outline is sorted by name;
    /// whether the caret's symbol is kept in view.
    folded: HashSet<String>,
    sorted: bool,
    follow: bool,
    shown: Vec<usize>,
    /// The caret's symbol last scrolled to, by path.
    followed: Option<String>,
    list: UniformListScrollHandle,
    _watch: Option<gpui::Subscription>,
}

impl StructureView {
    pub fn new(languages: Entity<Languages>) -> Self {
        StructureView {
            languages,
            doc: None,
            read: None,
            symbols: vec![],
            state: State::NoFile,
            generation: 0,
            folded: HashSet::new(),
            sorted: false,
            follow: true,
            shown: vec![],
            followed: None,
            list: UniformListScrollHandle::new(),
            _watch: None,
        }
    }

    pub fn symbols(&self) -> &[Symbol] {
        &self.symbols
    }

    /// Shows the outline of the file shown, following its edits and its
    /// caret.
    pub fn follow(&mut self, doc: Option<Entity<DocEditor>>, cx: &mut Context<Self>) {
        if doc.as_ref().map(|d| d.entity_id()) == self.doc.as_ref().map(|d| d.entity_id()) {
            return;
        }
        self.doc = doc.clone();
        self.read = None;
        self.symbols.clear();
        self.shown.clear();
        self.folded.clear();
        self.followed = None;
        self._watch = doc.as_ref().map(|d| {
            cx.observe(d, |v, _, cx| {
                v.soon(cx);
                cx.notify();
            })
        });
        self.state = if doc.is_some() {
            State::Reading
        } else {
            State::NoFile
        };
        self.soon(cx);
        cx.notify();
    }

    /// Reads the outline once edits pause, if the text changed.
    fn soon(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(400))
                .await;
            let _ = this.update(cx, |v, cx| {
                if v.generation == generation {
                    v.refresh(cx);
                }
            });
        })
        .detach();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(doc) = self.doc.clone() else {
            return;
        };
        let text = doc.read(cx).text_and_caret(cx).0;
        if self.read.as_ref() == Some(&text) {
            return;
        }
        let abs = doc.read(cx).path().to_string();
        if is_markdown(&abs) {
            self.set_symbols(markdown(&text.to_string()), cx);
            self.read = Some(text);
            return;
        }
        self.read = Some(text);
        let asked = self.languages.update(cx, |l, cx| {
            l.request(
                &abs,
                "textDocument/documentSymbol",
                json!({ "textDocument": { "uri": file_uri(&abs) } }),
                cx,
            )
        });
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            let answer = asked.await;
            let _ = this.update(cx, |v, cx| {
                if v.generation != generation {
                    return;
                }
                match answer {
                    Ok(a) => v.set_symbols(outline(&a), cx),
                    Err(e) => {
                        // Read again on the next edit.
                        v.read = None;
                        v.state = State::Failed(e);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn set_symbols(&mut self, symbols: Vec<Symbol>, cx: &mut Context<Self>) {
        self.symbols = symbols;
        self.state = State::Read;
        self.reshown();
        cx.notify();
    }

    fn reshown(&mut self) {
        self.shown = visible(&self.symbols, &self.folded, self.sorted);
    }

    fn fold(&mut self, path: &str, cx: &mut Context<Self>) {
        if !self.folded.remove(path) {
            self.folded.insert(path.to_string());
        }
        self.reshown();
        cx.notify();
    }

    /// Folds every symbol with others in it, or none.
    fn fold_all(&mut self, fold: bool, cx: &mut Context<Self>) {
        self.folded = if fold {
            (0..self.symbols.len())
                .filter(|i| has_children(&self.symbols, *i))
                .map(|i| self.symbols[i].path.clone())
                .collect()
        } else {
            HashSet::new()
        };
        self.reshown();
        cx.notify();
    }

    fn go(&self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(doc), Some(s)) = (self.doc.clone(), self.symbols.get(i)) else {
            return;
        };
        let (line, column) = (s.line, s.column);
        doc.update(cx, |d, cx| {
            d.go_to(line, column, window, cx);
            window.focus(&gpui::Focusable::focus_handle(d, cx), cx);
        });
    }

    /// The symbol the caret is in, by index.
    fn at_caret(&self, cx: &gpui::App) -> Option<usize> {
        let line = self.doc.as_ref()?.read(cx).position(cx).0.saturating_sub(1);
        at_line(&self.symbols, line)
    }

    fn row(
        &self,
        i: usize,
        caret: Option<usize>,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let s = &self.symbols[i];
        let folds = has_children(&self.symbols, i);
        let folded = self.folded.contains(&s.path);
        let path = s.path.clone();
        let (letter, colour) = badge(s.kind, p);
        h_flex()
            .id(("symbol", i))
            .w_full()
            .h(px(ROW))
            .gap(px(5.))
            .pl(px(4. + 14. * s.depth as f32))
            .pr(px(6.))
            .rounded(px(4.))
            .cursor_pointer()
            .when(caret == Some(i), |d| d.bg(p.selection))
            .when(caret != Some(i), |d| d.hover(|d| d.bg(p.surface_2)))
            .child(
                div()
                    .id(("symbol-fold", i))
                    .w(px(12.))
                    .flex_none()
                    .when(folds, |d| {
                        d.child(crate::project::chevron(!folded, p))
                            .on_click(cx.listener(move |v, _, _, cx| {
                                v.fold(&path, cx);
                                cx.stop_propagation();
                            }))
                    }),
            )
            .child(
                div()
                    .size(px(14.))
                    .flex_none()
                    .rounded(px(3.))
                    .bg(colour.opacity(0.16))
                    .text_color(colour)
                    .text_size(px(9.5))
                    .font_weight(FontWeight::BOLD)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(letter),
            )
            .child(
                div()
                    .flex_none()
                    .max_w(relative_name())
                    .truncate()
                    .when(s.depth == 0, |d| d.font_weight(FontWeight::MEDIUM))
                    .child(s.name.clone()),
            )
            .when_some(s.detail.clone(), |d, detail| {
                d.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(p.muted)
                        .text_size(px(11.))
                        .child(detail),
                )
            })
            .tooltip({
                let tip = match &s.detail {
                    Some(d) => format!("{} {} {d}", s.kind, s.name),
                    None => format!("{} {}", s.kind, s.name),
                };
                move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                }
            })
            .on_click(cx.listener(move |v, _, window, cx| v.go(i, window, cx)))
            .into_any_element()
    }
}

const ROW: f32 = 20.;

/// The widest a name is drawn, its detail taking the rest.
fn relative_name() -> gpui::DefiniteLength {
    gpui::relative(0.7)
}

/// A kind's badge, as IntelliJ marks its symbols: a letter and a colour.
fn badge(kind: &str, p: &Palette) -> (&'static str, Hsla) {
    match kind {
        "struct" | "class" => ("C", p.accent),
        "enum" => ("E", p.wait),
        "variant" => ("e", p.wait),
        "trait" => ("I", p.good),
        "impl" => ("i", p.good),
        "fn" | "new" => ("f", p.peer),
        "field" => ("p", p.busy),
        "var" | "string" => ("v", p.busy),
        "const" => ("c", p.busy),
        "type" => ("T", p.accent),
        "mod" | "file" => ("M", p.muted),
        "heading" => ("#", p.muted),
        _ => ("•", p.muted),
    }
}

impl Render for StructureView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let note = |t: String| {
            div()
                .px(px(8.))
                .text_size(px(11.5))
                .text_color(p.muted)
                .child(t)
                .into_any_element()
        };
        let caret = self.at_caret(cx);
        // The caret's symbol kept in view as it moves to another.
        if self.follow {
            let path = caret.map(|i| self.symbols[i].path.clone());
            if path != self.followed {
                if let Some(at) = caret.and_then(|c| self.shown.iter().position(|r| *r == c)) {
                    self.list.scroll_to_item(at, ScrollStrategy::Center);
                }
                self.followed = path;
            }
        }
        let body = match &self.state {
            State::NoFile => note("Open a file to see its outline.".into()),
            State::Reading if self.symbols.is_empty() => note("Reading…".into()),
            State::Failed(e) => note(e.clone()),
            _ if self.symbols.is_empty() => note(
                if self
                    .doc
                    .as_ref()
                    .is_some_and(|d| is_markdown(d.read(cx).path()))
                {
                    "It has no headings."
                } else {
                    "Its language server lists nothing."
                }
                .into(),
            ),
            _ => uniform_list(
                "structure",
                self.shown.len(),
                cx.processor(move |v, range: Range<usize>, _, cx| {
                    let p = palette::of(cx);
                    range
                        .filter_map(|at| v.shown.get(at).copied())
                        .map(|i| v.row(i, caret, &p, cx))
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.list)
            .flex_1()
            .into_any_element(),
        };
        let tool = |id: &'static str, label: &'static str, on: bool, tip: &'static str| {
            crate::gitlog::chip(id, label.to_string(), on, &p).tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(tip).build(window, cx)
            })
        };
        let bar = h_flex()
            .flex_none()
            .gap(px(4.))
            .px(px(6.))
            .h(px(26.))
            .child(
                tool("structure-sort", "A–Z", self.sorted, "Sort Alphabetically").on_click(
                    cx.listener(|v, _, _, cx| {
                        v.sorted = !v.sorted;
                        v.reshown();
                        cx.notify();
                    }),
                ),
            )
            .child(
                tool(
                    "structure-follow",
                    "Follow Caret",
                    self.follow,
                    "Autoscroll from Source: keep the caret's symbol in view",
                )
                .on_click(cx.listener(|v, _, _, cx| {
                    v.follow = !v.follow;
                    v.followed = None;
                    cx.notify();
                })),
            )
            .child(div().flex_1())
            .child(
                tool("structure-expand", "Expand All", false, "Expand All")
                    .on_click(cx.listener(|v, _, _, cx| v.fold_all(false, cx))),
            )
            .child(
                tool("structure-collapse", "Collapse All", false, "Collapse All")
                    .on_click(cx.listener(|v, _, _, cx| v.fold_all(true, cx))),
            );
        v_flex()
            .size_full()
            .min_h_0()
            .child(bar)
            .child(v_flex().flex_1().min_h_0().child(body))
    }
}

/// The outline's symbols as places to go, for File Structure's pop-up:
/// each one's name, what it is in, and where.
pub fn places(symbols: &[Symbol]) -> Vec<(String, String, u32, u32)> {
    symbols
        .iter()
        .enumerate()
        .map(|(i, s)| {
            // Its parents' names, outermost first.
            let mut parents = vec![];
            let mut depth = s.depth;
            for p in symbols[..i].iter().rev() {
                if depth == 0 {
                    break;
                }
                if p.depth < depth {
                    parents.push(p.name.as_str());
                    depth = p.depth;
                }
            }
            parents.reverse();
            let within = parents.join(" › ");
            let what = match (&s.detail, within.is_empty()) {
                (Some(d), true) => format!("{} · {d}", s.kind),
                (Some(d), false) => format!("{within} · {d}"),
                (None, true) => s.kind.to_string(),
                (None, false) => within,
            };
            (s.name.clone(), what, s.line, s.column)
        })
        .collect()
}
