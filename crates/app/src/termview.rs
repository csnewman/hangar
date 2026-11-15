//! A terminal session in the window: its screen drawn as styled text in
//! the design's terminal colours, sized to fit where it is shown, with what
//! is typed and pasted sent to the session. The mouse selects, as in any
//! terminal, and Find looks through the output. Its menu is IntelliJ's
//! terminal's: copy, paste, select all, find, clear the buffer, and the
//! session's own commands.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    App, Bounds, ClipboardItem, Context, Entity, FocusHandle, Focusable, Font, Hsla, IntoElement,
    KeyBinding, KeyDownEvent, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, Pixels, Point, Render, Rgba, ScrollEvent, Styled, Subscription, Task,
    WeakEntity, Window, actions, canvas, div, font, point, prelude::*, px, size,
};
use gpui_component::Sizable;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use hangar_ghostty::Placement;

use crate::api::Client;
use crate::palette;
use crate::settings::TerminalEmulator;
use crate::termconn::{self, Command, Connection, Event};
use crate::terminal::{Alacritty, CursorStyle, Emulator, Link, LinkKind, Select};
use crate::termpaint::{self, hsla};

/// A line's height for each point of the text's size.
const LINE_SPACING: f32 = 1.36;

/// The text's size the settings choose, and the height of a line.
fn metrics(cx: &App) -> (Pixels, Pixels) {
    let size = crate::settings::Settings::get(cx).terminal_font();
    (px(size), px((size * LINE_SPACING).round()))
}

/// How long a burst of output is taken in before a frame is drawn.
const FEED_BUDGET: Duration = Duration::from_millis(4);

actions!(
    terminal,
    [
        SendTab,
        SendBackTab,
        Find,
        CopySelection,
        Paste,
        SelectAll,
        ClearBuffer,
        /// A new session in a tab of its own, for the window holding the
        /// terminal to start.
        NewSession,
        /// The terminal's tab closed, its session left running.
        CloseSession,
        /// The terminal's session ended, and what runs in it, once asked.
        EndSession,
        /// The view moved to the shell's prompt before the top one shown,
        /// or after it (OSC 133's marks).
        PreviousPrompt,
        NextPrompt,
    ]
);

/// Binds Tab and Shift-Tab to the shell, ahead of gpui-component's binding
/// of them to moving focus, which takes them before a key handler sees them;
/// and the menu's commands, with Cmd on macOS and Ctrl-Shift elsewhere,
/// where Ctrl and a letter are the shell's.
pub fn bind_keys(cx: &mut App) {
    // A prompt before or after: Terminal.app's keys on macOS, as Ghostty
    // has them, and Ghostty's elsewhere.
    let keys = if cfg!(target_os = "macos") {
        [
            "cmd-f", "cmd-c", "cmd-v", "cmd-a", "cmd-t", "cmd-w", "cmd-up", "cmd-down",
        ]
    } else {
        [
            "ctrl-shift-f",
            "ctrl-shift-c",
            "ctrl-shift-v",
            "ctrl-shift-a",
            "ctrl-shift-t",
            "ctrl-shift-w",
            "ctrl-shift-up",
            "ctrl-shift-down",
        ]
    };
    let [find, copy, paste, all, new, close, previous, next] = keys;
    let ctx = Some("Terminal");
    cx.bind_keys([
        KeyBinding::new("tab", SendTab, ctx),
        KeyBinding::new("shift-tab", SendBackTab, ctx),
        KeyBinding::new(find, Find, ctx),
        KeyBinding::new(copy, CopySelection, ctx),
        KeyBinding::new(paste, Paste, ctx),
        KeyBinding::new(all, SelectAll, ctx),
        KeyBinding::new(new, NewSession, ctx),
        KeyBinding::new(close, CloseSession, ctx),
        KeyBinding::new(previous, PreviousPrompt, ctx),
        KeyBinding::new(next, NextPrompt, ctx),
    ]);
}

/// The terminal's size as the session has it, how many windows have the
/// session open, and whether this one holds the size. One window holds it,
/// as tmux's latest: the last to type or click there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sizing {
    pub cols: u16,
    pub rows: u16,
    pub viewers: u16,
    pub owner: bool,
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
    screen: Box<dyn Emulator>,
    /// Which emulator keeps the screen, as the settings chose when the
    /// terminal opened.
    emulator: TerminalEmulator,
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
    /// The text the program last copied to the clipboard (OSC 52), to be
    /// written to it: never one replayed, which was copied long ago.
    copied: Option<String>,
    cols: u16,
    rows: u16,
    font: Font,
    font_size: Pixels,
    line_height: Pixels,
    /// A cell's width, once measured.
    cell: Option<Pixels>,
    /// Scrolling, in pixels, not yet a whole line.
    scroll_left: f32,
    /// Where the text starts in the window, for the mouse.
    origin: Point<Pixels>,
    /// Whether a drag is selecting.
    selecting: bool,
    /// The button held, while the program is told of the mouse.
    reporting: Option<crate::termkeys::Button>,
    /// The link under the last right-click, for the menu to open or copy.
    menu_link: Option<LinkKind>,
    /// The link the pointer is on, and where the pointer is from the
    /// text's top left, for the hint beside it.
    hover: Option<(Link, Point<Pixels>)>,
    /// Whether the key that makes a click open a link is held: ⌘, else
    /// Ctrl.
    opens: bool,
    /// The directory the shell last said it is in, which a path in the
    /// output is from.
    cwd: Option<String>,
    /// The size as the session has it, once it says.
    sizing: Option<Sizing>,
    /// Find's query, while it is open, and whether its text is nowhere.
    find: Option<Entity<InputState>>,
    missing: bool,
    /// A command typed into a new session once it starts.
    run: Option<String>,
    /// Where sessions come from, and the folder a new one starts in, for
    /// starting another once the shell exits.
    client: Client,
    env: String,
    dir: Option<String>,
    _find: Option<Subscription>,
    /// What the program writes besides text: the pictures it draws, by the
    /// number their anchor in the grid names, and the progress it reports.
    extract: crate::termimage::Extract,
    pictures: HashMap<u64, Shown>,
    next_picture: u64,
    /// The progress the program last reported, and when.
    progress: Option<(crate::termimage::Progress, Instant)>,
    /// The pictures the emulator keeps, as drawn: by the screen they are
    /// on (a count of the screens made), image, generation of its pixels
    /// and the part shown.
    kitty: HashMap<KittyKey, Arc<gpui::RenderImage>>,
    screens: u64,
    /// A redraw due later, and when: an animation's next frame, a
    /// synchronized update the program has not yet ended, or a blinking
    /// cursor's next turn.
    redraw: Option<(Instant, Task<()>)>,
    /// When a blinking cursor last came on: at the last key or output.
    blink: Instant,
    /// What the programs say they are doing (OSC 7501).
    statuses: crate::termstatus::Records,
    /// Signals for the window around, as they come.
    signals: Vec<Signal>,
    /// Whether the terminal had the keys when last drawn.
    had_keys: bool,
}

type KittyKey = (u64, u32, u64, (u32, u32, u32, u32));

/// A picture drawn in the terminal, and its size where it is drawn.
struct Shown {
    image: std::sync::Arc<gpui::Image>,
    width: Pixels,
    height: Pixels,
}

