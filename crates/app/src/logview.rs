//! A followed log: a program's output as it arrives, split into lines with
//! the escape sequences a terminal would act on taken out, kept to the last
//! MAX_LINES, drawn in the terminal's colours and kept at its end unless
//! scrolled up. Its text is selected with the mouse as a terminal's is (a
//! word on a double click, a line on a triple) and copied, and found
//! through with ⌘F.

use std::cell::Cell;
use std::collections::VecDeque;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    Bounds, Context, Entity, FocusHandle, Focusable, HighlightStyle, IntoElement, KeyDownEvent,
    ListHorizontalSizingBehavior, MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement,
    Pixels, Point, Render, ScrollEvent, ScrollStrategy, SharedString, Styled, StyledText,
    UniformListScrollHandle, Window, canvas, div, prelude::*, px, uniform_list,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::{Sizable, h_flex};

use crate::palette;

/// A place in a log: a line, and a range of characters in it.
pub type Span = (usize, Range<usize>);

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

    /// Every place text is found, case aside, each line's in order.
    pub fn find(&self, text: &str) -> Vec<Span> {
        let lower = |s: &str| -> Vec<char> {
            s.chars()
                .map(|c| c.to_lowercase().next().unwrap_or(c))
                .collect()
        };
        let want = lower(text);
        if want.is_empty() {
            return vec![];
        }
        let mut out = vec![];
        for i in 0..self.len() {
            let line = lower(&self.line(i));
            let mut at = 0;
            while at + want.len() <= line.len() {
                if line[at..at + want.len()] == want[..] {
                    out.push((i, at..at + want.len()));
                    at += want.len();
                } else {
                    at += 1;
                }
            }
        }
        out
    }

    /// The text from one place to another, (line, character), in either
    /// order, lines ended by newlines.
    pub fn between(&self, a: (usize, usize), b: (usize, usize)) -> String {
        let (from, to) = if a <= b { (a, b) } else { (b, a) };
        let mut out = String::new();
        for i in from.0..=to.0.min(self.len().saturating_sub(1)) {
            let line: Vec<char> = self.line(i).chars().collect();
            let start = if i == from.0 {
                from.1.min(line.len())
            } else {
                0
            };
            let end = if i == to.0 {
                to.1.min(line.len())
            } else {
                line.len()
            };
            out.extend(&line[start..end.max(start)]);
            if i != to.0 {
                out.push('\n');
            }
        }
        out
    }

    /// The longest line, by characters, which sets the list's width.
    fn widest(&self) -> Option<usize> {
        (0..self.len()).max_by_key(|&i| self.line(i).chars().count())
    }
}

/// The word a character is in, as characters: a run of letters, digits
/// and underscores, else the character alone.
pub fn word_at(line: &str, col: usize) -> Range<usize> {
    let chars: Vec<char> = line.chars().collect();
    if col >= chars.len() {
        return chars.len()..chars.len();
    }
    let word = |c: &char| c.is_alphanumeric() || *c == '_';
    if !word(&chars[col]) {
        return col..col + 1;
    }
    let start = (0..col)
        .rev()
        .take_while(|i| word(&chars[*i]))
        .last()
        .unwrap_or(col);
    let end = (col..chars.len())
        .take_while(|i| word(&chars[*i]))
        .last()
        .unwrap_or(col)
        + 1;
    start..end
}

/// Character ranges in a line as byte ranges, for highlighting.
fn bytes(line: &str, chars: &Range<usize>) -> Range<usize> {
    let at = |n: usize| line.char_indices().nth(n).map_or(line.len(), |(b, _)| b);
    at(chars.start)..at(chars.end)
}

/// A line without its escapes, from its last carriage return.
fn shown(line: &str) -> String {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let line = line.rsplit('\r').next().unwrap_or(line);
    strip_ansi(line)
}

const ROW: f32 = 17.;
const PAD: f32 = 8.;
const FONT: f32 = 11.5;

