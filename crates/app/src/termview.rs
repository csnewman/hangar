//! A terminal session in the window: its screen drawn as styled text in
//! the design's terminal colours, sized to fit where it is shown, with what
//! is typed and pasted sent to the session.

use std::time::{Duration, Instant};

use gpui::{
    App, Bounds, ClipboardItem, Context, FocusHandle, Focusable, Font, Hsla, IntoElement,
    KeyBinding, KeyDownEvent, ParentElement, Pixels, Render, Rgba, ScrollEvent, Styled, StyledText,
    TextRun, UnderlineStyle, WeakEntity, Window, actions, canvas, div, font, prelude::*, px,
};

use crate::api::Client;
use crate::palette;
use crate::termconn::{self, Command, Connection, Event};
use crate::terminal::{Replies, Screen};

/// The text's size, and the height of a line.
const FONT_SIZE: Pixels = px(12.5);
const LINE_HEIGHT: Pixels = px(17.);

/// How long a burst of output is taken in before a frame is drawn.
const FEED_BUDGET: Duration = Duration::from_millis(4);

actions!(terminal, [SendTab, SendBackTab]);

/// Binds Tab and Shift-Tab to the shell, ahead of gpui-component's binding
/// of them to moving focus, which takes them before a key handler sees them.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("tab", SendTab, Some("Terminal")),
        KeyBinding::new("shift-tab", SendBackTab, Some("Terminal")),
    ]);
}

/// Where a terminal's connection is.
#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Connecting,
    Live,
    /// Lost, and being dialled again: why.
    Reconnecting(String),
    Exited(i32),
    Failed(String),
}

pub struct TerminalView {
    screen: Screen<Replies>,
    replies: Replies,
    /// The default foreground and background, for a screen made afresh.
    colours: (
        alacritty_terminal::vte::ansi::Rgb,
        alacritty_terminal::vte::ansi::Rgb,
    ),
    conn: Option<Connection>,
    focus: FocusHandle,
    state: State,
    /// The session's ID once attached, and its title.
    session: Option<String>,
    title: String,
    /// How many more bytes of output replay the session's past.
    history: usize,
    cols: u16,
    rows: u16,
    font: Font,
    /// A cell's width, once measured.
    cell: Option<Pixels>,
    /// Scrolling, in pixels, not yet a whole line.
    scroll_left: f32,
}

fn mono() -> Font {
    font(if cfg!(target_os = "macos") {
        "Menlo"
    } else if cfg!(target_os = "windows") {
        "Consolas"
    } else {
        "DejaVu Sans Mono"
    })
}

fn rgb(c: Hsla) -> alacritty_terminal::vte::ansi::Rgb {
    let c = Rgba::from(c);
    alacritty_terminal::vte::ansi::Rgb {
        r: (c.r * 255.) as u8,
        g: (c.g * 255.) as u8,
        b: (c.b * 255.) as u8,
    }
}

fn hsla(c: alacritty_terminal::vte::ansi::Rgb) -> Hsla {
    Rgba {
        r: c.r as f32 / 255.,
        g: c.g as f32 / 255.,
        b: c.b as f32 / 255.,
        a: 1.,
    }
    .into()
}