/// What names a picture's anchor: an OSC 8 link on the cell it starts at,
/// so it scrolls and clears with the text around it.
const PICTURE: &str = "hangar-picture:";

/// The pixels to a point that a program's pictures and its cell size are
/// counted in: a Retina screen's, on every screen, so a picture keeps its
/// size against the text whichever screen the window is on.
const PIXELS_PER_POINT: f32 = 2.;

/// A screen kept by an emulator, of a size, in the default colours.
fn screen(
    emulator: TerminalEmulator,
    cols: u16,
    rows: u16,
    colours: (
        alacritty_terminal::vte::ansi::Rgb,
        alacritty_terminal::vte::ansi::Rgb,
    ),
) -> Box<dyn Emulator> {
    let mut s: Box<dyn Emulator> = match emulator {
        TerminalEmulator::Ghostty => Box::new(crate::termghostty::Ghostty::new(
            cols as usize,
            rows as usize,
        )),
        TerminalEmulator::Alacritty => Box::new(Alacritty::new(cols as usize, rows as usize)),
    };
    s.set_default_colours(colours.0, colours.1);
    s
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

impl TerminalView {
    /// Attaches to an environment's session, or a new one when session is
    /// None.
    pub fn new(client: &Client, env: &str, session: Option<&str>, cx: &mut Context<Self>) -> Self {
        Self::attached(client, env, session, None, cx)
    }

    /// A new session, starting in a folder.
    pub fn starting_in(client: &Client, env: &str, dir: &str, cx: &mut Context<Self>) -> Self {
        Self::attached(client, env, None, Some(dir), cx)
    }

    fn attached(
        client: &Client,
        env: &str,
        session: Option<&str>,
        dir: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Self {
        let (cols, rows) = (80, 24);
        let p = palette::of(cx);
        let colours = (rgb(p.term_fg), rgb(p.term_tab));
        let emulator = crate::settings::Settings::get(cx).terminal_emulator;
        let screen = screen(emulator, cols, rows, colours);
        let mut extract = crate::termimage::Extract::default();
        extract.kitty_passes = screen.takes_kitty_graphics();
        let mut view = TerminalView {
            screen,
            emulator,
            colours,
            conn: None,
            focus: cx.focus_handle(),
            state: State::Connecting,
            session: session.map(str::to_string),
            title: String::new(),
            history: 0,
            copied: None,
            cols,
            rows,
            font: mono(),
            font_size: metrics(cx).0,
            line_height: metrics(cx).1,
            cell: None,
            scroll_left: 0.,
            origin: Point::default(),
            selecting: false,
            reporting: None,
            menu_link: None,
            hover: None,
            opens: false,
            cwd: None,
            sizing: None,
            find: None,
            missing: false,
            run: None,
            client: client.clone(),
            env: env.to_string(),
            dir: dir.map(str::to_string),
            _find: None,
            extract,
            pictures: HashMap::new(),
            next_picture: 0,
            progress: None,
            kitty: HashMap::new(),
            screens: 0,
            redraw: None,
            blink: Instant::now(),
            statuses: Default::default(),
            signals: vec![],
            had_keys: false,
        };
        view.connect(session, cx);
        view
    }

    /// Attaches to a session, or starts one in the view's folder, reading
    /// what it sends.
    fn connect(&mut self, session: Option<&str>, cx: &mut Context<Self>) {
        let (tx, rx) = async_channel::unbounded();
        let conn = termconn::attach_in(
            &self.client,
            &self.env,
            session,
            self.dir.as_deref(),
            self.screen.identity(),
            self.cols,
            self.rows,
            move |e| tx.send_blocking(e).is_ok(),
        );
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            while let Ok(first) = rx.recv().await {
                let alive = this.update(cx, |t, cx| {
                    let start = Instant::now();
                    t.take(first, cx);
                    while start.elapsed() < FEED_BUDGET {
                        let Ok(more) = rx.try_recv() else { break };
                        t.take(more, cx);
                    }
                    if let Some(text) = t.copied.take() {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    }
                    // A synchronized update is drawn once it ends, or after
                    // a second if the program never ends it, as Ghostty does.
                    if t.screen.synchronized() {
                        t.redraw_in(Duration::from_secs(1), cx);
                    } else {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    return;
                }
            }
        })
        .detach();
        self.conn = Some(conn);
    }

    /// Starts another session in the view once its shell has exited, as
    /// the web terminal's Start another does: a clean screen, a new shell.
    pub fn start_another(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.state, State::Exited(_) | State::Failed(_)) {
            return;
        }
        self.screen = screen(self.emulator, self.cols, self.rows, self.colours);
        self.screens += 1;
        self.progress = None;
        let cell = self.cell_pixels();
        self.screen
            .resize(self.cols as usize, self.rows as usize, cell);
        self.state = State::Connecting;
        self.session = None;
        self.title.clear();
        self.history = 0;
        self.sizing = None;
        self.connect(None, cx);
        cx.notify();
    }

    /// Types a command into the new session once it has started, as
    /// Open Shell in a container does.
    pub fn run_once_started(&mut self, command: impl Into<String>) {
        self.run = Some(command.into());
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

    pub fn sizing(&self) -> Option<Sizing> {
        self.sizing
    }

    /// Whether this window's size is the terminal's: until the session says,
    /// it is taken to be.
    fn holds_size(&self) -> bool {
        self.sizing.is_none_or(|s| s.owner)
    }

    /// Someone types, pastes or clicks here: the window takes the size, as
    /// the web's terminal does.
    fn claim(&mut self) {
        if let Some(s) = &mut self.sizing
            && !s.owner
        {
            s.owner = true;
            self.send(Command::Resize(self.cols, self.rows));
            self.resize_screen(self.cols, self.rows);
        }
    }

    /// Fit to This Window: the size taken, the keys given back.
    fn fit_here(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.claim();
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Types text into the session, as if it had been typed.
    pub fn type_text(&mut self, text: &str) {
        self.claim();
        self.send(Command::Input(text.as_bytes().to_vec()));
    }

    /// Draws a picture at the cursor, as wide as it is (narrowed to the
    /// screen) and as many rows tall as it takes: an anchor where it
    /// starts, then the cursor below it, as a terminal moves it past one.
    fn place(&mut self, p: crate::termimage::Picture) {
        let format = match p.format {
            crate::termimage::Format::Png => gpui::ImageFormat::Png,
            crate::termimage::Format::Jpeg => gpui::ImageFormat::Jpeg,
            crate::termimage::Format::Gif => gpui::ImageFormat::Gif,
            crate::termimage::Format::Webp => gpui::ImageFormat::Webp,
        };
        let cell = f32::from(self.cell.unwrap_or(px(7.5)));
        let room = (self.cols as f32 * cell).max(cell);
        let (mut w, mut h) = (
            p.width as f32 / PIXELS_PER_POINT,
            p.height as f32 / PIXELS_PER_POINT,
        );
        if w > room {
            h *= room / w;
            w = room;
        }
        let rows = (h / f32::from(self.line_height)).ceil().max(1.) as usize;
        let id = self.next_picture;
        self.next_picture += 1;
        self.pictures.insert(
            id,
            Shown {
                image: std::sync::Arc::new(gpui::Image::from_bytes(format, p.bytes)),
                width: px(w),
                height: px(h),
            },
        );
        let mut anchor = format!("\x1b]8;;{PICTURE}{id}\x1b\\ \x1b]8;;\x1b\\\r").into_bytes();
        anchor.extend(std::iter::repeat_n(b'\n', rows));
        self.screen.feed(&anchor);
    }

    /// The pictures the emulator placed, as elements where they are drawn:
    /// those under the text, then those over it. A picture's pixels are
    /// read once a generation of them, and let go once it is not shown.
    fn kitty(
        &mut self,
        placed: &[Placement],
        window: &mut Window,
        cx: &mut App,
    ) -> Vec<termpaint::Picture> {
        let mut out = vec![];
        if placed.is_empty() && self.kitty.is_empty() {
            return out;
        }
        // Placements are in pixels of the cell size the emulator was
        // given, the screen's, which a cell's points are rounded from.
        let cell = self.cell.unwrap_or(px(7.5));
        let (cw, ch) = self.cell_pixels();
        let x = f32::from(cell) / cw.max(1) as f32;
        let y = f32::from(self.line_height) / ch.max(1) as f32;
        let mut shown = HashMap::new();
        for p in placed {
            let key = (self.screens, p.image, p.generation, p.source);
            let image = if let Some(i) = shown.get(&key) {
                Arc::clone(i)
            } else if let Some(i) = self.kitty.remove(&key) {
                i
            } else if let Some(i) = self
                .screen
                .picture(p.image)
                .and_then(|pic| picture_image(&pic, p.source))
            {
                i
            } else {
                continue;
            };
            shown.insert(key, Arc::clone(&image));
            let at = Bounds::new(
                point(
                    cell * p.col as f32 + px(p.offset.0 as f32 * x),
                    self.line_height * p.row as f32 + px(p.offset.1 as f32 * y),
                ),
                size(px(p.width as f32 * x), px(p.height as f32 * y)),
            );
            out.push(termpaint::Picture {
                layer: p.layer,
                at,
                image,
            });
        }
        for (_, gone) in std::mem::replace(&mut self.kitty, shown) {
            cx.drop_image(gone, Some(window));
        }
        out
    }

    /// Whether a blinking cursor is lit: the first half of each 1.2 s
    /// since the last key or output, as Ghostty blinks it, with the view
    /// drawn again at the turn.
    fn blink_lit(&mut self, cx: &mut Context<Self>) -> bool {
        const HALF: Duration = Duration::from_millis(600);
        let since = self.blink.elapsed();
        let turns = since.as_millis() / HALF.as_millis();
        self.redraw_in(HALF * (turns as u32 + 1) - since, cx);
        turns.is_multiple_of(2)
    }

    /// Draws the view again after a while, unless a redraw is due sooner.
    fn redraw_in(&mut self, after: Duration, cx: &mut Context<Self>) {
        let due = Instant::now() + after;
        if self.redraw.as_ref().is_some_and(|(at, _)| *at <= due) {
            return;
        }
        let task = cx.spawn(async move |this: WeakEntity<Self>, cx| {
            cx.background_executor().timer(after).await;
            let _ = this.update(cx, |t, cx| {
                t.redraw = None;
                cx.notify();
            });
        });
        self.redraw = Some((due, task));
    }

    /// A cell's size in pixels, for programs that ask.
    fn cell_pixels(&self) -> (u32, u32) {
        let w = f32::from(self.cell.unwrap_or(px(7.5))) * PIXELS_PER_POINT;
        let h = f32::from(self.line_height) * PIXELS_PER_POINT;
        (w.round() as u32, h.round() as u32)
    }

    /// The pixel under the pointer, from the screen's top left, in the
    /// cell size programs are told.
    fn pixel_at(&self, at: Point<Pixels>) -> (u32, u32) {
        let cell = f32::from(self.cell.unwrap_or(px(7.5)));
        let (cw, ch) = self.cell_pixels();
        let x = f32::from(at.x - self.origin.x).max(0.) / cell * cw as f32;
        let y = f32::from(at.y - self.origin.y).max(0.) / f32::from(self.line_height) * ch as f32;
        (x as u32, y as u32)
    }

    fn send(&self, c: Command) {
        if let Some(conn) = &self.conn {
            conn.send(c);
        }
    }

    fn take(&mut self, e: Event, cx: &mut Context<Self>) {
        self.take_event(e);
        for s in std::mem::take(&mut self.signals) {
            cx.emit(s);
        }
    }

    fn take_event(&mut self, e: Event) {
        match e {
            Event::Attached {
                session,
                title,
                history,
                owner,
            } => {
                // Attached again, the session's past is replayed: drawn on
                // a clean screen, not over what was shown.
                if self.session.is_some() && self.state != State::Connecting {
                    self.screen = screen(self.emulator, self.cols, self.rows, self.colours);
                    self.screens += 1;
                    self.progress = None;
                    let cell = self.cell_pixels();
                    self.screen
                        .resize(self.cols as usize, self.rows as usize, cell);
                }
                self.session = Some(session);
                self.title = title;
                self.history = history;
                self.state = State::Live;
                self.sizing = self.sizing.map(|s| Sizing { owner, ..s });
                // The size this window would have: the session's, if it
                // holds the size.
                self.send(Command::Prefer(self.cols, self.rows));
                if let Some(run) = self.run.take() {
                    self.send(Command::Input(format!("{run}\r").into_bytes()));
                }
            }
            Event::Output(b) => {
                self.blink = Instant::now();
                // What was under the pointer may have moved; the next move
                // finds it again.
                if self.hover.take().is_some() {
                    self.screen.set_hover(None);
                }
                let replayed = self.history.min(b.len());
                self.history -= replayed;
                let mut extra = Vec::new();
                for piece in self.extract.split(&b) {
                    match piece {
                        crate::termimage::Piece::Text(t) => self.screen.feed(&t),
                        // What the past posted was seen then.
                        crate::termimage::Piece::Notification(n) => {
                            if replayed == 0 {
                                self.signals.push(Signal::Notification {
                                    title: n.title,
                                    body: n.body,
                                });
                            }
                        }
                        crate::termimage::Piece::Progress(p) => {
                            self.progress = (p.state != 0).then(|| (p, Instant::now()))
                        }
                        crate::termimage::Piece::Picture(p) => self.place(p),
                        crate::termimage::Piece::Reply(r) => extra.extend(r),
                        crate::termimage::Piece::Directory(d) => self.cwd = Some(d),
                    }
                }
                self.apply_effects(extra, replayed > 0);
            }
            Event::Size {
                cols,
                rows,
                viewers,
                owner,
            } => {
                self.sizing = Some(Sizing {
                    cols,
                    rows,
                    viewers,
                    owner,
                });
                // Drawn at the session's size while another window holds
                // it, so the program's screen is as it drew it.
                let (c, r) = if owner {
                    (self.cols, self.rows)
                } else {
                    (cols, rows)
                };
                self.resize_screen(c, r);
            }
            Event::Reconnecting { why, retry_in } => {
                self.state =
                    State::Reconnecting(format!("{why}; trying again in {}", seconds(retry_in)));
            }
            Event::Exited(code) => {
                self.state = State::Exited(code);
                self.conn = None;
                self.progress = None;
                if self.statuses.ended() {
                    self.signals.push(Signal::Status);
                }
            }
            Event::Failed(e) => {
                self.state = State::Failed(e);
                self.conn = None;
            }
        }
    }

    /// Takes what the program asked of the terminal, with replies of the
    /// view's own (kitty's): the replies, the title, the text copied, a
    /// reset. The past, replayed, asked what was answered then.
    fn apply_effects(&mut self, extra: Vec<u8>, replayed: bool) {
        let effects = self.screen.effects();
        let mut replies = effects.reply;
        replies.extend(extra);
        let mut changed = false;
        if effects.reset {
            self.title.clear();
            self.progress = None;
            changed |= self.statuses.clear();
        }
        // The past's bell rang then, and what it said of its work has
        // moved on.
        if !replayed {
            if effects.bell {
                self.signals.push(Signal::Bell);
            }
            for s in effects.statuses {
                changed |= self.statuses.report(s);
            }
        }
        if effects.prompt_started {
            changed |= self.statuses.ended();
        }
        if changed {
            self.signals.push(Signal::Status);
        }
        if !replayed && effects.copied.is_some() {
            self.copied = effects.copied;
        }
        // What the program asks is answered by the window holding the size
        // alone, since every window watching would answer, and the program
        // read every answer.
        if !replayed && !replies.is_empty() && self.holds_size() {
            self.send(Command::Input(replies));
        }
        if let Some(t) = effects.title {
            self.title = t;
        }
    }

    /// Resizes the screen, answering at once a program that asked to hear
    /// of it (mode 2048).
    fn resize_screen(&mut self, cols: u16, rows: u16) {
        let cell = self.cell_pixels();
        self.screen.resize(cols as usize, rows as usize, cell);
        self.apply_effects(vec![], false);
    }

    /// Fits the screen to bounds, telling the session its new size.
    fn fit(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let cell = *self.cell.get_or_insert_with(|| {
            let id = window.text_system().resolve_font(&self.font);
            window
                .text_system()
                .advance(id, self.font_size, 'm')
                .map(|s| s.width)
                .unwrap_or(px(7.5))
        });
        let cols = (f32::from(bounds.size.width) / f32::from(cell))
            .floor()
            .max(2.) as u16;
        let rows = (f32::from(bounds.size.height) / f32::from(self.line_height))
            .floor()
            .max(1.) as u16;
        self.origin = bounds.origin;
        if (cols, rows) != (self.cols, self.rows) {
            (self.cols, self.rows) = (cols, rows);
            if self.holds_size() {
                self.resize_screen(cols, rows);
            }
            self.send(Command::Prefer(cols, rows));
            cx.notify();
        }
    }

    fn key(&mut self, e: &KeyDownEvent, cx: &mut Context<Self>) {
        let k = &e.keystroke;
        if k.key == "enter" && matches!(self.state, State::Exited(_)) {
            self.start_another(cx);
            return;
        }
        if k.modifiers.platform {
            return;
        }
        let key = crate::termkeys::Key {
            name: &k.key,
            text: k.key_char.as_deref(),
            shift: k.modifiers.shift,
            alt: k.modifiers.alt,
            ctrl: k.modifiers.control,
        };
        let modes = self.screen.key_modes(!cfg!(target_os = "macos"));
        if let Some(b) = crate::termkeys::encode(&key, &modes) {
            self.input(b);
            self.seen(cx);
            cx.stop_propagation();
        }
    }

    /// What its programs say they are doing that most needs the user.
    pub fn status(&self) -> Option<crate::termstatus::Shown> {
        self.statuses.shown()
    }

    /// The user has seen the terminal: what its programs finished or
    /// failed at is let go.
    pub fn seen(&mut self, cx: &mut Context<Self>) {
        if self.statuses.seen() {
            cx.emit(Signal::Status);
        }
    }

    /// Sends what a key types, back at the bottom of the screen.
    fn input(&mut self, b: Vec<u8>) {
        self.blink = Instant::now();
        self.claim();
        if self.screen.scrolled_back() > 0 {
            self.screen.scroll(-(self.screen.scrolled_back() as i32));
        }
        self.send(Command::Input(b));
    }

    /// The wheel: to a program that asked for the mouse, its reports; in a
    /// full-screen program that asked, the cursor keys; else through the
    /// scrollback.
    fn scroll(&mut self, e: &ScrollEvent, cx: &mut Context<Self>) {
        use crate::termkeys::{Button, Press};
        self.scroll_left += f32::from(e.delta.pixel_delta(self.line_height).y);
        let lines = (self.scroll_left / f32::from(self.line_height)).trunc();
        if lines == 0. {
            return;
        }
        self.scroll_left -= lines * f32::from(self.line_height);
        let up = lines > 0.;
        let mouse = self.screen.mouse_modes();
        if mouse.any() || self.screen.wheel_is_arrows() {
            self.claim();
        }
        if mouse.any() {
            let (row, col, _) = self.cell_at(e.position);
            let pixel = self.pixel_at(e.position);
            let button = if up {
                Button::WheelUp
            } else {
                Button::WheelDown
            };
            for _ in 0..lines.abs() as usize {
                let m = &e.modifiers;
                if let Some(b) = crate::termkeys::mouse(
                    button,
                    Press::Down,
                    col,
                    row,
                    pixel,
                    m.shift,
                    m.alt,
                    m.control,
                    &mouse,
                ) {
                    self.send(Command::Input(b));
                }
            }
        } else if self.screen.wheel_is_arrows() {
            let modes = self.screen.key_modes(false);
            let key = crate::termkeys::Key {
                name: if up { "up" } else { "down" },
                ..Default::default()
            };
            if let Some(b) = crate::termkeys::encode(&key, &modes) {
                for _ in 0..lines.abs() as usize {
                    self.send(Command::Input(b.clone()));
                }
            }
        } else {
            self.screen.scroll(lines as i32);
            cx.notify();
        }
    }

    /// Tells the program of the mouse, if it asked and Shift is not held
    /// (Shift selects, as in other terminals); false when it did not.
    fn report(
        &mut self,
        button: crate::termkeys::Button,
        press: crate::termkeys::Press,
        at: Point<Pixels>,
        m: &gpui::Modifiers,
    ) -> bool {
        let modes = self.screen.mouse_modes();
        if !modes.any() || m.shift {
            return false;
        }
        // A press is someone using this window; the pointer passing over
        // it is not.
        if press != crate::termkeys::Press::Moved {
            self.claim();
        }
        let (row, col, _) = self.cell_at(at);
        let (row, col) = (
            row.min(self.rows as usize - 1),
            col.min(self.cols as usize - 1),
        );
        let pixel = self.pixel_at(at);
        if let Some(b) = crate::termkeys::mouse(
            button, press, col, row, pixel, false, m.alt, m.control, &modes,
        ) {
            self.send(Command::Input(b));
        }
        true
    }

    /// Copies what is selected.
    pub fn copy(&self, cx: &mut App) {
        if let Some(text) = self.screen.selection() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    /// Sends the clipboard's text, as the program asks for a paste to be
    /// marked, its line ends as Enter.
    fn paste(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
            let text = text.replace("\r\n", "\r").replace('\n', "\r");
            let b = if self.screen.bracketed_paste() {
                format!("\x1b[200~{text}\x1b[201~")
            } else {
                text
            };
            self.input(b.into_bytes());
        }
    }

    fn select_all(&mut self, cx: &mut Context<Self>) {
        self.screen.select_all();
        cx.notify();
    }

    fn clear_buffer(&mut self, cx: &mut Context<Self>) {
        self.screen.clear_buffer();
        if let Some(input) = &self.find {
            let query = input.read(cx).value().to_string();
            self.find_text(&query, true, cx);
        }
        cx.notify();
    }

    /// A right-press, taken before the menu opens on it: to a program that
    /// asked for the mouse, its report and no menu (Shift still gives the
    /// menu); else the link under it is kept for the menu.
    fn right_press(&mut self, e: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        use crate::termkeys::{Button, Press};
        if e.button != MouseButton::Right {
            return;
        }
        window.focus(&self.focus, cx);
        if self.report(Button::Right, Press::Down, e.position, &e.modifiers) {
            self.reporting = Some(Button::Right);
            cx.stop_propagation();
            return;
        }
        let (row, col, _) = self.cell_at(e.position);
        self.menu_link = self.screen.link_at(row, col).map(|l| l.kind);
    }

    /// Asks the tab around the terminal to open a link: it knows the
    /// environment's files, and its ports' addresses.
    fn open_link(&mut self, link: LinkKind, cx: &mut Context<Self>) {
        let dir = self.cwd.clone().or_else(|| self.dir.clone());
        cx.emit(OpenLink { link, dir });
    }

    /// Underlines the link under the pointer, if any, and keeps it for the
    /// hint.
    fn hover_at(&mut self, at: Point<Pixels>, m: &Modifiers, cx: &mut Context<Self>) {
        let (row, col, _) = self.cell_at(at);
        let link = self.screen.link_at(row, col);
        let opens = opens_link(m);
        let changed = match (&self.hover, &link) {
            (Some((was, _)), Some(now)) => !was.same_place(now),
            (None, None) => false,
            _ => true,
        };
        if changed {
            self.screen.set_hover(link.as_ref());
        }
        let moved = link.is_some();
        self.hover = link.map(|l| (l, at - self.origin));
        if changed || moved || opens != self.opens {
            self.opens = opens;
            cx.notify();
        }
    }

    /// The pointer left the terminal: no link is under it.
    fn leave(&mut self, cx: &mut Context<Self>) {
        if self.hover.take().is_some() {
            self.screen.set_hover(None);
            cx.notify();
        }
    }

    /// The cell under the pointer: its row, its column, and whether the
    /// pointer is on its right half.
    fn cell_at(&self, at: Point<Pixels>) -> (usize, usize, bool) {
        let cell = f32::from(self.cell.unwrap_or(px(7.5)));
        let x = (f32::from(at.x - self.origin.x) / cell).max(0.);
        let y = (f32::from(at.y - self.origin.y) / f32::from(self.line_height)).max(0.);
        (y as usize, x as usize, x.fract() >= 0.5)
    }

    /// A press starts a selection: a click cells, a double click words, a
    /// triple click lines; with Shift it carries the one there is on.
    fn press(&mut self, e: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        use crate::termkeys::{Button, Press};
        let button = match e.button {
            MouseButton::Left => Button::Left,
            MouseButton::Middle => Button::Middle,
            MouseButton::Right => Button::Right,
            _ => return,
        };
        // ⌘-click (Ctrl-click elsewhere) opens a link.
        if opens_link(&e.modifiers) && button == Button::Left {
            let (row, col, _) = self.cell_at(e.position);
            if let Some(link) = self.screen.link_at(row, col) {
                self.open_link(link.kind, cx);
            }
            return;
        }
        if self.report(button, Press::Down, e.position, &e.modifiers) {
            self.reporting = Some(button);
            return;
        }
        if e.button != MouseButton::Left {
            return;
        }
        let (row, col, right) = self.cell_at(e.position);
        if e.modifiers.shift && self.screen.selection().is_some() {
            self.screen.select_to(row, col, right);
        } else {
            // A drag with Option (Ctrl-Alt elsewhere) selects a rectangle,
            // as in Ghostty.
            let m = &e.modifiers;
            let block = if cfg!(target_os = "macos") {
                m.alt
            } else {
                m.alt && (m.control || m.platform)
            };
            let how = match e.click_count {
                0 | 1 if block => Select::Block,
                0 | 1 => Select::Cells,
                2 => Select::Words,
                _ => Select::Lines,
            };
            self.screen.select(row, col, right, how);
        }
        self.selecting = true;
        cx.notify();
    }

    fn drag(&mut self, e: &MouseMoveEvent, cx: &mut Context<Self>) {
        use crate::termkeys::{Button, Press};
        if e.pressed_button.is_none() {
            self.hover_at(e.position, &e.modifiers, cx);
        }
        let held = self.reporting.unwrap_or(Button::None);
        if (self.reporting.is_some() || !self.selecting)
            && self.report(held, Press::Moved, e.position, &e.modifiers)
        {
            return;
        }
        if !self.selecting || e.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let (row, col, right) = self.cell_at(e.position);
        self.screen.select_to(row, col, right);
        cx.notify();
    }

    /// A click that selected nothing clears the selection.
    fn release(&mut self, e: &MouseUpEvent, cx: &mut Context<Self>) {
        if let Some(button) = self.reporting.take() {
            self.report(button, crate::termkeys::Press::Up, e.position, &e.modifiers);
            return;
        }
        self.selecting = false;
        if self.screen.selection().is_none() {
            self.screen.clear_selection();
        }
        cx.notify();
    }

    /// Opens Find over the terminal, its query focused, starting from what
    /// is selected.
    pub fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = match &self.find {
            Some(i) => i.clone(),
            None => {
                let start = self
                    .screen
                    .selection()
                    .filter(|s| !s.contains('\n'))
                    .unwrap_or_default();
                let input = cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder("Find")
                        .default_value(start)
                });
                self._find = Some(cx.subscribe_in(
                    &input,
                    window,
                    |t, input, e: &InputEvent, _, cx| {
                        let query = input.read(cx).value().to_string();
                        match e {
                            InputEvent::Change => t.find_text(&query, true, cx),
                            InputEvent::PressEnter { shift, .. } => t.find_text(&query, !shift, cx),
                            _ => {}
                        }
                    },
                ));
                self.find = Some(input.clone());
                let query = input.read(cx).value().to_string();
                self.find_text(&query, true, cx);
                input
            }
        };
        window.focus(&input.focus_handle(cx), cx);
        cx.notify();
    }

    /// Whether Find is open, and if so whether its query is found.
    pub fn finding(&self) -> Option<bool> {
        self.find.as_ref().map(|_| !self.missing)
    }

    /// Finds the query, up (older) or down (newer).
    fn find_text(&mut self, query: &str, up: bool, cx: &mut Context<Self>) {
        self.missing = !query.is_empty() && !self.screen.find(query, up);
        cx.notify();
    }

    /// Closes Find, giving the terminal the keyboard back.
    pub fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find = None;
        self._find = None;
        self.missing = false;
        self.screen.find("", true);
        window.focus(&self.focus, cx);
        cx.notify();
    }
}

