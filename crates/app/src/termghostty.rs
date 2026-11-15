//! Ghostty's terminal emulation (hangar-ghostty, libghostty-vt) as a
//! terminal's screen: the Emulator a terminal uses when the settings choose
//! Ghostty. A session it starts is told it runs in Ghostty, and it answers
//! XTVERSION so, since what keeps its screen is Ghostty's own emulation.
//!
//! It keeps kitty graphics itself, as Ghostty does: a program's pictures are
//! stored and placed by Ghostty's emulation, up to PICTURE_STORAGE bytes a
//! screen, and the view draws the placements it reads. Pictures come only
//! in the commands themselves: the file, temporary file and shared memory
//! media stay off, since the program runs on another machine and those
//! would be this one's.

use std::time::{Duration, Instant};

use alacritty_terminal::vte::ansi::Rgb;
use hangar_ghostty::{Mouse, Picture, Placement, Terminal, Wide};
use regex::Regex;

use crate::terminal::{
    ANCHOR, Cells, CursorLook, Effects, Emulator, Line, Link, LinkKind, Mark, PATH, Select, Span,
    URL, Underline, faded, indexed_in, path_and_place,
};

/// A point of Ghostty's whole screen, scrollback and all: (row, column).
type At = (usize, usize);

/// The character a program writes where a kitty picture's virtual
/// placement is to show (kitty's Unicode placeholders).
const PLACEHOLDER: char = '\u{10EEEE}';

/// Placements with the strips of one picture that meet joined into one.
/// Ghostty gives a picture shown with placeholders a row of cells at a
/// time, and drawn apart the strips show a seam where each meets the next.
/// A strip joins the one above it when it is on the next row, from that
/// row's top, over the same cells, and shows the image's next rows.
fn joined(strips: Vec<Placement>) -> Vec<Placement> {
    // Each placement, and the last row it covers.
    let mut out: Vec<(Placement, i32)> = Vec::with_capacity(strips.len());
    'strips: for p in strips {
        // Placements come ordered by layer and z: one that meets p is
        // among the last of p's layer and z.
        for (q, last) in out.iter_mut().rev() {
            if (q.layer, q.z) != (p.layer, p.z) {
                break;
            }
            let (qx, qy, qw, qh) = q.source;
            let (px, py, pw, ph) = p.source;
            if q.strip
                && p.strip
                && (q.image, q.generation) == (p.image, p.generation)
                && (q.col, q.offset.0, q.width) == (p.col, p.offset.0, p.width)
                && p.row == *last + 1
                && p.offset.1 == 0
                && (qx, qw) == (px, pw)
                && qy + qh == py
            {
                q.height += p.height;
                q.source.3 += ph;
                *last = p.row;
                continue 'strips;
            }
        }
        let row = p.row;
        out.push((p, row));
    }
    out.into_iter().map(|(p, _)| p).collect()
}

pub struct Ghostty {
    t: Terminal,
    columns: usize,
    rows: usize,
    /// The link the pointer is on, underlined.
    hover: Option<(At, At)>,
    /// Where a selection started: a cell and the side of it the pointer was
    /// on, or the word or line first selected, which a drag extends by
    /// words or lines.
    anchor: Option<Anchor>,
    /// The patterns links are found by, made once.
    links: Option<(Regex, Regex)>,
    /// The pictures on the lines last read, and how the cursor is drawn.
    placed: Vec<Placement>,
    cursor_look: CursorLook,
    /// What animations' frames are timed from.
    started: Instant,
}

/// How many bytes of pictures a screen keeps, the oldest deleted past it.
const PICTURE_STORAGE: u64 = 64 << 20;

/// How many bytes of rows the scrollback keeps: Ghostty's own default, many
/// thousands of lines.
const SCROLLBACK: usize = 50_000_000;

#[derive(Debug, Clone, Copy)]
enum Anchor {
    Cell(At, bool),
    Unit(At, At, Select),
    /// A corner of a rectangle, and the side of its cell pressed.
    Block(At, bool),
}

