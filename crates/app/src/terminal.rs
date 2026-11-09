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
    pending: std::sync::Arc<std::sync::Mutex<(Vec<u8>, Option<String>)>>,
}

impl Replies {
    /// The replies since this was last called, and the title, if it was
    /// set meanwhile.
    pub fn take(&self) -> (Vec<u8>, Option<String>) {
        let mut p = self.pending.lock().unwrap();
        (std::mem::take(&mut p.0), p.1.take())
    }
}

impl EventListener for Replies {
    fn send_event(&self, e: Event) {
        let mut p = self.pending.lock().unwrap();
        match e {
            Event::PtyWrite(s) => p.0.extend_from_slice(s.as_bytes()),
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
}

impl Screen<Quiet> {
    pub fn new(columns: usize, lines: usize) -> Self {
        Self::with_listener(columns, lines, Quiet)
    }
}

impl<L: EventListener> Screen<L> {
    pub fn with_listener(columns: usize, lines: usize, listener: L) -> Self {
        Screen {
            term: Term::new(Config::default(), &Size { columns, lines }, listener),
            parser: Processor::new(),
            fg: named(NamedColor::Foreground),
            bg: named(NamedColor::Background),
            search: None,
            found: None,
        }
    }

    /// Sets the colours text and background take when the shell names none.
    pub fn set_default_colours(&mut self, fg: Rgb, bg: Rgb) {
        self.fg = fg;
        self.bg = bg;
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

    /// Whether the program has asked for the cursor keys' application
    /// sequences (ESC O A rather than ESC [ A).
    pub fn app_cursor(&self) -> bool {
        self.term.mode().contains(TermMode::APP_CURSOR)
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

    /// Whether Find is on a match.
    pub fn found(&self) -> bool {
        self.found.is_some()
    }

    /// The cursor's row and column on the screen.
    pub fn cursor(&self) -> (usize, usize) {
        let p = self.term.grid().cursor.point;
        (p.line.0.max(0) as usize, p.column.0)
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
                        underline: cell.flags.intersects(Flags::ALL_UNDERLINES),
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
                n => named(n),
            }),
            Color::Indexed(i) => self.term.colors()[i as usize].unwrap_or_else(|| indexed(i)),
        }
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
        n if (n as usize) < 16 => indexed(n as u8),
        n => indexed((n as usize - NamedColor::DimBlack as usize) as u8),
    }
}

/// xterm's 256 colours: 16 named, a 6x6x6 cube, then 24 greys.
fn indexed(i: u8) -> Rgb {
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
            let (r, g, b) = ANSI[i as usize];
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