impl TerminalView {
    /// Says what a click on the link under the pointer does, just below
    /// its line (above it on the last rows), as VS Code's terminal does.
    fn link_hint(&self, p: &palette::Palette) -> Option<gpui::AnyElement> {
        let (link, at) = self.hover.as_ref()?;
        let line = f32::from(self.line_height);
        let row = (f32::from(at.y) / line).max(0.).floor();
        // The text's padding: 8 at the sides, 4 above.
        let below = row + 2. < self.rows as f32;
        let top = if below {
            4. + (row + 1.) * line + 2.
        } else {
            4. + row * line - 24.
        };
        // Toward the middle from the pointer, so it stays in the view.
        let width = self.cols as f32 * f32::from(self.cell.unwrap_or(px(7.5)));
        let x = f32::from(at.x);
        Some(
            div()
                .debug_selector(|| "term-link-hint".into())
                .absolute()
                .top(px(top.max(0.)))
                .map(|d| {
                    if x < width / 2. {
                        d.left(px(8. + x))
                    } else {
                        d.right(px((width - x).max(0.) + 8.))
                    }
                })
                .px(px(6.))
                .py(px(2.))
                .rounded(px(4.))
                .bg(p.surface)
                .border_1()
                .border_color(p.border)
                .shadow_md()
                .text_size(px(11.))
                .text_color(if self.opens { p.text } else { p.muted })
                .child(hint(&link.kind))
                .into_any_element(),
        )
    }

