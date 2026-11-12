//! A terminal's screen: alacritty_terminal parses what the shell writes and
//! keeps the grid and its scrollback; this turns the visible part into lines
//! of styled spans, which the terminal view draws as text. It also keeps
//! what is selected, and finds text in the scrollback.

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::index::{Column, Direction, Line as GridLine, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor, Rgb};

/// What names a link that anchors something drawn over the text rather
/// than a link to follow.
const ANCHOR: &str = "hangar-";

/// What the grid is told it measures.
struct Size {
    columns: usize,
    lines: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.lines
    }

    fn screen_lines(&self) -> usize {
        self.lines
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

/// Where the terminal's events go: a bell, a title, a reply to the shell.
/// A screen fed by hand needs none of them.
pub struct Quiet;

impl EventListener for Quiet {
    fn send_event(&self, _: Event) {}
}

/// What a terminal says back to the shell, and what it calls itself, as
/// the program in it asks: the replies to its queries (its size, its
/// colours, the device's attributes), which a terminal must write back, and
/// the window title it sets.
#[derive(Clone, Default)]
pub struct Replies {
    pending: std::sync::Arc<std::sync::Mutex<(Vec<Reply>, Option<String>)>>,
}

/// A reply as the program asked for it: written as it is, or a colour
/// (OSC 4, 10, 11 and 12's queries), answered with the screen's own.
enum Reply {
    Bytes(Vec<u8>),
    Colour(usize, ColourReply),
}

type ColourReply = std::sync::Arc<dyn Fn(Rgb) -> String + Sync + Send + 'static>;

impl Replies {
    /// The replies since this was last called, in the order asked, colours
    /// answered by `colour` (`Screen::colour`), and the title, if it was set
    /// meanwhile.
    pub fn take(&self, colour: impl Fn(usize) -> Rgb) -> (Vec<u8>, Option<String>) {
        let mut p = self.pending.lock().unwrap();
        let mut out = vec![];
        for r in std::mem::take(&mut p.0) {
            match r {
                Reply::Bytes(b) => out.extend(b),
                Reply::Colour(i, f) => out.extend(f(colour(i)).into_bytes()),
            }
        }
        (out, p.1.take())
    }
}

impl EventListener for Replies {
    fn send_event(&self, e: Event) {
        let mut p = self.pending.lock().unwrap();
        match e {
            Event::PtyWrite(s) => p.0.push(Reply::Bytes(s.into_bytes())),
            Event::ColorRequest(i, f) => p.0.push(Reply::Colour(i, f)),
            Event::Title(t) => p.1 = Some(t),
            _ => {}
        }
    }
}

/// How a cell is marked over its colours: selected, or found by Find, the
/// match Find is on drawn apart from the others.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mark {
    #[default]
    None,
    Selected,
    Match,
    Current,
}

/// What a drag selects: cells, whole words, or whole lines, as a click,
/// a double click and a triple click start it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Select {
    Cells,
    Words,
    Lines,
}

/// A run of cells drawn alike, `len` bytes of its line's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub len: usize,
    pub fg: Rgb,
    pub bg: Rgb,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub mark: Mark,
}

/// One row of the screen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub spans: Vec<Span>,
}

pub struct Screen<L: EventListener = Quiet> {
    term: Term<L>,
    parser: Processor,
    /// The colours text and background take when the shell names none.
    fg: Rgb,
    bg: Rgb,
    /// What Find looks for, and the match it is on.
    search: Option<(String, RegexSearch)>,
    found: Option<Match>,
    /// The link the pointer is on, underlined.
    hover: Option<Match>,
    /// The patterns links are found by, made once: the pointer asks at
    /// every move.
    links: Option<(RegexSearch, RegexSearch)>,
}

/// A link in the terminal's text, and the cells it takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub kind: LinkKind,
    range: Match,
}

/// What a link opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkKind {
    /// An address in the text.
    Url(String),
    /// A file and a place in it, as compilers and test runners write them
    /// (src/store/redis.rs:31:9): its path as written, its line and column
    /// from 1, or 0 when not given.
    Path {
        path: String,
        line: u32,
        column: u32,
    },
    /// A link a program wrote around text (OSC 8): where it goes need not
    /// be what it shows.
    Embedded { uri: String, text: String },
}