/// The pane showing a log.
pub struct LogPane {
    id: SharedString,
    lines: Lines,
    /// Shown while there are no lines: what is awaited, or why none came.
    empty: SharedString,
    /// Whether it stays at the end as lines come.
    follow: bool,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    /// The selection: where it was started and where it reaches, as (line,
    /// character); and whether the mouse is still drawing it.
    anchor: Option<(usize, usize)>,
    head: (usize, usize),
    selecting: bool,
    /// Where the lines are drawn and how wide a character is, for placing
    /// the mouse.
    origin: Rc<Cell<Point<Pixels>>>,
    cell: Option<Pixels>,
    /// Find: its field, what it matches and which match is shown.
    find: Option<(Entity<InputState>, gpui::Subscription)>,
    found: Vec<Span>,
    current: usize,
}

impl LogPane {
    /// A pane, its list known to tests by id.
    pub fn new(
        id: impl Into<SharedString>,
        empty: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> Self {
        LogPane {
            id: id.into(),
            lines: Lines::default(),
            empty: empty.into(),
            follow: true,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            anchor: None,
            head: (0, 0),
            selecting: false,
            origin: Rc::new(Cell::new(Point::default())),
            cell: None,
            find: None,
            found: vec![],
            current: 0,
        }
    }

    pub fn push(&mut self, text: &str, cx: &mut Context<Self>) {
        let before = self.lines.len();
        self.lines.push(text);
        // The oldest lines dropped move every place up.
        let dropped = (before + text.matches('\n').count()).saturating_sub(self.lines.len());
        if dropped > 0 {
            self.anchor = None;
        }
        if self.find.is_some() {
            self.refind(cx);
        }
        cx.notify();
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.lines.clear();
        self.follow = true;
        self.anchor = None;
        self.found.clear();
        cx.notify();
    }

    /// The text selected, if any.
    pub fn selected(&self) -> Option<String> {
        let a = self.anchor?;
        (a != self.head).then(|| self.lines.between(a, self.head))
    }

    /// Selects every line.
    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        if self.lines.is_empty() {
            return;
        }
        let last = self.lines.len() - 1;
        self.anchor = Some((0, 0));
        self.head = (last, self.lines.line(last).chars().count());
        cx.notify();
    }

    /// Where the mouse is among the lines: (line, character).
    fn place(&self, at: Point<Pixels>, window: &mut Window) -> (usize, usize) {
        let cell = self.cell.unwrap_or_else(|| {
            let font = gpui::font(crate::diff::mono());
            let id = window.text_system().resolve_font(&font);
            window
                .text_system()
                .advance(id, px(FONT), 'm')
                .map(|s| s.width)
                .unwrap_or(px(7.))
        });
        let offset = self.scroll.0.borrow().base_handle.offset();
        let origin = self.origin.get();
        let y = f32::from(at.y - origin.y - offset.y).max(0.);
        let x = f32::from(at.x - origin.x - offset.x - px(PAD)).max(0.);
        let line = ((y / ROW) as usize).min(self.lines.len().saturating_sub(1));
        let width = self.lines.line(line).chars().count();
        let col = ((x / f32::from(cell)).round() as usize).min(width);
        (line, col)
    }

    fn mouse_down(&mut self, e: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        if self.lines.is_empty() {
            return;
        }
        if self.cell.is_none() {
            let font = gpui::font(crate::diff::mono());
            let id = window.text_system().resolve_font(&font);
            self.cell = window
                .text_system()
                .advance(id, px(FONT), 'm')
                .map(|s| s.width)
                .ok();
        }
        let (line, col) = self.place(e.position, window);
        match e.click_count {
            2 => {
                let w = word_at(&self.lines.line(line), col);
                self.anchor = Some((line, w.start));
                self.head = (line, w.end);
            }
            n if n >= 3 => {
                self.anchor = Some((line, 0));
                self.head = (line, self.lines.line(line).chars().count());
            }
            _ if e.modifiers.shift && self.anchor.is_some() => self.head = (line, col),
            _ => {
                self.anchor = Some((line, col));
                self.head = (line, col);
            }
        }
        self.selecting = e.click_count < 2;
        // Lines arriving would carry what is being read away.
        self.follow = false;
        cx.notify();
    }