impl TerminalView {
    /// Attaches to an environment's session, or a new one when session is
    /// None.
    pub fn new(client: &Client, env: &str, session: Option<&str>, cx: &mut Context<Self>) -> Self {
        let (cols, rows) = (80, 24);
        let replies = Replies::default();
        let p = palette::of(cx);
        let colours = (rgb(p.term_fg), rgb(p.term_tab));
        let mut screen = Screen::with_listener(cols as usize, rows as usize, replies.clone());
        screen.set_default_colours(colours.0, colours.1);
        let (tx, rx) = async_channel::unbounded();
        let conn = termconn::attach(client, env, session, cols, rows, move |e| {
            tx.send_blocking(e).is_ok()
        });
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            while let Ok(first) = rx.recv().await {
                let alive = this.update(cx, |t, cx| {
                    let start = Instant::now();
                    t.take(first);
                    while start.elapsed() < FEED_BUDGET {
                        let Ok(more) = rx.try_recv() else { break };
                        t.take(more);
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
            }
        })
        .detach();
        TerminalView {
            screen,
            replies,
            colours,
            conn: Some(conn),
            focus: cx.focus_handle(),
            state: State::Connecting,
            session: session.map(str::to_string),
            title: String::new(),
            history: 0,
            cols,
            rows,
            font: mono(),
            cell: None,
            scroll_left: 0.,
        }
    }

    pub fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    /// Types text into the session, as if it had been typed.
    pub fn type_text(&mut self, text: &str) {
        self.send(Command::Input(text.as_bytes().to_vec()));
    }

    fn send(&self, c: Command) {
        if let Some(conn) = &self.conn {
            conn.send(c);
        }
    }

    fn take(&mut self, e: Event) {
        match e {
            Event::Attached {
                session,
                title,
                history,
            } => {
                // Attached again, the session's past is replayed: drawn on
                // a clean screen, not over what was shown.
                if self.session.is_some() && self.state != State::Connecting {
                    let mut screen = Screen::with_listener(
                        self.cols as usize,
                        self.rows as usize,
                        self.replies.clone(),
                    );
                    screen.set_default_colours(self.colours.0, self.colours.1);
                    self.screen = screen;
                }
                self.session = Some(session);
                self.title = title;
                self.history = history;
                self.state = State::Live;
                // The size this window has, which the session's may not be.
                self.send(Command::Resize(self.cols, self.rows));
            }
            Event::Output(b) => {
                self.screen.feed(&b);
                let replayed = self.history.min(b.len());
                self.history -= replayed;
                let (replies, title) = self.replies.take();
                // What the past asked was answered then.
                if replayed == 0 && !replies.is_empty() {
                    self.send(Command::Input(replies));
                }
                if let Some(t) = title {
                    self.title = t;
                }
            }
            Event::Size { cols, rows, owner } => {
                if !owner && (cols, rows) != (self.cols, self.rows) {
                    self.screen.resize(cols as usize, rows as usize);
                }
            }
            Event::Reconnecting { why, retry_in } => {
                self.state =
                    State::Reconnecting(format!("{why}; trying again in {}", seconds(retry_in)));
            }
            Event::Exited(code) => {
                self.state = State::Exited(code);
                self.conn = None;
            }
            Event::Failed(e) => {
                self.state = State::Failed(e);
                self.conn = None;
            }
        }
    }

    /// Fits the screen to bounds, telling the session its new size.
    fn fit(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let cell = *self.cell.get_or_insert_with(|| {
            let id = window.text_system().resolve_font(&self.font);
            window
                .text_system()
                .advance(id, FONT_SIZE, 'm')
                .map(|s| s.width)
                .unwrap_or(px(7.5))
        });
        let cols = (f32::from(bounds.size.width) / f32::from(cell))
            .floor()
            .max(2.) as u16;
        let rows = (f32::from(bounds.size.height) / f32::from(LINE_HEIGHT))
            .floor()
            .max(1.) as u16;
        if (cols, rows) != (self.cols, self.rows) {
            (self.cols, self.rows) = (cols, rows);
            self.screen.resize(cols as usize, rows as usize);
            self.send(Command::Resize(cols, rows));
            cx.notify();
        }
    }

    fn key(&mut self, e: &KeyDownEvent, cx: &mut Context<Self>) {
        let k = &e.keystroke;
        let paste = if cfg!(target_os = "macos") {
            k.modifiers.platform && k.key == "v"
        } else {
            k.modifiers.control && k.modifiers.shift && k.key == "v"
        };
        if paste {
            if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                let text = text.replace("\r\n", "\r").replace('\n', "\r");
                let b = if self.screen.bracketed_paste() {
                    format!("\x1b[200~{text}\x1b[201~")
                } else {
                    text
                };
                self.send(Command::Input(b.into_bytes()));
            }
            return;
        }
        if let Some(b) = bytes(e, self.screen.app_cursor()) {
            self.input(b);
            cx.stop_propagation();
        }
    }