impl Link {
    /// Whether two links take the same cells.
    pub fn same_place(&self, other: &Link) -> bool {
        self.range == other.range
    }
}

impl Screen<Quiet> {
    pub fn new(columns: usize, lines: usize) -> Self {
        Self::with_listener(columns, lines, Quiet)
    }
}

impl<L: EventListener> Screen<L> {
    pub fn with_listener(columns: usize, lines: usize, listener: L) -> Self {
        Screen {
            // The Kitty keyboard protocol, for programs that ask for it,
            // so they can tell Shift-Enter from Enter.
            term: Term::new(
                Config {
                    kitty_keyboard: true,
                    ..Config::default()
                },
                &Size { columns, lines },
                listener,
            ),
            parser: Processor::new(),
            fg: named(NamedColor::Foreground),
            bg: named(NamedColor::Background),
            search: None,
            found: None,
            hover: None,
            links: None,
        }
    }

    /// Underlines the link given, the one the pointer is on.
    pub fn set_hover(&mut self, link: Option<&Link>) {
        self.hover = link.map(|l| l.range.clone());
    }

    /// Sets the colours text and background take when the shell names none;
    /// on a light background the sixteen named colours are a light theme's,
    /// so yellow and white stay readable.
    pub fn set_default_colours(&mut self, fg: Rgb, bg: Rgb) {
        self.fg = fg;
        self.bg = bg;
    }

    /// Whether the background is light, and the named colours a light
    /// theme's.
    fn light(&self) -> bool {
        let Rgb { r, g, b } = self.bg;
        (r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000 > 128
    }

    /// Scrolls the view through the scrollback by lines, up for positive.
    pub fn scroll(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
    }

    /// How far the view is scrolled back from the bottom, in lines.
    pub fn scrolled_back(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// Whether the program in the terminal has asked for pastes to be
    /// bracketed.
    pub fn bracketed_paste(&self) -> bool {
        self.term.mode().contains(TermMode::BRACKETED_PASTE)
    }

    /// What the program has asked of the keyboard; alt_is_meta is the
    /// platform's choice.
    pub fn key_modes(&self, alt_is_meta: bool) -> crate::termkeys::KeyModes {
        let m = self.term.mode();
        crate::termkeys::KeyModes {
            app_cursor: m.contains(TermMode::APP_CURSOR),
            disambiguate: m.contains(TermMode::DISAMBIGUATE_ESC_CODES),
            all_as_escapes: m.contains(TermMode::REPORT_ALL_KEYS_AS_ESC),
            alt_is_meta,
        }
    }

    /// What the program has asked of the mouse.
    pub fn mouse_modes(&self) -> crate::termkeys::MouseModes {
        let m = self.term.mode();
        crate::termkeys::MouseModes {
            clicks: m.contains(TermMode::MOUSE_REPORT_CLICK),
            drags: m.contains(TermMode::MOUSE_DRAG),
            motion: m.contains(TermMode::MOUSE_MOTION),
            sgr: m.contains(TermMode::SGR_MOUSE),
        }
    }

    /// Whether the wheel moves a full-screen program's cursor instead of
    /// the scrollback: on its own screen, as it asks (1007).
    pub fn wheel_is_arrows(&self) -> bool {
        self.term
            .mode()
            .contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL)
    }

    /// Takes what the shell wrote.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    pub fn resize(&mut self, columns: usize, lines: usize) {
        self.term.resize(Size { columns, lines });
    }

    /// A row and column of the screen as a place in the grid, the view's
    /// scroll taken into account.
    fn point(&self, row: usize, col: usize) -> Point {
        let grid = self.term.grid();
        let row = row.min(grid.screen_lines().saturating_sub(1)) as i32;
        let col = col.min(grid.columns().saturating_sub(1));
        Point::new(GridLine(row - grid.display_offset() as i32), Column(col))
    }