    fn mouse_move(&mut self, e: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting || e.pressed_button != Some(MouseButton::Left) {
            self.selecting = false;
            return;
        }
        self.head = self.place(e.position, window);
        cx.notify();
    }

    fn copy(&self, cx: &mut Context<Self>) {
        if let Some(text) = self.selected() {
            crate::treeops::copy(text, cx);
        }
    }

    fn key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &e.keystroke;
        let cmd = if cfg!(target_os = "macos") {
            k.modifiers.platform
        } else {
            k.modifiers.control
        };
        match k.key.as_str() {
            "c" if cmd => self.copy(cx),
            "a" if cmd => self.select_all(cx),
            "f" if cmd => self.open_find(window, cx),
            "escape" if self.anchor.is_some() => {
                self.anchor = None;
                cx.notify();
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    /// Opens Find over the log, with the selection in it if there is one.
    pub fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let seed = self.selected().filter(|s| !s.contains('\n'));
        if let Some((input, _)) = &self.find {
            let input = input.clone();
            if let Some(seed) = seed {
                input.update(cx, |i, cx| i.set_value(seed, window, cx));
                self.refind(cx);
            }
            window.focus(&input.focus_handle(cx), cx);
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find in the log"));
        if let Some(seed) = seed {
            input.update(cx, |i, cx| i.set_value(seed, window, cx));
        }
        let sub = cx.subscribe_in(&input, window, |v, _, e: &InputEvent, _, cx| match e {
            InputEvent::Change => {
                v.current = 0;
                v.refind(cx);
                v.show_current(cx);
            }
            InputEvent::PressEnter { shift, .. } => v.step(!*shift, cx),
            _ => {}
        });
        window.focus(&input.focus_handle(cx), cx);
        self.find = Some((input, sub));
        self.refind(cx);
        self.show_current(cx);
        cx.notify();
    }

    fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find = None;
        self.found.clear();
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn refind(&mut self, cx: &mut Context<Self>) {
        let Some((input, _)) = &self.find else {
            return;
        };
        let text = input.read(cx).value().to_string();
        self.found = self.lines.find(&text);
        self.current = self.current.min(self.found.len().saturating_sub(1));
    }

    /// Shows the next match, or the one before, round from the ends.
    fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        if self.found.is_empty() {
            return;
        }
        let n = self.found.len();
        self.current = if forward {
            (self.current + 1) % n
        } else {
            (self.current + n - 1) % n
        };
        self.show_current(cx);
    }

    fn show_current(&mut self, cx: &mut Context<Self>) {
        if let Some((line, _)) = self.found.get(self.current) {
            self.follow = false;
            self.scroll.scroll_to_item(*line, ScrollStrategy::Center);
        }
        cx.notify();
    }

    /// A line as drawn: the selection and the matches on it marked.
    fn draw(&self, i: usize, p: &palette::Palette) -> gpui::AnyElement {
        let text = self.lines.line(i);
        let mut marks: Vec<(Range<usize>, HighlightStyle)> = vec![];
        let first = self.found.partition_point(|(l, _)| *l < i);
        for (n, (_, r)) in self.found[first..]
            .iter()
            .take_while(|(l, _)| *l == i)
            .enumerate()
        {
            let current = first + n == self.current;
            marks.push((
                bytes(&text, r),
                HighlightStyle {
                    background_color: Some(if current {
                        p.wait.opacity(0.75)
                    } else {
                        p.wait.opacity(0.3)
                    }),
                    ..Default::default()
                },
            ));
        }
        if let Some(a) = self.anchor
            && a != self.head
        {
            let (from, to) = if a <= self.head {
                (a, self.head)
            } else {
                (self.head, a)
            };
            if (from.0..=to.0).contains(&i) {
                let width = text.chars().count();
                let start = if i == from.0 { from.1 } else { 0 };
                let end = if i == to.0 { to.1 } else { width };
                if start < end {
                    // The selection is drawn under the matches.
                    marks.insert(
                        0,
                        (
                            bytes(&text, &(start..end.min(width))),
                            HighlightStyle {
                                background_color: Some(p.accent.opacity(0.35)),
                                ..Default::default()
                            },
                        ),
                    );
                }
            }
        }
        // Highlights must not overlap: the later of two that do is cut.
        marks.sort_by_key(|(r, _)| r.start);
        let mut end = 0;
        let marks: Vec<_> = marks
            .into_iter()
            .filter_map(|(r, h)| {
                let r = r.start.max(end)..r.end;
                (r.start < r.end).then(|| {
                    end = r.end;
                    (r, h)
                })
            })
            .collect();
        div()
            .h(px(ROW))
            .px(px(PAD))
            .whitespace_nowrap()
            .text_color(p.term_fg)
            .child(StyledText::new(SharedString::from(text)).with_highlights(marks))
            .into_any_element()
    }

    pub fn set_empty(&mut self, empty: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.empty = empty.into();
        cx.notify();
    }

    /// What is shown while there are no lines.
    pub fn empty(&self) -> &str {
        &self.empty
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
            uniform_list(
                self.id.clone(),
                self.lines.len(),
                cx.processor(move |v, range: Range<usize>, _, cx| {
                    let p = palette::of(cx);
                    range.map(|i| v.draw(i, &p)).collect::<Vec<_>>()
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
        let origin = self.origin.clone();
        let has_selection = self.selected().is_some();
        let found = if self.found.is_empty() {
            "No matches".to_string()
        } else {
            format!("{} of {}", self.current + 1, self.found.len())
        };
        div()
            .id(SharedString::from(format!("{}-pane", self.id)))
            .relative()
            .size_full()
            .min_h_0()
            .bg(p.term_bg)
            .font_family(crate::diff::mono())
            .text_size(px(FONT))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|v, e: &KeyDownEvent, window, cx| v.key(e, window, cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|v, e: &MouseDownEvent, window, cx| v.mouse_down(e, window, cx)),
            )
            .on_mouse_move(
                cx.listener(|v, e: &MouseMoveEvent, window, cx| v.mouse_move(e, window, cx)),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|v, _, _, _| v.selecting = false),
            )
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
                let (selected, find) = (me.clone(), me.clone());
                menu.item(
                    PopupMenuItem::new("Copy")
                        .disabled(!has_selection)
                        .on_click(move |_, _, cx| {
                            let _ = selected.update(cx, |v, cx| v.copy(cx));
                        }),
                )
                .item(PopupMenuItem::new("Find…").on_click(move |_, window, cx| {
                    let _ = find.update(cx, |v, cx| v.open_find(window, cx));
                }))
                .separator()
                .item(PopupMenuItem::new("Copy All").on_click(move |_, _, cx| {
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
            .child(
                canvas(
                    move |bounds: Bounds<Pixels>, _, _| origin.set(bounds.origin),
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .when_some(self.find.as_ref().map(|(i, _)| i.clone()), |d, input| {
                let missing = self.found.is_empty() && !input.read(cx).value().is_empty();
                d.child(
                    h_flex()
                        .absolute()
                        .top(px(4.))
                        .right(px(12.))
                        .w(px(300.))
                        .gap(px(6.))
                        // Escape closes Find, taken before the field, which
                        // would keep it.
                        .capture_action(cx.listener(
                            |v, _: &gpui_component::input::Escape, window, cx| {
                                cx.stop_propagation();
                                v.close_find(window, cx)
                            },
                        ))
                        .p(px(4.))
                        .rounded(px(6.))
                        .bg(p.surface)
                        .border_1()
                        .border_color(if missing { p.bad } else { p.border })
                        .shadow_md()
                        .child(div().flex_1().child(Input::new(&input).small()))
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(11.))
                                .text_color(p.muted)
                                .child(found),
                        ),
                )
            })
    }
}