    /// Sends what a key types, back at the bottom of the screen.
    fn input(&mut self, b: Vec<u8>) {
        if self.screen.scrolled_back() > 0 {
            self.screen.scroll(-(self.screen.scrolled_back() as i32));
        }
        self.send(Command::Input(b));
    }

    fn scroll(&mut self, e: &ScrollEvent, cx: &mut Context<Self>) {
        self.scroll_left += f32::from(e.delta.pixel_delta(LINE_HEIGHT).y);
        let lines = (self.scroll_left / f32::from(LINE_HEIGHT)).trunc();
        if lines != 0. {
            self.scroll_left -= lines * f32::from(LINE_HEIGHT);
            self.screen.scroll(lines as i32);
            cx.notify();
        }
    }

    /// Copies the visible screen's text, which there is no selection to
    /// narrow yet.
    pub fn copy_screen(&self, cx: &mut App) {
        let text: Vec<String> = self
            .screen
            .lines()
            .into_iter()
            .map(|l| l.text.trim_end().to_string())
            .collect();
        cx.write_to_clipboard(ClipboardItem::new_string(
            text.join("\n").trim_end().to_string(),
        ));
    }
}

/// What a key sends to the shell; app_cursor is the program's asking for
/// the cursor keys' application sequences.
fn bytes(e: &KeyDownEvent, app_cursor: bool) -> Option<Vec<u8>> {
    let k = &e.keystroke;
    if k.modifiers.platform {
        return None;
    }
    let arrow = |c: u8| {
        if app_cursor {
            vec![0x1b, b'O', c]
        } else {
            vec![0x1b, b'[', c]
        }
    };
    let named: Option<Vec<u8>> = match k.key.as_str() {
        "enter" => Some(b"\r".to_vec()),
        "backspace" if k.modifiers.alt => Some(b"\x1b\x7f".to_vec()),
        "backspace" => Some(b"\x7f".to_vec()),
        "tab" if k.modifiers.shift => Some(b"\x1b[Z".to_vec()),
        "tab" => Some(b"\t".to_vec()),
        "escape" => Some(b"\x1b".to_vec()),
        "up" => Some(arrow(b'A')),
        "down" => Some(arrow(b'B')),
        "right" if k.modifiers.alt => Some(b"\x1bf".to_vec()),
        "left" if k.modifiers.alt => Some(b"\x1bb".to_vec()),
        "right" => Some(arrow(b'C')),
        "left" => Some(arrow(b'D')),
        "home" => Some(b"\x1b[H".to_vec()),
        "end" => Some(b"\x1b[F".to_vec()),
        "pageup" => Some(b"\x1b[5~".to_vec()),
        "pagedown" => Some(b"\x1b[6~".to_vec()),
        "delete" => Some(b"\x1b[3~".to_vec()),
        "space" if k.modifiers.control => Some(vec![0]),
        _ => None,
    };
    if named.is_some() {
        return named;
    }
    if k.modifiers.control && k.key.len() == 1 {
        let c = k.key.as_bytes()[0].to_ascii_lowercase();
        if c.is_ascii_lowercase() {
            return Some(vec![c & 0x1f]);
        }
    }
    let text = k.key_char.as_ref()?;
    if k.modifiers.alt && !cfg!(target_os = "macos") {
        let mut b = vec![0x1b];
        b.extend_from_slice(text.as_bytes());
        return Some(b);
    }
    Some(text.as_bytes().to_vec())
}