    /// Starts a selection at a cell of the screen; right_half is whether
    /// the pointer is on the cell's right half, so a drag takes the cell or
    /// leaves it as a text editor does.
    pub fn select(&mut self, row: usize, col: usize, right_half: bool, how: Select) {
        let ty = match how {
            Select::Cells => SelectionType::Simple,
            Select::Words => SelectionType::Semantic,
            Select::Lines => SelectionType::Lines,
        };
        let side = if right_half { Side::Right } else { Side::Left };
        self.term.selection = Some(Selection::new(ty, self.point(row, col), side));
    }

    /// Takes the selection to a cell, as a drag or a Shift-click does.
    pub fn select_to(&mut self, row: usize, col: usize, right_half: bool) {
        let point = self.point(row, col);
        let side = if right_half { Side::Right } else { Side::Left };
        if let Some(s) = &mut self.term.selection {
            s.update(point, side);
        }
    }

    pub fn clear_selection(&mut self) {
        self.term.selection = None;
    }

    /// Selects everything: the scrollback and the screen, down to its last
    /// line with text or the cursor's, whichever is lower.
    pub fn select_all(&mut self) {
        let grid = self.term.grid();
        let mut last = self.term.bottommost_line();
        while last > grid.cursor.point.line && grid[last].is_clear() {
            last -= 1;
        }
        let start = Point::new(self.term.topmost_line(), Column(0));
        let end = Point::new(last, self.term.last_column());
        let mut s = Selection::new(SelectionType::Simple, start, Side::Left);
        s.update(end, Side::Right);
        self.term.selection = Some(s);
    }

    /// Clears the scrollback and the screen above the cursor, as IntelliJ's
    /// Clear Buffer does: the cursor's line, the prompt, goes to the top.
    /// A full-screen program's own screen is left as it is.
    pub fn clear_buffer(&mut self) {
        use alacritty_terminal::vte::ansi::{ClearMode, Handler};
        self.term.selection = None;
        self.found = None;
        if !self.term.mode().contains(TermMode::ALT_SCREEN) {
            self.term.scroll_display(Scroll::Bottom);
            let row = self.term.grid().cursor.point.line.0;
            if row > 0 {
                self.term.scroll_up(row as usize);
                self.term.goto_line(0);
            }
        }
        self.term.clear_screen(ClearMode::Saved);
    }

    /// The selected text, lines joined as the shell wrote them; None with
    /// nothing selected.
    pub fn selection(&self) -> Option<String> {
        self.term.selection_to_string().filter(|s| !s.is_empty())
    }

    /// Finds text in the screen and its scrollback, any case unless it has
    /// a capital, and scrolls to it. A new query finds its last match, the
    /// newest; asked again, up goes to the one before, older, and down to
    /// the one after, wrapping round. False when it is nowhere; an empty
    /// query ends finding.
    pub fn find(&mut self, query: &str, up: bool) -> bool {
        if query.is_empty() {
            self.search = None;
            self.found = None;
            return false;
        }
        let fresh = self.search.as_ref().is_none_or(|(q, _)| q != query);
        if fresh {
            let Ok(regex) = RegexSearch::new(&literal(query)) else {
                return false;
            };
            self.search = Some((query.to_string(), regex));
            self.found = None;
        }
        let grid = self.term.grid();
        let last = Point::new(
            GridLine(grid.screen_lines() as i32 - 1),
            Column(grid.columns() - 1),
        );
        let (origin, direction) = match (&self.found, up) {
            (None, _) => (last, Direction::Left),
            (Some(m), true) => (*m.start(), Direction::Left),
            (Some(m), false) => (*m.end(), Direction::Right),
        };
        let Some((_, regex)) = self.search.as_mut() else {
            return false;
        };
        // From a match, the search starts beside it, not on it.
        let origin = match (&self.found, direction) {
            (Some(_), Direction::Left) => {
                origin.sub(&self.term, alacritty_terminal::index::Boundary::None, 1)
            }
            (Some(_), Direction::Right) => {
                origin.add(&self.term, alacritty_terminal::index::Boundary::None, 1)
            }
            _ => origin,
        };
        let found = self
            .term
            .search_next(regex, origin, direction, Side::Left, None);
        if let Some(m) = &found {
            self.term.scroll_to_point(*m.start());
        }
        self.found = found;
        self.found.is_some()
    }