/// The version of Ghostty this emulation is.
fn version() -> String {
    hangar_ghostty::ghostty_version()
}

impl Ghostty {
    pub fn new(columns: usize, rows: usize) -> Ghostty {
        let mut t = Terminal::new(
            columns as u16,
            rows as u16,
            &format!("ghostty {}", version()),
        )
        .expect("making a Ghostty terminal");
        t.set_kitty_image_limit(PICTURE_STORAGE);
        t.set_scrollback(Some(SCROLLBACK));
        Ghostty {
            t,
            columns,
            rows,
            hover: None,
            anchor: None,
            links: None,
            placed: vec![],
            cursor_look: CursorLook::default(),
            started: Instant::now(),
        }
    }

    /// The cell after a point of the screen, the next row's first after a
    /// row's last.
    fn next(&self, (r, c): At) -> At {
        if c + 1 < self.columns {
            (r, c + 1)
        } else {
            (r + 1, 0)
        }
    }

    /// The cell before a point, None before the screen's first.
    fn prev(&self, (r, c): At) -> Option<At> {
        match (r, c) {
            (0, 0) => None,
            (r, 0) => Some((r - 1, self.columns.saturating_sub(1))),
            (r, c) => Some((r, c - 1)),
        }
    }

    /// A word or a line at a point of the screen, as a selection by it
    /// takes.
    fn unit(&self, at: At, how: Select) -> (At, At) {
        let found = match how {
            Select::Words => self.t.word_at(at),
            Select::Lines => self.t.line_at(at),
            Select::Cells | Select::Block => None,
        };
        found.unwrap_or((at, at))
    }

    /// The text of the rows of the viewport from first to last, joined,
    /// and each character's place in the whole screen.
    fn text_of(&mut self, first: usize, last: usize) -> (String, Vec<At>) {
        let frame = self.t.frame();
        let top = self.t.above_viewport();
        let mut text = String::new();
        let mut places = vec![];
        for (r, row) in frame.rows.iter().enumerate().take(last + 1).skip(first) {
            for (c, cell) in row.cells.iter().enumerate() {
                if cell.wide == Wide::SpacerTail {
                    continue;
                }
                let s = if cell.text.is_empty() {
                    " "
                } else {
                    cell.text.as_str()
                };
                for ch in s.chars() {
                    text.push(ch);
                    places.push((top + r, c));
                }
            }
        }
        (text, places)
    }

    /// The link a program wrote around the text (OSC 8) at a point of the
    /// viewport, with the cells of that row it takes.
    fn embedded_at(&mut self, row: usize, col: usize) -> Option<Link> {
        let uri = self.t.hyperlink_at(row, col)?;
        if uri.starts_with(ANCHOR) {
            return None;
        }
        // The cells the link covers, on into the rows it wraps onto.
        let same =
            |(r, c): (usize, usize)| self.t.hyperlink_at(r, c).as_deref() == Some(uri.as_str());
        let before = |(r, c): (usize, usize)| {
            if c > 0 {
                Some((r, c - 1))
            } else if r > 0 && self.t.wraps(r - 1) {
                Some((r - 1, self.columns - 1))
            } else {
                None
            }
        };
        let after = |(r, c): (usize, usize)| {
            if c + 1 < self.columns {
                Some((r, c + 1))
            } else if r + 1 < self.rows && self.t.wraps(r) {
                Some((r + 1, 0))
            } else {
                None
            }
        };
        let mut start = (row, col);
        while let Some(p) = before(start).filter(|&p| same(p)) {
            start = p;
        }
        let mut end = (row, col);
        while let Some(p) = after(end).filter(|&p| same(p)) {
            end = p;
        }
        let (line, places) = self.text_of(start.0, end.0);
        let top = self.t.above_viewport();
        let range = ((top + start.0, start.1), (top + end.0, end.1));
        let text: String = line
            .chars()
            .zip(places)
            .filter(|&(_, at)| within(at, range))
            .map(|(ch, _)| ch)
            .collect();
        Some(Link {
            kind: LinkKind::Embedded {
                uri,
                text: text.trim_end().to_string(),
            },
            range: Cells::Screen(range.0, range.1),
        })
    }
}

