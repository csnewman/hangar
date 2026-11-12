//! A terminal session in the window: its screen drawn as styled text in
//! the design's terminal colours, sized to fit where it is shown, with what
//! is typed and pasted sent to the session. The mouse selects, as in any
//! terminal, and Find looks through the output. Its menu is IntelliJ's
//! terminal's: copy, paste, select all, find, clear the buffer, and the
//! session's own commands.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{
    App, Bounds, ClipboardItem, Context, Entity, FocusHandle, Focusable, Font, Hsla, IntoElement,
    KeyBinding, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, Pixels, Point, Render, Rgba, ScrollEvent, Styled, StyledText, Subscription,
    TextRun, UnderlineStyle, WeakEntity, Window, actions, canvas, div, font, prelude::*, px,
};
use gpui_component::Sizable;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};

use crate::api::Client;
use crate::palette;
use crate::termconn::{self, Command, Connection, Event};
use crate::terminal::{Mark, Replies, Screen, Select};

/// The text's size, and the height of a line.
const FONT_SIZE: Pixels = px(12.5);
const LINE_HEIGHT: Pixels = px(17.);

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
    ]
);

/// Binds Tab and Shift-Tab to the shell, ahead of gpui-component's binding
/// of them to moving focus, which takes them before a key handler sees them;
/// and the menu's commands, with Cmd on macOS and Ctrl-Shift elsewhere,
/// where Ctrl and a letter are the shell's.
pub fn bind_keys(cx: &mut App) {
    let keys = if cfg!(target_os = "macos") {
        ["cmd-f", "cmd-c", "cmd-v", "cmd-a", "cmd-t", "cmd-w"]
    } else {
        [
            "ctrl-shift-f",
            "ctrl-shift-c",
            "ctrl-shift-v",
            "ctrl-shift-a",
            "ctrl-shift-t",
            "ctrl-shift-w",
        ]
    };
    let [find, copy, paste, all, new, close] = keys;
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
    /// Where the text starts in the window, for the mouse.
    origin: Point<Pixels>,
    /// Whether a drag is selecting.
    selecting: bool,
    /// The button held, while the program is told of the mouse.
    reporting: Option<crate::termkeys::Button>,
    /// The link under the last right-click, for the menu to open or copy.
    menu_link: Option<String>,
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
    progress: Option<crate::termimage::Progress>,
    /// The window's pixels to a point, for a picture's size.
    scale: f32,
}

/// A picture drawn in the terminal, and its size where it is drawn.
struct Shown {
    image: std::sync::Arc<gpui::Image>,
    width: Pixels,
    height: Pixels,
}