    /// Says when the terminal is drawn at another window's size, with
    /// fitting it to this one, and how many windows have it open, as the
    /// web's terminal does; None when there is nothing to say.
    fn size_bar(&self, p: &palette::Palette, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let s = self.sizing.filter(|_| self.state == State::Live)?;
        let other = !s.owner && (s.cols, s.rows) != (self.cols, self.rows);
        if !other && s.viewers < 2 {
            return None;
        }
        let viewers = s.viewers;
        Some(
            gpui_component::h_flex()
                .id("term-sizing")
                .debug_selector(|| "term-sizing".into())
                .absolute()
                .bottom(px(8.))
                .right(px(12.))
                .gap(px(10.))
                .pl(px(10.))
                .pr(px(4.))
                .py(px(3.))
                .rounded(px(6.))
                .bg(p.surface)
                .border_1()
                .border_color(p.border)
                .shadow_md()
                .text_size(px(11.))
                .text_color(p.muted)
                // Its clicks are its own, not a selection's in the text.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .when(viewers > 1, |d| {
                    d.child(
                        gpui_component::h_flex()
                            .id("term-viewers")
                            .gap(px(4.))
                            .child(
                                gpui::svg()
                                    .path(crate::assets::icon("eye"))
                                    .size(px(12.))
                                    .text_color(p.muted),
                            )
                            .child(viewers.to_string())
                            .tooltip(move |window, cx| {
                                gpui_component::tooltip::Tooltip::new(format!(
                                    "{viewers} windows have this terminal open"
                                ))
                                .build(window, cx)
                            }),
                    )
                })
                .when(other, |d| {
                    d.child(format!("Sized for another window ({}×{})", s.cols, s.rows))
                        .child(
                            crate::ui::button("term-fit", "Fit to This Window", p)
                                .debug_selector(|| "term-fit".into())
                                .on_click(cx.listener(|t, _, window, cx| t.fit_here(window, cx))),
                        )
                })
                .into_any_element(),
        )
    }
}