/// Whether a point is within a run of cells, both ends included.
fn within(at: At, (a, b): (At, At)) -> bool {
    a <= at && at <= b
}

impl Emulator for Ghostty {
    fn feed(&mut self, bytes: &[u8]) {
        self.t.feed(bytes)
    }

    fn resize(&mut self, columns: usize, lines: usize, (w, h): (u32, u32)) {
        (self.columns, self.rows) = (columns.max(1), lines.max(1));
        self.t.resize(columns as u16, lines as u16, w, h)
    }

    /// On a light background the sixteen named colours are a light
    /// theme's, so yellow and white stay readable, as alacritty's screen
    /// has them.
    fn set_default_colours(&mut self, fg: Rgb, bg: Rgb) {
        let light = crate::terminal::is_light(bg);
        let palette: [hangar_ghostty::Rgb; 256] =
            std::array::from_fn(|i| ours(indexed_in(i as u8, light)));
        self.t.set_colours(ours(fg), ours(bg), &palette);
        self.t.set_dark(!light);
    }

    fn set_hover(&mut self, link: Option<&Link>) {
        self.hover = link.and_then(|l| match l.range {
            Cells::Screen(a, b) => Some((a, b)),
            Cells::Grid(_) => None,
        });
    }

    fn scroll(&mut self, lines: i32) {
        self.t.scroll(lines as isize)
    }

    fn scrolled_back(&self) -> usize {
        self.t.scrolled_back()
    }

    fn bracketed_paste(&self) -> bool {
        self.t.mode(2004, false)
    }

    fn key_modes(&self, alt_is_meta: bool) -> crate::termkeys::KeyModes {
        let kitty = self.t.kitty_keyboard();
        crate::termkeys::KeyModes {
            app_cursor: self.t.mode(1, false),
            disambiguate: kitty & 1 != 0,
            all_as_escapes: kitty & 8 != 0,
            alt_is_meta,
        }
    }

    fn mouse_modes(&self) -> crate::termkeys::MouseModes {
        let m = self.t.mouse();
        crate::termkeys::MouseModes {
            clicks: m != Mouse::None,
            drags: matches!(m, Mouse::Button | Mouse::Any),
            motion: m == Mouse::Any,
            sgr: self.t.mode(1006, false),
            pixels: self.t.mode(1016, false),
        }
    }

    fn wheel_is_arrows(&self) -> bool {
        self.t.alternate_screen() && self.t.mode(1007, false)
    }

    /// A press starts a selection: by cells it selects nothing until the
    /// pointer moves; by words or lines, the word or line it is on.
    fn select(&mut self, row: usize, col: usize, right_half: bool, how: Select) {
        let Some(at) = self.t.screen_point(row, col) else {
            return;
        };
        match how {
            Select::Cells => {
                self.t.clear_selection();
                self.anchor = Some(Anchor::Cell(at, right_half));
            }
            Select::Block => {
                self.t.clear_selection();
                self.anchor = Some(Anchor::Block(at, right_half));
            }
            how => {
                let (a, b) = self.unit(at, how);
                self.t.select(a, b);
                self.anchor = Some(Anchor::Unit(a, b, how));
            }
        }
    }