/// What names a picture's anchor: an OSC 8 link on the cell it starts at,
/// so it scrolls and clears with the text around it.
const PICTURE: &str = "hangar-picture:";

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
        let replies = Replies::default();
        let p = palette::of(cx);
        let colours = (rgb(p.term_fg), rgb(p.term_tab));
        let mut screen = Screen::with_listener(cols as usize, rows as usize, replies.clone());
        screen.set_default_colours(colours.0, colours.1);
        let mut view = TerminalView {
            screen,
            replies,
            colours,
            conn: None,
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
            origin: Point::default(),
            selecting: false,
            reporting: None,
            menu_link: None,
            sizing: None,
            find: None,
            missing: false,
            run: None,
            client: client.clone(),
            env: env.to_string(),
            dir: dir.map(str::to_string),
            _find: None,
            extract: Default::default(),
            pictures: HashMap::new(),
            next_picture: 0,
            progress: None,
            scale: 2.,
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
            self.cols,
            self.rows,
            move |e| tx.send_blocking(e).is_ok(),
        );
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
        self.conn = Some(conn);
    }

    /// Starts another session in the view once its shell has exited, as
    /// the web terminal's Start another does: a clean screen, a new shell.
    pub fn start_another(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.state, State::Exited(_) | State::Failed(_)) {
            return;
        }
        let mut screen =
            Screen::with_listener(self.cols as usize, self.rows as usize, self.replies.clone());
        screen.set_default_colours(self.colours.0, self.colours.1);
        self.screen = screen;
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
            self.screen.resize(self.cols as usize, self.rows as usize);
            self.send(Command::Resize(self.cols, self.rows));
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
        let (mut w, mut h) = (p.width as f32 / self.scale, p.height as f32 / self.scale);
        if w > room {
            h *= room / w;
            w = room;
        }
        let rows = (h / f32::from(LINE_HEIGHT)).ceil().max(1.) as usize;
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
                owner,
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
                self.sizing = self.sizing.map(|s| Sizing { owner, ..s });
                // The size this window would have: the session's, if it
                // holds the size.
                self.send(Command::Prefer(self.cols, self.rows));
                if let Some(run) = self.run.take() {
                    self.send(Command::Input(format!("{run}\r").into_bytes()));
                }
            }
            Event::Output(b) => {
                let replayed = self.history.min(b.len());
                self.history -= replayed;
                let mut extra = Vec::new();
                for piece in self.extract.split(&b) {
                    match piece {
                        crate::termimage::Piece::Text(t) => self.screen.feed(&t),
                        crate::termimage::Piece::Progress(p) => {
                            self.progress = (p.state != 0).then_some(p)
                        }
                        crate::termimage::Piece::Picture(p) => self.place(p),
                        crate::termimage::Piece::Reply(r) => extra.extend(r),
                    }
                }
                let (mut replies, title) = self.replies.take();
                replies.extend(extra);
                // What the past asked was answered then; what the program
                // asks is answered by the window holding the size alone,
                // since every window watching would answer, and the program
                // read every answer.
                if replayed == 0 && !replies.is_empty() && self.holds_size() {
                    self.send(Command::Input(replies));
                }
                if let Some(t) = title {
                    self.title = t;
                }
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
                self.screen.resize(c as usize, r as usize);
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
        self.origin = bounds.origin;
        self.scale = window.scale_factor();
        if (cols, rows) != (self.cols, self.rows) {
            (self.cols, self.rows) = (cols, rows);
            if self.holds_size() {
                self.screen.resize(cols as usize, rows as usize);
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
            cx.stop_propagation();
        }
    }

    /// Sends what a key types, back at the bottom of the screen.
    fn input(&mut self, b: Vec<u8>) {
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
        self.scroll_left += f32::from(e.delta.pixel_delta(LINE_HEIGHT).y);
        let lines = (self.scroll_left / f32::from(LINE_HEIGHT)).trunc();
        if lines == 0. {
            return;
        }
        self.scroll_left -= lines * f32::from(LINE_HEIGHT);
        let up = lines > 0.;
        let mouse = self.screen.mouse_modes();
        if mouse.any() || self.screen.wheel_is_arrows() {
            self.claim();
        }
        if mouse.any() {
            let (row, col, _) = self.cell_at(e.position);
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
        if let Some(b) =
            crate::termkeys::mouse(button, press, col, row, false, m.alt, m.control, &modes)
        {
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
        self.menu_link = self.screen.link_at(row, col);
    }

    /// The cell under the pointer: its row, its column, and whether the
    /// pointer is on its right half.
    fn cell_at(&self, at: Point<Pixels>) -> (usize, usize, bool) {
        let cell = f32::from(self.cell.unwrap_or(px(7.5)));
        let x = (f32::from(at.x - self.origin.x) / cell).max(0.);
        let y = (f32::from(at.y - self.origin.y) / f32::from(LINE_HEIGHT)).max(0.);
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
        // Cmd-click (Ctrl-click elsewhere) opens a link.
        let opens = if cfg!(target_os = "macos") {
            e.modifiers.platform
        } else {
            e.modifiers.control
        };
        if opens && button == Button::Left {
            let (row, col, _) = self.cell_at(e.position);
            if let Some(url) = self.screen.link_at(row, col) {
                cx.open_url(&url);
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
            let how = match e.click_count {
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
    link: Option<String>,
    selected: bool,
) -> gpui_component::menu::PopupMenu {
    if let Some(url) = link {
        let copied = url.clone();
        menu = menu
            .item(PopupMenuItem::new("Open Link").on_click(move |_, _, cx| cx.open_url(&url)))
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
        // The selection in the accent, Find's matches in amber, the one it
        // is on in solid amber.
        let selected = p.accent.opacity(0.35);
        let found = p.busy.opacity(0.3);
        let current = p.busy;
        let mark_text = p.term_bg;
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
                        color: match s.mark {
                            Mark::Current => mark_text,
                            _ => hsla(s.fg),
                        },
                        background_color: Some(match s.mark {
                            Mark::None => hsla(s.bg),
                            Mark::Selected => selected,
                            Mark::Match => found,
                            Mark::Current => current,
                        }),
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
        // The pictures on the screen, or starting above it: each where its
        // anchor is.
        let cell = self.cell.unwrap_or(px(7.5));
        let tallest = self
            .pictures
            .values()
            .map(|s| (f32::from(s.height) / f32::from(LINE_HEIGHT)).ceil() as usize)
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
                            .top(LINE_HEIGHT * row as f32)
                            .w(shown.width)
                            .h(shown.height)
                            .into_any_element(),
                    )
                })
                .collect()
        };
        let progress = self.progress;
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
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, e: &MouseUpEvent, _, cx| this.release(e, cx)),
            )
            .on_action(cx.listener(|this, _: &Find, window, cx| this.open_find(window, cx)))
            .on_action(cx.listener(|this, _: &CopySelection, _, cx| this.copy(cx)))
            .on_action(cx.listener(|this, _: &Paste, _, cx| this.paste(cx)))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| this.select_all(cx)))
            .on_action(cx.listener(|this, _: &ClearBuffer, _, cx| this.clear_buffer(cx)))
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
                    terminal_menu(menu.action_context(focus.clone()), link, selected)
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
                    .font(self.font.clone())
                    .text_size(FONT_SIZE)
                    .line_height(LINE_HEIGHT)
                    .children(rows)
                    .children(pictures)
                    .child(
                        canvas(
                            move |bounds, window, cx| {
                                if let Some(me) = me.upgrade() {
                                    me.update(cx, |t, cx| t.fit(bounds, window, cx));
                                }
                            },
                            |_, _, _, _| {},
                        )
                        // Pinned to the top left, so its bounds are the
                        // text's: an absolute box with no offsets sits
                        // after the rows.
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                    ),
            )
            // A program's progress, along the top, as the web's terminal
            // shows it: red when it failed, amber paused, full and faint
            // when it cannot say how far.
            .when_some(progress, |d, pr| {
                let colour = match pr.state {
                    2 => p.bad,
                    4 => p.busy,
                    3 => p.accent.opacity(0.4),
                    _ => p.accent,
                };
                let done = if pr.state == 3 {
                    1.
                } else {
                    pr.value as f32 / 100.
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
                        .child(div().h_full().w(gpui::relative(done)).bg(colour))
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

/// A wait as said: "0.5s", "4s".
fn seconds(d: Duration) -> String {
    if d < Duration::from_secs(1) {
        format!("{:.1}s", d.as_secs_f32())
    } else {
        format!("{}s", d.as_secs())
    }
}