/// Splits the runs at the cursor's cell and swaps that cell's colours.
fn cursor_runs(runs: Vec<TextRun>, at: usize, len: usize) -> Vec<TextRun> {
    let mut out = Vec::new();
    let mut pos = 0;
    for r in runs {
        let (start, end) = (pos, pos + r.len);
        pos = end;
        if end <= at || start >= at + len {
            out.push(r);
            continue;
        }
        if at > start {
            out.push(TextRun {
                len: at - start,
                ..r.clone()
            });
        }
        out.push(TextRun {
            len,
            color: r.background_color.unwrap_or(gpui::black()),
            background_color: Some(r.color),
            ..r.clone()
        });
        if end > at + len {
            out.push(TextRun {
                len: end - at - len,
                ..r
            });
        }
    }
    out
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        self.screen
            .set_default_colours(rgb(p.term_fg), rgb(p.term_tab));
        let focused = self.focus.is_focused(window);
        let show_cursor = focused && self.screen.scrolled_back() == 0;
        let (cursor_row, cursor_col) = self.screen.cursor();
        let rows: Vec<_> = self
            .screen
            .lines()
            .into_iter()
            .enumerate()
            .map(|(row, line)| {
                let mut runs: Vec<TextRun> = line
                    .spans
                    .iter()
                    .map(|s| TextRun {
                        len: s.len,
                        font: Font {
                            weight: if s.bold {
                                gpui::FontWeight::BOLD
                            } else {
                                Default::default()
                            },
                            style: if s.italic {
                                gpui::FontStyle::Italic
                            } else {
                                Default::default()
                            },
                            ..self.font.clone()
                        },
                        color: hsla(s.fg),
                        background_color: Some(hsla(s.bg)),
                        underline: s.underline.then(|| UnderlineStyle {
                            thickness: px(1.),
                            ..Default::default()
                        }),
                        strikethrough: None,
                    })
                    .collect();
                if show_cursor && row == cursor_row {
                    let at = line.text.char_indices().nth(cursor_col).map(|(i, _)| i);
                    if let Some(at) = at {
                        let len = line.text[at..].chars().next().map_or(1, char::len_utf8);
                        runs = cursor_runs(runs, at, len);
                    }
                }
                StyledText::new(line.text).with_runs(runs)
            })
            .collect();
        let me = cx.entity().downgrade();
        let notice = match &self.state {
            State::Connecting => Some("Connecting…".to_string()),
            State::Exited(code) => Some(format!("The shell exited ({code}).")),
            State::Failed(e) => Some(format!("Could not connect: {e}")),
            State::Reconnecting(why) => Some(format!("Reconnecting: {why}")),
            State::Live => None,
        };
        let reconnecting = matches!(self.state, State::Reconnecting(_));
        div()
            .id("terminal")
            .track_focus(&self.focus)
            .key_context("Terminal")
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| this.key(e, cx)))
            .on_action(cx.listener(|this, _: &SendTab, _, _| this.input(b"\t".to_vec())))
            .on_action(cx.listener(|this, _: &SendBackTab, _, _| this.input(b"\x1b[Z".to_vec())))
            .on_scroll(cx.listener(|this, e: &ScrollEvent, _, cx| this.scroll(e, cx)))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.focus, cx);
                }),
            )
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(p.term_tab)
            .px(px(8.))
            .py(px(4.))
            .child(
                div()
                    .size_full()
                    .relative()
                    .font(self.font.clone())
                    .text_size(FONT_SIZE)
                    .line_height(LINE_HEIGHT)
                    .children(rows)
                    .child(
                        canvas(
                            move |bounds, window, cx| {
                                if let Some(me) = me.upgrade() {
                                    me.update(cx, |t, cx| t.fit(bounds, window, cx));
                                }
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    ),
            )
            .when_some(notice, |d, n| {
                d.child(
                    div()
                        .absolute()
                        .bottom(px(6.))
                        .right(px(10.))
                        .text_size(px(11.))
                        .text_color(if reconnecting { p.busy } else { p.idle })
                        .child(n),
                )
            })
    }
}

/// A wait as said: "0.5s", "4s".
fn seconds(d: Duration) -> String {
    if d < Duration::from_secs(1) {
        format!("{:.1}s", d.as_secs_f32())
    } else {
        format!("{}s", d.as_secs())
    }
}