    /// Takes the selection to a cell: by cells, the cells between the
    /// press and here, a cell counted when the pointer is past its middle,
    /// as a text editor does; by words or lines, through the word or line
    /// here.
    fn select_to(&mut self, row: usize, col: usize, right_half: bool) {
        let Some(at) = self.t.screen_point(row, col) else {
            return;
        };
        match self.anchor {
            Some(Anchor::Cell(from, from_right)) => {
                // Boundaries between cells: before one, or after it.
                let boundary = |p: At, right: bool| if right { self.next(p) } else { p };
                let (a, b) = (boundary(from, from_right), boundary(at, right_half));
                let (start, end) = if a <= b { (a, b) } else { (b, a) };
                match self.prev(end).filter(|last| start <= *last) {
                    Some(last) => self.t.select(start, last),
                    None => self.t.clear_selection(),
                }
            }
            Some(Anchor::Unit(a, b, how)) => {
                let (c, d) = self.unit(at, how);
                self.t.select(a.min(c), b.max(d));
            }
            Some(Anchor::Block((row, col), right)) => {
                // The columns between the boundaries the press and the
                // pointer are at, as a drag by cells counts them.
                let (a, b) = (col + right as usize, at.1 + right_half as usize);
                let (left, past) = (a.min(b), a.max(b));
                if past == left {
                    self.t.clear_selection();
                } else {
                    let (top, bottom) = (row.min(at.0), row.max(at.0));
                    self.t.select_rectangle((top, left), (bottom, past - 1));
                }
            }
            None => {}
        }
    }

    fn clear_selection(&mut self) {
        self.anchor = None;
        self.t.clear_selection();
    }

    fn select_all(&mut self) {
        self.anchor = None;
        self.t.select_all();
    }

    fn clear_buffer(&mut self) {
        self.anchor = None;
        self.t.clear_buffer();
    }

    fn selection(&self) -> Option<String> {
        self.t.selection()
    }

    /// Finds text, ASCII letters in any case, as Ghostty does.
    fn find(&mut self, query: &str, up: bool) -> bool {
        self.t.find(query, up)
    }

    /// The link at a cell: one a program wrote around the text (OSC 8),
    /// else an address, else a file with a line, as the text says it,
    /// across the rows a long line wraps over.
    fn link_at(&mut self, row: usize, col: usize) -> Option<Link> {
        if let Some(link) = self.embedded_at(row, col) {
            return Some(link);
        }
        let mut first = row;
        while first > 0 && self.t.wraps(first - 1) {
            first -= 1;
        }
        let mut last = row;
        while last + 1 < self.rows && self.t.wraps(last) {
            last += 1;
        }
        let (text, places) = self.text_of(first, last);
        let top = self.t.above_viewport();
        let here = (top + row, col);
        if self.links.is_none() {
            self.links = Some((Regex::new(URL).ok()?, Regex::new(PATH).ok()?));
        }
        let (url, path) = self.links.as_ref()?;
        let char_at = |byte: usize| text[..byte].chars().count();
        let found = |re: &Regex| {
            re.find_iter(&text).find_map(|m| {
                let (s, e) = (char_at(m.start()), char_at(m.end()));
                let range = (places[s], places[e.checked_sub(1)?]);
                within(here, range).then(|| (m.as_str().to_string(), range))
            })
        };
        if let Some((t, (a, b))) = found(url) {
            return Some(Link {
                kind: LinkKind::Url(t),
                range: Cells::Screen(a, b),
            });
        }
        let (t, (a, b)) = found(path)?;
        let (path, line, column) = path_and_place(&t)?;
        Some(Link {
            kind: LinkKind::Path { path, line, column },
            range: Cells::Screen(a, b),
        })
    }

    fn anchors(&self, prefix: &str, above: usize) -> Vec<(i32, usize, String)> {
        self.t.links_named(prefix, above, self.columns, self.rows)
    }

    fn cursor(&self) -> (usize, usize) {
        self.t.cursor().0
    }

    fn cursor_shown(&self) -> bool {
        self.t.cursor().1
    }

    fn cursor_look(&self) -> CursorLook {
        self.cursor_look
    }