/// The terminal's menu, as IntelliJ's: the link right-clicked, if any; the
/// text; then the session.
fn terminal_menu(
    mut menu: gpui_component::menu::PopupMenu,
    link: Option<LinkKind>,
    me: gpui::WeakEntity<TerminalView>,
    selected: bool,
) -> gpui_component::menu::PopupMenu {
    if let Some(link) = link {
        let copied = match &link {
            LinkKind::Url(url) => url.clone(),
            LinkKind::Embedded { uri, .. } => uri.clone(),
            LinkKind::Path { path, line, column } if *column > 0 => {
                format!("{path}:{line}:{column}")
            }
            LinkKind::Path { path, line, .. } => format!("{path}:{line}"),
        };
        menu = menu
            .item(PopupMenuItem::new("Open Link").on_click(move |_, _, cx| {
                let _ = me.update(cx, |t, cx| t.open_link(link.clone(), cx));
            }))
            .item(
                PopupMenuItem::new("Copy Link")
                    .on_click(move |_, _, cx| crate::treeops::copy(copied.clone(), cx)),
            )
            .separator();
    }
    menu.menu_with_disabled("Copy", Box::new(CopySelection), !selected)
        .menu("Paste", Box::new(Paste))
        .menu("Select All", Box::new(SelectAll))
        .separator()
        .menu("Find", Box::new(Find))
        .menu("Clear Buffer", Box::new(ClearBuffer))
        .separator()
        .menu("New Session", Box::new(NewSession))
        .menu("Close Tab", Box::new(CloseSession))
        .menu("End Session…", Box::new(EndSession))
}

