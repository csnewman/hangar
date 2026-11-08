//! The Structure tool window: the outline of the file shown, as its
//! language server gives it (textDocument/documentSymbol), read again as
//! the file is edited; a symbol clicked puts the caret on it.

use std::time::Duration;

use gpui::{
    Context, Entity, FontWeight, IntoElement, ParentElement, Render, SharedString, Styled, Window,
    div, prelude::*, px,
};
use gpui_component::input::Rope;
use gpui_component::{h_flex, v_flex};
use serde_json::{Value, json};

use crate::docedit::DocEditor;
use crate::languages::{Languages, file_uri};
use crate::palette;

/// A symbol in the outline, at its depth: its name, what it is, and where
/// its name is (0-based, the column in UTF-16 units).
#[derive(Debug, Clone, PartialEq)]
pub struct Symbol {
    pub depth: usize,
    pub name: String,
    pub kind: &'static str,
    pub line: u32,
    pub column: u32,
}

/// A documentSymbol answer as the outline's rows: nested DocumentSymbols
/// in order, each under its parent, or flat SymbolInformation.
pub fn outline(answer: &Value) -> Vec<Symbol> {
    let mut out = vec![];
    if let Some(list) = answer.as_array() {
        for s in list {
            walk(s, 0, &mut out);
        }
    }
    out
}

fn walk(s: &Value, depth: usize, out: &mut Vec<Symbol>) {
    // DocumentSymbol names its place by selectionRange, SymbolInformation
    // by location.
    let start = if s["selectionRange"].is_object() {
        &s["selectionRange"]["start"]
    } else {
        &s["location"]["range"]["start"]
    };
    out.push(Symbol {
        depth,
        name: s["name"].as_str().unwrap_or_default().to_string(),
        kind: kind(s["kind"].as_u64().unwrap_or(0)),
        line: start["line"].as_u64().unwrap_or(0) as u32,
        column: start["character"].as_u64().unwrap_or(0) as u32,
    });
    for c in s["children"].as_array().into_iter().flatten() {
        walk(c, depth + 1, out);
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
        22 => "variant",
        23 => "struct",
        26 => "type",
        _ => "",
    }
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
            _watch: None,
        }
    }

    pub fn symbols(&self) -> &[Symbol] {
        &self.symbols
    }

    /// Shows the outline of the file shown, following its edits.
    pub fn follow(&mut self, doc: Option<Entity<DocEditor>>, cx: &mut Context<Self>) {
        if doc.as_ref().map(|d| d.entity_id()) == self.doc.as_ref().map(|d| d.entity_id()) {
            return;
        }
        self.doc = doc.clone();
        self.read = None;
        self.symbols.clear();
        self._watch = doc.as_ref().map(|d| cx.observe(d, |v, _, cx| v.soon(cx)));
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
        self.read = Some(text);
        let abs = doc.read(cx).path().to_string();
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
                    Ok(a) => {
                        v.symbols = outline(&a);
                        v.state = State::Read;
                    }
                    Err(e) => {
                        // Read again on the next edit.
                        v.read = None;
                        v.state = State::Failed(e);
                    }
                }
                cx.notify();
            });
        })
        .detach();
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
}

impl Render for StructureView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let note = |t: String| {
            div()
                .text_size(px(11.5))
                .text_color(p.muted)
                .child(t)
                .into_any_element()
        };
        let body = match &self.state {
            State::NoFile => note("Open a file to see its outline.".into()),
            State::Reading if self.symbols.is_empty() => note("Reading…".into()),
            State::Failed(e) => note(e.clone()),
            _ if self.symbols.is_empty() => note("Its language server lists nothing.".into()),
            _ => {
                let mut list = v_flex().id("structure").overflow_y_scroll().size_full();
                for (i, s) in self.symbols.iter().enumerate() {
                    let hover = p.surface_2;
                    list = list.child(
                        h_flex()
                            .id(("symbol", i))
                            .h(px(20.))
                            .gap(px(6.))
                            .pl(px(4. + 12. * s.depth as f32))
                            .rounded(px(4.))
                            .cursor_pointer()
                            .hover(move |d| d.bg(hover))
                            .child(
                                div()
                                    .w(px(38.))
                                    .flex_none()
                                    .text_size(px(10.5))
                                    .text_color(p.accent)
                                    .child(SharedString::from(s.kind)),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .when(s.depth == 0, |d| d.font_weight(FontWeight::MEDIUM))
                                    .child(s.name.clone()),
                            )
                            .on_click(cx.listener(move |v, _, window, cx| v.go(i, window, cx))),
                    );
                }
                list.into_any_element()
            }
        };
        v_flex().size_full().min_h_0().child(body)
    }
}