    /// The link on a cell: one a program wrote around the text (OSC 8),
    /// else an address, else a file with a line, as the text says it.
    pub fn link_at(&mut self, row: usize, col: usize) -> Option<Link> {
        let point = self.point(row, col);
        if let Some(link) = self.term.grid()[point].hyperlink()
            && !link.uri().starts_with(ANCHOR)
        {
            let range = self.hyperlink_range(point);
            let text = self
                .term
                .bounds_to_string(*range.start(), *range.end())
                .replace('\n', "");
            return Some(Link {
                kind: LinkKind::Embedded {
                    uri: link.uri().to_string(),
                    text,
                },
                range,
            });
        }
        if self.links.is_none() {
            self.links = Some((RegexSearch::new(URL).ok()?, RegexSearch::new(PATH).ok()?));
        }
        let (url, path) = self.links.as_mut()?;
        if let Some((range, text)) = match_at(&self.term, url, point) {
            return Some(Link {
                kind: LinkKind::Url(text),
                range,
            });
        }
        let (range, text) = match_at(&self.term, path, point)?;
        let (path, line, column) = path_and_place(&text)?;
        Some(Link {
            kind: LinkKind::Path { path, line, column },
            range,
        })
    }

    /// The cells around a point that carry the same link it does.
    fn hyperlink_range(&self, point: Point) -> Match {
        use alacritty_terminal::index::Boundary;
        let grid = self.term.grid();
        let link = grid[point].hyperlink();
        let (mut start, mut end) = (point, point);
        for _ in 0..4096 {
            let before = start.sub(&self.term, Boundary::Grid, 1);
            if before == start || grid[before].hyperlink() != link {
                break;
            }
            start = before;
        }
        for _ in 0..4096 {
            let after = end.add(&self.term, Boundary::Grid, 1);
            if after == end || grid[after].hyperlink() != link {
                break;
            }
            end = after;
        }
        start..=end
    }

    /// The cells on the screen, and in the lines above it given, that
    /// carry a link named with prefix: each's row on the screen (above it
    /// negative), column and the rest of its name. A terminal's pictures
    /// are anchored so.
    pub fn anchors(&self, prefix: &str, above: usize) -> Vec<(i32, usize, String)> {
        let grid = self.term.grid();
        let top = -(grid.display_offset() as i32);
        let oldest = -(grid.history_size() as i32);
        let first = (top - above as i32).max(oldest);
        let mut out = vec![];
        for line in first..top + grid.screen_lines() as i32 {
            for col in 0..grid.columns() {
                let cell = &grid[Point::new(GridLine(line), Column(col))];
                if let Some(rest) = cell
                    .hyperlink()
                    .and_then(|h| h.uri().strip_prefix(prefix).map(str::to_string))
                {
                    out.push((line - top, col, rest));
                }
            }
        }
        out
    }

    /// Whether Find is on a match.
    pub fn found(&self) -> bool {
        self.found.is_some()
    }

    /// The cursor's row and column on the screen.
    pub fn cursor(&self) -> (usize, usize) {
        let p = self.term.grid().cursor.point;
        (p.line.0.max(0) as usize, p.column.0)
    }

    /// Whether the program shows the cursor: one that draws its own, as
    /// Claude Code's prompt does, hides it.
    pub fn cursor_shown(&self) -> bool {
        self.term.mode().contains(TermMode::SHOW_CURSOR)
    }

    /// A colour by the index a program asks after it (OSC 4's 0 to 255,
    /// then the foreground, background and cursor, OSC 10 to 12): the one
    /// the program set, else the screen's own. A program learns from the
    /// background whether the theme is light or dark.
    pub fn colour(&self, index: usize) -> Rgb {
        if let Some(c) = self.term.colors()[index] {
            return c;
        }
        match index {
            i if i < 256 => indexed_in(i as u8, self.light()),
            i if i == NamedColor::Background as usize => self.bg,
            _ => self.fg,
        }
    }