/// Whether a click with these keys opens a link: ⌘ on a Mac, Ctrl
/// elsewhere, as IntelliJ's terminal and VS Code's have it.
fn opens_link(m: &Modifiers) -> bool {
    if cfg!(target_os = "macos") {
        m.platform
    } else {
        m.control
    }
}

/// What the hint beside a link says a click does.
fn hint(link: &LinkKind) -> String {
    let key = if cfg!(target_os = "macos") {
        "⌘-click"
    } else {
        "Ctrl-click"
    };
    match link {
        LinkKind::Url(url) => match local_port(url) {
            Some((port, _)) => format!("{key} to open port {port} of this environment"),
            None => format!("{key} to open {url}"),
        },
        LinkKind::Embedded { uri, .. } => format!("{key} to open {uri}"),
        LinkKind::Path { path, line, .. } => format!("{key} to open {path} at line {line}"),
    }
}

/// An address on the machine itself -- localhost, 127.0.0.1, 0.0.0.0 or
/// [::1] -- which in a terminal of the environment's is the environment:
/// its port (the scheme's own when none is given), and the rest of the
/// address after it, from its path on.
pub fn local_port(url: &str) -> Option<(u16, String)> {
    let (scheme, rest) = url.split_once("://")?;
    let default = match scheme {
        "http" => 80,
        "https" => 443,
        _ => return None,
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !h.ends_with(':') && !p.contains(']') => (h, p.parse().ok()?),
        _ => (authority, default),
    };
    matches!(host, "localhost" | "127.0.0.1" | "0.0.0.0" | "[::1]")
        .then(|| (port, tail.to_string()))
}

/// A link opened in a terminal, by ⌘-click or its menu, for the tab around
/// it to follow. `dir` is where a relative path is from: the directory the
/// shell last said it was in, else the one the session started in.
pub struct OpenLink {
    pub link: LinkKind,
    pub dir: Option<String>,
}

impl gpui::EventEmitter<OpenLink> for TerminalView {}