    /// The visible rows, each a line of text with its spans, the selection
    /// and Find's matches marked.
    fn lines(&mut self) -> Vec<Line> {
        let mut frame = self.t.frame();
        self.placed = joined(std::mem::take(&mut frame.placements));
        self.cursor_look = CursorLook {
            style: frame.cursor_style,
            blinking: frame.cursor_blinking,
            colour: frame.cursor_colour.map(theirs),
        };
        let found = self.t.found();
        let matches = self.t.matches();
        let top = self.t.above_viewport();
        frame
            .rows
            .iter()
            .enumerate()
            .map(|(r, row)| {
                let mut line = Line::default();
                for (c, cell) in row.cells.iter().enumerate() {
                    if cell.wide == Wide::SpacerTail {
                        let start = line.cells.last().copied().unwrap_or(0);
                        line.cells.push(start);
                        continue;
                    }
                    line.cells.push(line.text.len());
                    let mut fg = theirs(cell.fg.unwrap_or(frame.fg));
                    let mut bg = theirs(cell.bg.unwrap_or(frame.bg));
                    if cell.inverse {
                        std::mem::swap(&mut fg, &mut bg);
                    }
                    if cell.faint {
                        fg = faded(fg, bg);
                    }
                    if cell.invisible {
                        fg = bg;
                    }
                    let mark = if found.is_some_and(|m| within((r, c), m)) {
                        Mark::Current
                    } else if cell.selected {
                        Mark::Selected
                    } else if matches.iter().any(|m| within((r, c), *m)) {
                        Mark::Match
                    } else {
                        Mark::None
                    };
                    let hovered = self.hover.is_some_and(|h| within((top + r, c), h));
                    let span = Span {
                        len: 0,
                        fg,
                        bg,
                        own_bg: cell.inverse || cell.bg.is_some(),
                        bold: cell.bold,
                        italic: cell.italic,
                        underline: if hovered && cell.underline == Underline::None {
                            Underline::Single
                        } else {
                            cell.underline
                        },
                        underline_colour: cell.underline_colour.map(theirs),
                        strikethrough: cell.strikethrough,
                        overline: cell.overline,
                        mark,
                    };
                    // A kitty placeholder cell, with the diacritics saying
                    // which part of a picture it holds, is blank under the
                    // picture Ghostty places there.
                    let start = line.text.len();
                    line.text.push_str(
                        if cell.text.is_empty() || cell.text.starts_with(PLACEHOLDER) {
                            " "
                        } else {
                            &cell.text
                        },
                    );
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

    fn effects(&mut self) -> Effects {
        let e = self.t.take_effects();
        Effects {
            reply: e.reply,
            title: e.title,
            copied: e.copied,
            bell: e.bell,
            reset: e.reset,
            statuses: e.statuses,
            prompt_started: e.prompt_started,
        }
    }

    fn pointer(&self) -> crate::terminal::Pointer {
        self.t.pointer()
    }

    fn scroll_prompts(&mut self, prompts: i32) {
        self.t.scroll_prompts(prompts as isize)
    }

    fn reports_focus(&self) -> bool {
        self.t.mode(1004, false)
    }

    fn reports_theme(&self) -> bool {
        self.t.mode(2031, false)
    }

    fn takes_kitty_graphics(&self) -> bool {
        true
    }

    fn identity(&self) -> Option<(String, String)> {
        Some(("ghostty".into(), version()))
    }

    fn placements(&self) -> Vec<Placement> {
        self.placed.clone()
    }

    fn picture(&self, image: u32) -> Option<Picture> {
        self.t.picture(image)
    }

    fn synchronized(&self) -> bool {
        self.t.mode(2026, false)
    }

    fn tick_animations(&mut self) -> Option<Duration> {
        let now = self.started.elapsed().as_millis() as u64;
        let next = self.t.tick_animations(now)?;
        Some(Duration::from_millis(next.saturating_sub(now)))
    }
}

fn ours(c: Rgb) -> hangar_ghostty::Rgb {
    hangar_ghostty::Rgb {
        r: c.r,
        g: c.g,
        b: c.b,
    }
}

fn theirs(c: hangar_ghostty::Rgb) -> Rgb {
    Rgb {
        r: c.r,
        g: c.g,
        b: c.b,
    }
}