    /// The visible rows, each a line of text with its spans, the selection
    /// and Find's matches marked.
    pub fn lines(&mut self) -> Vec<Line> {
        let selected = self
            .term
            .selection
            .as_ref()
            .and_then(|s| s.to_range(&self.term));
        let grid = self.term.grid();
        let columns = grid.columns();
        let offset = grid.display_offset() as i32;
        let (top, bottom) = (-offset, grid.screen_lines() as i32 - 1 - offset);
        let matches: Vec<Match> = match self.search.as_mut() {
            Some((_, regex)) => RegexIter::new(
                Point::new(GridLine(top), Column(0)),
                Point::new(GridLine(bottom), Column(columns - 1)),
                Direction::Right,
                &self.term,
                regex,
            )
            .collect(),
            None => vec![],
        };
        let grid = self.term.grid();
        let found = self.found.clone();
        let hover = self.hover.clone();
        (0..grid.screen_lines() as i32)
            .map(|row| {
                let line_at = GridLine(row - offset);
                let row = &grid[line_at];
                let mut line = Line::default();
                for col in 0..columns {
                    let cell = &row[Column(col)];
                    if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                        continue;
                    }
                    let (mut fg, mut bg) = (self.rgb(cell.fg), self.rgb(cell.bg));
                    if cell.flags.contains(Flags::INVERSE) {
                        std::mem::swap(&mut fg, &mut bg);
                    }
                    let point = Point::new(line_at, Column(col));
                    let mark = if found.as_ref().is_some_and(|m| m.contains(&point)) {
                        Mark::Current
                    } else if selected.as_ref().is_some_and(|s| s.contains(point)) {
                        Mark::Selected
                    } else if matches.iter().any(|m| m.contains(&point)) {
                        Mark::Match
                    } else {
                        Mark::None
                    };
                    let span = Span {
                        len: 0,
                        fg,
                        bg,
                        bold: cell.flags.contains(Flags::BOLD),
                        italic: cell.flags.contains(Flags::ITALIC),
                        underline: cell.flags.intersects(Flags::ALL_UNDERLINES)
                            || hover.as_ref().is_some_and(|h| h.contains(&point)),
                        mark,
                    };
                    let start = line.text.len();
                    line.text.push(cell.c);
                    let len = line.text.len() - start;
                    match line.spans.last_mut() {
                        Some(last)
                            if Span {
                                len: last.len,
                                ..span
                            } == *last =>
                        {
                            last.len += len
                        }
                        _ => line.spans.push(Span { len, ..span }),
                    }
                }
                line
            })
            .collect()
    }

    /// A colour as the screen shows it: the shell's own choice of palette
    /// entry, else the default palette.
    fn rgb(&self, c: Color) -> Rgb {
        match c {
            Color::Spec(rgb) => rgb,
            Color::Named(n) => self.term.colors()[n].unwrap_or(match n {
                NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::Cursor => {
                    self.fg
                }
                NamedColor::Background => self.bg,
                n => named_in(n, self.light()),
            }),
            Color::Indexed(i) => {
                self.term.colors()[i as usize].unwrap_or_else(|| indexed_in(i, self.light()))
            }
        }
    }
}

/// A URL in a terminal's text: a scheme a browser or the system opens,
/// then anything up to a space, a quote or an angle bracket.
const URL: &str = r#"(https?|file|ftp)://[^\s<>"'`\x00-\x1f\x7f]+"#;

/// The text a pattern matches across a point, from the line above it to
/// the line below: an address wraps.
fn match_at<L: EventListener>(
    term: &Term<L>,
    regex: &mut RegexSearch,
    point: Point,
) -> Option<(Match, String)> {
    use alacritty_terminal::index::Boundary;
    let columns = term.grid().columns();
    let start = Point::new(point.line - 1, Column(0)).grid_clamp(term, Boundary::Grid);
    let end = Point::new(point.line + 1, Column(columns - 1)).grid_clamp(term, Boundary::Grid);
    let found =
        RegexIter::new(start, end, Direction::Right, term, regex).find(|m| m.contains(&point))?;
    let text = term
        .bounds_to_string(*found.start(), *found.end())
        .replace('\n', "");
    Some((found, text))
}