/// What a terminal tells the window around it beyond its screen: its bell
/// rang, its program posted a notification (OSC 9, OSC 777), or what its
/// programs say they are doing changed (`TerminalView::status`).
#[derive(Debug, Clone, PartialEq)]
pub enum Signal {
    Bell,
    Notification { title: String, body: String },
    Status,
}

impl gpui::EventEmitter<Signal> for TerminalView {}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        // The theme's colours, given to the emulator when they change: each
        // time, it draws every row afresh.
        let colours = (rgb(p.term_fg), rgb(p.term_tab));
        if colours != self.colours {
            self.colours = colours;
            self.screen.set_default_colours(colours.0, colours.1);
            // A program that asked hears the terminal turn light or dark.
            if self.screen.reports_theme() && self.holds_size() {
                let scheme = if crate::terminal::is_light(colours.1) {
                    2
                } else {
                    1
                };
                self.send(Command::Input(format!("\x1b[?997;{scheme}n").into_bytes()));
            }
        }
        // A size chosen in the settings: the cell is measured again, and
        // the next fit tells the session how many rows and columns fit.
        let (size, line) = metrics(cx);
        if (size, line) != (self.font_size, self.line_height) {
            (self.font_size, self.line_height) = (size, line);
            self.cell = None;
        }
        let focused = self.focus.is_focused(window);
        // A program that asked hears the terminal gain and lose the keys:
        // its focus, in the window in front.
        let has_keys = focused && window.is_window_active();
        if has_keys != self.had_keys {
            self.had_keys = has_keys;
            if self.screen.reports_focus() && self.holds_size() {
                let report: &[u8] = if has_keys { b"\x1b[I" } else { b"\x1b[O" };
                self.send(Command::Input(report.to_vec()));
            }
        }
        if let Some(due) = self.screen.tick_animations() {
            self.redraw_in(due, cx);
        }
        let lines = self.screen.lines();
        let look = self.screen.cursor_look();
        let (row, col) = self.screen.cursor();
        // The cursor the program shows, at the bottom of the scrollback: as
        // it asks while the window has the keys, else a hollow block, as
        // Ghostty draws it.
        let lit = !focused || !look.blinking || self.blink_lit(cx);
        let cursor =
            (self.screen.scrolled_back() == 0 && self.screen.cursor_shown() && lit).then(|| {
                termpaint::Cursor {
                    row,
                    col,
                    style: if focused {
                        look.style
                    } else {
                        CursorStyle::HollowBlock
                    },
                    colour: look.colour.map(hsla),
                }
            });
        let placed = self.screen.placements();
        let kitty = self.kitty(&placed, window, cx);
        let painting = termpaint::Screen {
            lines,
            cell: self.cell.unwrap_or(px(7.5)),
            line_height: self.line_height,
            font: self.font.clone(),
            font_size: self.font_size,
            // The selection in the accent, Find's matches in amber, the one
            // it is on in solid amber.
            colours: termpaint::Colours {
                selected: p.accent.opacity(0.35),
                found: p.busy.opacity(0.3),
                current: p.busy,
                mark_text: p.term_bg,
                fg: p.term_fg,
            },
            cursor,
            pictures: kitty,
        };
        // The pictures on the screen, or starting above it: each where its
        // anchor is.
        let cell = self.cell.unwrap_or(px(7.5));
        let tallest = self
            .pictures
            .values()
            .map(|s| (f32::from(s.height) / f32::from(self.line_height)).ceil() as usize)
            .max()
            .unwrap_or(0);
        let pictures: Vec<gpui::AnyElement> = if self.pictures.is_empty() {
            vec![]
        } else {
            self.screen
                .anchors(PICTURE, tallest)
                .into_iter()
                .filter_map(|(row, col, name)| {
                    let shown = self.pictures.get(&name.parse::<u64>().ok()?)?;
                    Some(
                        gpui::img(shown.image.clone())
                            .absolute()
                            .left(cell * col as f32)
                            .top(self.line_height * row as f32)
                            .w(shown.width)
                            .h(shown.height)
                            .into_any_element(),
                    )
                })
                .collect()
        };
        // A report not renewed in 15 s is taken to be over, as Ghostty
        // takes it, so a program that died part way leaves no bar behind.
        const PROGRESS_LASTS: Duration = Duration::from_secs(15);
        let progress = match self.progress {
            Some((_, at)) if at.elapsed() >= PROGRESS_LASTS => {
                self.progress = None;
                None
            }
            Some((pr, at)) => {
                self.redraw_in(PROGRESS_LASTS - at.elapsed(), cx);
                // One that cannot say how far moves to and fro.
                if pr.state == 3 {
                    self.redraw_in(Duration::from_millis(33), cx);
                }
                Some((pr, at.elapsed()))
            }
            None => None,
        };
        let pointer = if self.opens && self.hover.is_some() {
            gpui::CursorStyle::PointingHand
        } else {
            pointer(self.screen.pointer())
        };
        let me = cx.entity().downgrade();
        let notice = match &self.state {
            State::Connecting => Some("Connecting…".to_string()),
            State::Exited(code) => Some(format!("The session has ended ({code}).")),
            State::Failed(e) => Some(format!("Could not connect: {e}")),
            State::Reconnecting(why) => Some(format!("Reconnecting: {why}")),
            State::Live => None,
        };
        let reconnecting = matches!(self.state, State::Reconnecting(_));
        let ended = matches!(self.state, State::Exited(_));
        div()
            .id("terminal")
            .debug_selector(|| "terminal".into())
            .track_focus(&self.focus)
            .key_context("Terminal")
            // Keys typed in Find, inside the terminal, are Find's.
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                if this.focus.is_focused(window) {
                    this.key(e, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &SendTab, window, _| {
                if this.focus.is_focused(window) {
                    this.input(b"\t".to_vec())
                }
            }))
            .on_action(cx.listener(|this, _: &SendBackTab, window, _| {
                if this.focus.is_focused(window) {
                    this.input(b"\x1b[Z".to_vec())
                }
            }))
            .on_scroll(cx.listener(|this, e: &ScrollEvent, _, cx| this.scroll(e, cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, window, cx| this.press(e, window, cx)),
            )
            .capture_any_mouse_down(
                cx.listener(|this, e: &MouseDownEvent, window, cx| this.right_press(e, window, cx)),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, e: &MouseDownEvent, window, cx| this.press(e, window, cx)),
            )
            .on_mouse_up(
                MouseButton::Right,
                cx.listener(|this, e: &MouseUpEvent, _, cx| this.release(e, cx)),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(|this, e: &MouseUpEvent, _, cx| this.release(e, cx)),
            )
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| this.drag(e, cx)))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !*hovered {
                    this.leave(cx)
                }
            }))
            .on_modifiers_changed(cx.listener(|this, e: &gpui::ModifiersChangedEvent, _, cx| {
                let opens = opens_link(&e.modifiers);
                if opens != this.opens {
                    this.opens = opens;
                    if this.hover.is_some() {
                        cx.notify();
                    }
                }
            }))
            // The program's pointer; with the key that opens one held, a
            // link is a link.
            .cursor(pointer)
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, e: &MouseUpEvent, _, cx| this.release(e, cx)),
            )
            .on_action(cx.listener(|this, _: &Find, window, cx| this.open_find(window, cx)))
            .on_action(cx.listener(|this, _: &CopySelection, _, cx| this.copy(cx)))
            .on_action(cx.listener(|this, _: &Paste, _, cx| this.paste(cx)))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| this.select_all(cx)))
            .on_action(cx.listener(|this, _: &ClearBuffer, _, cx| this.clear_buffer(cx)))
            .on_action(cx.listener(|this, _: &PreviousPrompt, _, cx| {
                this.screen.scroll_prompts(-1);
                cx.notify()
            }))
            .on_action(cx.listener(|this, _: &NextPrompt, _, cx| {
                this.screen.scroll_prompts(1);
                cx.notify()
            }))
            // The Edit menu's.
            .on_action(cx.listener(|this, _: &gpui_component::input::Copy, _, cx| this.copy(cx)))
            .on_action(cx.listener(|this, _: &gpui_component::input::Paste, _, cx| this.paste(cx)))
            .on_action(
                cx.listener(|this, _: &gpui_component::input::SelectAll, _, cx| {
                    this.select_all(cx)
                }),
            )
            .context_menu({
                let me = cx.entity().downgrade();
                let focus = self.focus.clone();
                move |menu, _, cx| {
                    crate::ui::modal(cx);
                    let (link, selected) = me
                        .upgrade()
                        .map(|t| {
                            let t = t.read(cx);
                            (t.menu_link.clone(), t.screen.selection().is_some())
                        })
                        .unwrap_or_default();
                    terminal_menu(
                        menu.action_context(focus.clone()),
                        link,
                        me.clone(),
                        selected,
                    )
                }
            })
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
                    .child(
                        canvas(
                            move |bounds, window, cx| {
                                if let Some(me) = me.upgrade() {
                                    me.update(cx, |t, cx| t.fit(bounds, window, cx));
                                }
                            },
                            move |bounds, _, window, cx| {
                                termpaint::paint(&painting, bounds, window, cx)
                            },
                        )
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                    )
                    .children(pictures),
            )
            // A program's progress, along the top, as the web's terminal
            // shows it: red when it failed, amber paused, and a part moving
            // to and fro when it cannot say how far.
            .when_some(progress, |d, (pr, age)| {
                let colour = match pr.state {
                    2 => p.bad,
                    4 => p.busy,
                    _ => p.accent,
                };
                let bar = if pr.state == 3 {
                    // A third of the width, across and back every 2 s.
                    let phase = (age.as_secs_f32() / 2.).fract();
                    let across = 1. - (2. * phase - 1.).abs();
                    div()
                        .absolute()
                        .top_0()
                        .h_full()
                        .left(gpui::relative(across * 2. / 3.))
                        .w(gpui::relative(1. / 3.))
                } else {
                    div().h_full().w(gpui::relative(pr.value as f32 / 100.))
                };
                d.child(
                    div()
                        .id("terminal-progress")
                        .debug_selector(|| "terminal-progress".into())
                        .absolute()
                        .top_0()
                        .left_0()
                        .w_full()
                        .h(px(3.))
                        .bg(p.surface_2)
                        .child(bar.bg(colour))
                        .tooltip(|window, cx| {
                            gpui_component::tooltip::Tooltip::new("Progress the program reports")
                                .build(window, cx)
                        }),
                )
            })
            .when_some(self.find.clone(), |d, input| {
                let missing = self.missing;
                d.child(
                    div()
                        .absolute()
                        .top(px(4.))
                        .right(px(12.))
                        .w(px(260.))
                        // Escape closes Find, taken before the query's
                        // field, which would keep it.
                        .capture_action(cx.listener(
                            |this, _: &gpui_component::input::Escape, window, cx| {
                                cx.stop_propagation();
                                this.close_find(window, cx)
                            },
                        ))
                        .p(px(4.))
                        .rounded(px(6.))
                        .bg(p.surface)
                        .border_1()
                        .border_color(if missing { p.bad } else { p.border })
                        .shadow_md()
                        .child(Input::new(&input).small()),
                )
            })
            .when_some(self.size_bar(&p, cx), |d, bar| d.child(bar))
            .when_some(self.link_hint(&p), |d, hint| d.child(hint))
            .when_some(notice, |d, n| {
                d.child(
                    gpui_component::h_flex()
                        .absolute()
                        .bottom(px(6.))
                        .right(px(10.))
                        .gap(px(8.))
                        .text_size(px(11.))
                        .text_color(if reconnecting { p.busy } else { p.idle })
                        .child(n)
                        .when(ended, |d| {
                            d.child(
                                crate::ui::button("start-another", "Start another", &p)
                                    .debug_selector(|| "start-another".into())
                                    .on_click(cx.listener(|t, _, window, cx| {
                                        window.focus(&t.focus, cx);
                                        t.start_another(cx)
                                    })),
                            )
                        }),
                )
            })
    }
}

