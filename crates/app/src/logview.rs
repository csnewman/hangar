//! A followed log: a program's output as it arrives, split into lines with
//! the escape sequences a terminal would act on taken out, kept to the last
//! MAX_LINES, drawn in the terminal's colours and kept at its end unless
//! scrolled up.

use std::collections::VecDeque;
use std::ops::Range;

use gpui::{
    Context, IntoElement, ListHorizontalSizingBehavior, ParentElement, Render, ScrollEvent,
    SharedString, Styled, UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};

use crate::palette;

/// The most lines a log keeps; the oldest go first.
pub const MAX_LINES: usize = 20_000;

/// Takes out what a terminal would act on rather than show: colours,
/// cursor movement, titles.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            // CSI: parameters and intermediates, then one final byte.
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: ended by BEL or ESC \.
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// A log's lines: those ended, and the one still being written.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Lines {
    done: VecDeque<String>,
    tail: String,
}

impl Lines {
    /// Adds output as it came: a line is ended by a newline, and a carriage
    /// return within one starts it again, as a progress bar redraws itself.
    pub fn push(&mut self, text: &str) {
        self.tail.push_str(text);
        while let Some(at) = self.tail.find('\n') {
            let line: String = self.tail.drain(..=at).collect();
            self.done.push_back(shown(&line[..line.len() - 1]));
        }
        while self.done.len() > MAX_LINES {
            self.done.pop_front();
        }
    }

    pub fn clear(&mut self) {
        self.done.clear();
        self.tail.clear();
    }

    /// How many lines are shown: the ended ones, and the one being
    /// written, if any of it has come.
    pub fn len(&self) -> usize {
        self.done.len() + usize::from(!self.tail.is_empty())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// A line as shown.
    pub fn line(&self, i: usize) -> String {
        match self.done.get(i) {
            Some(l) => l.clone(),
            None => shown(&self.tail),
        }
    }

    /// Every line, as shown, each ended by a newline.
    pub fn text(&self) -> String {
        let mut s = String::new();
        for i in 0..self.len() {
            s.push_str(&self.line(i));
            s.push('\n');
        }
        s
    }

    /// The longest line, by characters, which sets the list's width.
    fn widest(&self) -> Option<usize> {
        (0..self.len()).max_by_key(|&i| self.line(i).chars().count())
    }
}

/// A line without its escapes, from its last carriage return.
fn shown(line: &str) -> String {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let line = line.rsplit('\r').next().unwrap_or(line);
    strip_ansi(line)
}

const ROW: f32 = 17.;

/// The pane showing a log.
pub struct LogPane {
    id: SharedString,
    lines: Lines,
    /// Shown while there are no lines: what is awaited, or why none came.
    empty: SharedString,
    /// Whether it stays at the end as lines come.
    follow: bool,
    scroll: UniformListScrollHandle,
}

impl LogPane {
    /// A pane, its list known to tests by id.
    pub fn new(id: impl Into<SharedString>, empty: impl Into<SharedString>) -> Self {
        LogPane {
            id: id.into(),
            lines: Lines::default(),
            empty: empty.into(),
            follow: true,
            scroll: UniformListScrollHandle::new(),
        }
    }

    pub fn push(&mut self, text: &str, cx: &mut Context<Self>) {
        self.lines.push(text);
        cx.notify();
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.lines.clear();
        self.follow = true;
        cx.notify();
    }

    pub fn set_empty(&mut self, empty: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.empty = empty.into();
        cx.notify();
    }

    pub fn lines(&self) -> &Lines {
        &self.lines
    }

    pub fn following(&self) -> bool {
        self.follow
    }

    /// Keeps to the end as lines come, or stays where it is.
    pub fn set_follow(&mut self, follow: bool, cx: &mut Context<Self>) {
        self.follow = follow;
        cx.notify();
    }
}

impl Render for LogPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let body = if self.lines.is_empty() {
            div()
                .p(px(8.))
                .text_color(p.muted)
                .child(self.empty.clone())
                .into_any_element()
        } else {
            if self.follow {
                self.scroll.scroll_to_bottom();
            }
            let fg = p.term_fg;
            uniform_list(
                self.id.clone(),
                self.lines.len(),
                cx.processor(move |v, range: Range<usize>, _, _| {
                    range
                        .map(|i| {
                            div()
                                .h(px(ROW))
                                .px(px(8.))
                                .whitespace_nowrap()
                                .text_color(fg)
                                .child(v.lines.line(i))
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .with_width_from_item(self.lines.widest())
            .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
            .track_scroll(&self.scroll)
            .size_full()
            .into_any_element()
        };
        let me = cx.entity().downgrade();
        let follow = self.follow;
        div()
            .id(SharedString::from(format!("{}-pane", self.id)))
            .size_full()
            .min_h_0()
            .bg(p.term_bg)
            .font_family(crate::diff::mono())
            .text_size(px(11.5))
            // Scrolled up, it stays there; scrolled back to the end, it
            // follows again.
            .on_scroll(cx.listener(|v, e: &ScrollEvent, window, cx| {
                let up = e.delta.pixel_delta(window.line_height()).y > px(0.);
                if up {
                    v.follow = false;
                } else if v.scroll.0.borrow().base_handle.offset().y
                    <= -v.scroll.0.borrow().base_handle.max_offset().y + px(ROW)
                {
                    v.follow = true;
                }
                cx.notify();
            }))
            .context_menu(move |menu, _, cx| {
                crate::ui::modal(cx);
                let (copy, clear, end) = (me.clone(), me.clone(), me.clone());
                menu.item(PopupMenuItem::new("Copy All").on_click(move |_, _, cx| {
                    if let Some(v) = copy.upgrade() {
                        let text = v.read(cx).lines.text();
                        crate::treeops::copy(text, cx);
                    }
                }))
                .item(PopupMenuItem::new("Clear").on_click(move |_, _, cx| {
                    let _ = clear.update(cx, |v, cx| v.clear(cx));
                }))
                .separator()
                .item(
                    PopupMenuItem::new("Scroll to the End")
                        .checked(follow)
                        .on_click(move |_, _, cx| {
                            let _ = end.update(cx, |v, cx| v.set_follow(!v.follow, cx));
                        }),
                )
            })
            .child(body)
    }
}