/// A file and a place in it, as compilers, linters and test runners write
/// them: a path ending in a name with an extension, then a line, and maybe
/// a column (src/store/redis.rs:31:9, ./main.go:12, /srv/app/x.ts:3:1).
const PATH: &str = r"(\.{1,2}/|/)?([A-Za-z0-9_@+-][A-Za-z0-9_.@+-]*/)*[A-Za-z0-9_@+-][A-Za-z0-9_.@+-]*\.[A-Za-z0-9]+:[0-9]+(:[0-9]+)?";

/// A path match's path, line and column (0 when not given).
fn path_and_place(text: &str) -> Option<(String, u32, u32)> {
    let mut parts = text.rsplitn(3, ':');
    let last = parts.next()?;
    let middle = parts.next()?;
    match parts.next() {
        Some(path) => Some((path.to_string(), middle.parse().ok()?, last.parse().ok()?)),
        None => Some((middle.to_string(), last.parse().ok()?, 0)),
    }
}

/// Text as a pattern that matches it and nothing else.
fn literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn named(n: NamedColor) -> Rgb {
    named_in(n, false)
}

/// A named colour, from a light theme's sixteen or a dark one's.
fn named_in(n: NamedColor, light: bool) -> Rgb {
    match n {
        NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::Cursor => Rgb {
            r: 0xd4,
            g: 0xd4,
            b: 0xd4,
        },
        NamedColor::Background => Rgb {
            r: 0x1e,
            g: 0x1e,
            b: 0x1e,
        },
        NamedColor::DimForeground => Rgb {
            r: 0x9a,
            g: 0x9a,
            b: 0x9a,
        },
        n if (n as usize) < 16 => indexed_in(n as u8, light),
        n => indexed_in((n as usize - NamedColor::DimBlack as usize) as u8, light),
    }
}

/// xterm's 256 colours: 16 named, from a light theme's or a dark one's
/// (VS Code's Light+ and Dark+), a 6x6x6 cube, then 24 greys.
fn indexed_in(i: u8, light: bool) -> Rgb {
    const LIGHT: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00),
        (0xcd, 0x31, 0x31),
        (0x00, 0xbc, 0x00),
        (0x94, 0x98, 0x00),
        (0x04, 0x51, 0xa5),
        (0xbc, 0x05, 0xbc),
        (0x05, 0x98, 0xbc),
        (0x55, 0x55, 0x55),
        (0x66, 0x66, 0x66),
        (0xcd, 0x31, 0x31),
        (0x14, 0xce, 0x14),
        (0xb5, 0xba, 0x00),
        (0x04, 0x51, 0xa5),
        (0xbc, 0x05, 0xbc),
        (0x05, 0x98, 0xbc),
        (0xa5, 0xa5, 0xa5),
    ];
    const ANSI: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00),
        (0xcd, 0x31, 0x31),
        (0x0d, 0xbc, 0x79),
        (0xe5, 0xe5, 0x10),
        (0x24, 0x72, 0xc8),
        (0xbc, 0x3f, 0xbc),
        (0x11, 0xa8, 0xcd),
        (0xe5, 0xe5, 0xe5),
        (0x66, 0x66, 0x66),
        (0xf1, 0x4c, 0x4c),
        (0x23, 0xd1, 0x8b),
        (0xf5, 0xf5, 0x43),
        (0x3b, 0x8e, 0xea),
        (0xd6, 0x70, 0xd6),
        (0x29, 0xb8, 0xdb),
        (0xe5, 0xe5, 0xe5),
    ];
    match i {
        0..16 => {
            let (r, g, b) = if light { LIGHT } else { ANSI }[i as usize];
            Rgb { r, g, b }
        }
        16..232 => {
            let i = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Rgb {
                r: level(i / 36),
                g: level(i / 6 % 6),
                b: level(i % 6),
            }
        }
        _ => {
            let v = 8 + (i - 232) * 10;
            Rgb { r: v, g: v, b: v }
        }
    }
}