/// The platform's pointer for one a program asks for, by its W3C name.
fn pointer(p: crate::terminal::Pointer) -> gpui::CursorStyle {
    use crate::terminal::Pointer as P;
    use gpui::CursorStyle as C;
    match p {
        P::Text => C::IBeam,
        P::VerticalText => C::IBeamCursorForVerticalLayout,
        P::Pointer => C::PointingHand,
        P::Crosshair | P::Cell => C::Crosshair,
        P::ContextMenu => C::ContextualMenu,
        P::Copy => C::DragCopy,
        P::Alias => C::DragLink,
        P::NoDrop | P::NotAllowed => C::OperationNotAllowed,
        P::Grab => C::OpenHand,
        P::Grabbing | P::Move | P::AllScroll => C::ClosedHand,
        P::ColResize => C::ResizeColumn,
        P::RowResize => C::ResizeRow,
        P::NResize => C::ResizeUp,
        P::SResize => C::ResizeDown,
        P::EResize => C::ResizeRight,
        P::WResize => C::ResizeLeft,
        P::NsResize => C::ResizeUpDown,
        P::EwResize => C::ResizeLeftRight,
        P::NeResize | P::SwResize | P::NeswResize => C::ResizeUpRightDownLeft,
        P::NwResize | P::SeResize | P::NwseResize => C::ResizeUpLeftDownRight,
        P::Default | P::Help | P::Progress | P::Wait | P::ZoomIn | P::ZoomOut => C::Arrow,
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

/// The part of a picture shown, as gpui draws it (BGRA).
fn picture_image(
    pic: &hangar_ghostty::Picture,
    (x, y, w, h): (u32, u32, u32, u32),
) -> Option<Arc<gpui::RenderImage>> {
    let (x, y) = (x.min(pic.width), y.min(pic.height));
    let (w, h) = (w.min(pic.width - x), h.min(pic.height - y));
    if w == 0 || h == 0 {
        return None;
    }
    let mut bgra = Vec::with_capacity(w as usize * h as usize * 4);
    for row in y..y + h {
        let start = (row as usize * pic.width as usize + x as usize) * 4;
        let line = pic.rgba.get(start..start + w as usize * 4)?;
        for p in line.as_chunks::<4>().0 {
            bgra.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
        }
    }
    let buffer = image::RgbaImage::from_raw(w, h, bgra)?;
    Some(Arc::new(gpui::RenderImage::new([image::Frame::new(
        buffer,
    )])))
}
