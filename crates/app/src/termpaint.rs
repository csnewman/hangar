//! Paints a terminal's screen with every character at its own cell, as
//! Ghostty places them. Runs of plain text are shaped together at a cell's
//! width; any other character is shaped alone where its cell starts, so a
//! glyph a fallback font draws wider or narrower than a cell moves nothing
//! after it. Box drawing and blocks are shapes that fill their cells
//! (termsprite). Layers go in Ghostty's order: pictures under the
//! background, the cells' backgrounds, pictures under the text, a block
//! cursor, the text and the lines through it, pictures over the text, and
//! the other cursors.

use std::sync::Arc;

use gpui::{
    App, Bounds, Corners, Font, Hsla, PathBuilder, Pixels, Point, Rgba, TextAlign, TextRun, Window,
    fill, point, px, size,
};
use hangar_ghostty::Layer;

use crate::terminal::{CursorStyle, Line, Mark, Span, Underline};
use crate::termsprite::{self, Shape};
use alacritty_terminal::vte::ansi::Rgb;

/// A screen as it is to be painted.
pub struct Screen {
    pub lines: Vec<Line>,
    pub cell: Pixels,
    pub line_height: Pixels,
    pub font: Font,
    pub font_size: Pixels,
    pub colours: Colours,
    pub cursor: Option<Cursor>,
    pub pictures: Vec<Picture>,
    /// The least contrast text keeps against its background: below it,
    /// text is drawn white or black, whichever stands out more.
    pub min_contrast: f32,
}

/// The colours of what the view marks on the screen, and the text's
/// default for a cursor on an empty cell.
pub struct Colours {
    pub selected: Hsla,
    pub found: Hsla,
    pub current: Hsla,
    /// The text on Find's current match.
    pub mark_text: Hsla,
    pub fg: Hsla,
    /// The cursor's, where the theme gives one and the program none.
    pub cursor: Option<Hsla>,
}

/// The cursor, where it is shown: its row and column, its shape, and its
/// own colour if it has one (else the text's under it).
pub struct Cursor {
    pub row: usize,
    pub col: usize,
    pub style: CursorStyle,
    pub colour: Option<Hsla>,
}

/// A picture, where it is drawn from the screen's top left, in a layer.
pub struct Picture {
    pub layer: Layer,
    pub at: Bounds<Pixels>,
    pub image: Arc<gpui::RenderImage>,
}

pub fn hsla(c: alacritty_terminal::vte::ansi::Rgb) -> Hsla {
    Rgba {
        r: c.r as f32 / 255.,
        g: c.g as f32 / 255.,
        b: c.b as f32 / 255.,
        a: 1.,
    }
    .into()
}

/// Each span's columns, from its first to the one after its last.
fn span_columns(line: &Line) -> Vec<(usize, usize)> {
    let mut out = Vec::with_capacity(line.spans.len());
    let (mut col, mut end) = (0, 0);
    for s in &line.spans {
        end += s.len;
        let start = col;
        while col < line.cells.len() && line.cells[col] < end {
            col += 1;
        }
        out.push((start, col));
    }
    out
}

/// Where lines are drawn in a row, from its top, and how thick.
struct Metrics {
    underline: Pixels,
    strike: Pixels,
    /// A box drawing character's light line, and an underline.
    light: f32,
}

fn metrics(s: &Screen, window: &Window) -> Metrics {
    let ts = window.text_system();
    let id = ts.resolve_font(&s.font);
    let ascent = ts.ascent(id, s.font_size);
    // Below the baseline, which some platforms' metrics count as negative.
    let descent = ts.descent(id, s.font_size).abs();
    let x_height = ts.x_height(id, s.font_size);
    let top = (s.line_height - ascent - descent) / 2.;
    let baseline = top + ascent;
    // A whole number of the screen's pixels, about one a point at 12.
    let scale = window.scale_factor();
    let light = (f32::from(s.font_size) * scale / 12.).round().max(1.) / scale;
    Metrics {
        underline: baseline + descent * 0.618 - px(light / 2.),
        strike: baseline - x_height / 2. - px(light / 2.),
        light,
    }
}

pub fn paint(s: &Screen, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
    crate::renderstats::count(crate::renderstats::TERMINAL_PAINT);
    let origin = bounds.origin;
    let (cw, lh) = (s.cell, s.line_height);
    let at = |row: usize, col: usize| point(origin.x + cw * col as f32, origin.y + lh * row as f32);
    let m = metrics(s, window);
    let columns: Vec<Vec<(usize, usize)>> = s.lines.iter().map(span_columns).collect();
    let pictures = |layer: Layer, window: &mut Window| {
        for p in s.pictures.iter().filter(|p| p.layer == layer) {
            let image = Bounds::new(origin + p.at.origin, p.at.size);
            let _ =
                window.paint_image(bounds, image, Corners::default(), p.image.clone(), 0, false);
        }
    };
    let quad = |window: &mut Window, from: Point<Pixels>, w: Pixels, h: Pixels, c: Hsla| {
        window.paint_quad(fill(Bounds::new(from, size(w, h)), c));
    };
    // The cursor's cell: its span and how many columns it takes.
    let cursor = s.cursor.as_ref().and_then(|c| {
        let line = s.lines.get(c.row)?;
        let (col, width, _) = line.graphemes().find(|&(col, w, _)| col + w > c.col)?;
        let fg = columns[c.row]
            .iter()
            .position(|&(a, b)| col >= a && col < b)
            .map(|i| hsla(line.spans[i].fg));
        let colour = c.colour.or(s.colours.cursor).or(fg).unwrap_or(s.colours.fg);
        Some((c, col, width, colour))
    });

    pictures(Layer::BelowBackground, window);
    // The cells' backgrounds, then the marks over them, each joined where
    // they meet: a screen of them is a few rectangles, not one a run.
    let (mut grounds, mut marks) = (vec![], vec![]);
    for (r, line) in s.lines.iter().enumerate() {
        for (sp, &(c0, c1)) in line.spans.iter().zip(&columns[r]) {
            if c1 <= c0 {
                continue;
            }
            let b = Bounds::new(at(r, c0), size(cw * (c1 - c0) as f32, lh));
            if sp.own_bg {
                grounds.push(Rect { b, c: hsla(sp.bg) });
            }
            let mark = match sp.mark {
                Mark::None => None,
                Mark::Selected => Some(s.colours.selected),
                Mark::Match => Some(s.colours.found),
                Mark::Current => Some(s.colours.current),
            };
            if let Some(c) = mark {
                marks.push(Rect { b, c });
            }
        }
    }
    for r in coalesce(grounds).into_iter().chain(coalesce(marks)) {
        window.paint_quad(fill(r.b, r.c));
    }
    pictures(Layer::BelowText, window);
    // A block cursor goes under its cell's text, drawn in the cell's
    // background over it.
    let block = cursor.filter(|(c, ..)| c.style == CursorStyle::Block);
    if let Some((c, col, width, colour)) = block {
        quad(window, at(c.row, col), cw * width as f32, lh, colour);
    }
    // Box drawing and blocks, gathered from every row and joined.
    let mut sprites = vec![];
    for (r, line) in s.lines.iter().enumerate() {
        let on_cursor = |col: usize| block.is_some_and(|(c, cc, ..)| c.row == r && cc == col);
        let row = at(r, 0);
        text(
            s,
            &m,
            line,
            &columns[r],
            row,
            &on_cursor,
            &mut sprites,
            window,
            cx,
        );
        for (sp, &(c0, c1)) in line.spans.iter().zip(&columns[r]) {
            if c1 > c0 {
                let colour = text_colour(s, sp);
                lines_through(s, &m, sp, at(r, c0), cw * (c1 - c0) as f32, colour, window);
            }
        }
    }
    for r in coalesce(sprites) {
        window.paint_quad(fill(r.b, r.c));
    }
    pictures(Layer::AboveText, window);
    if let Some((c, col, width, colour)) = cursor {
        let from = at(c.row, col);
        let w = cw * width as f32;
        let t = px(m.light);
        match c.style {
            CursorStyle::Block => {}
            CursorStyle::Bar => quad(window, from, t, lh, colour),
            CursorStyle::Underline => {
                quad(window, from + point(px(0.), m.underline), w, t * 2., colour)
            }
            CursorStyle::HollowBlock => {
                quad(window, from, w, t, colour);
                quad(window, from + point(px(0.), lh - t), w, t, colour);
                quad(window, from, t, lh, colour);
                quad(window, from + point(w - t, px(0.)), t, lh, colour);
            }
        }
    }
}

/// The colour a span's text is drawn in, against its background with the
/// contrast the settings ask for.
fn text_colour(s: &Screen, sp: &Span) -> Hsla {
    if sp.mark == Mark::Current {
        s.colours.mark_text
    } else {
        hsla(contrasting(sp.fg, sp.bg, s.min_contrast))
    }
}

/// A colour's relative luminance, as WCAG measures it.
fn luminance(c: Rgb) -> f32 {
    let lin = |v: u8| {
        let v = v as f32 / 255.;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
}

/// The contrast between two colours, from 1 (none) to 21.
pub fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Text's colour against its background: as it is, unless its contrast is
/// below `least`, then white or black, whichever has more, as Ghostty's
/// minimum-contrast draws it.
pub fn contrasting(fg: Rgb, bg: Rgb, least: f32) -> Rgb {
    if least <= 1. || contrast(fg, bg) >= least {
        return fg;
    }
    let white = Rgb {
        r: 255,
        g: 255,
        b: 255,
    };
    let black = Rgb { r: 0, g: 0, b: 0 };
    if contrast(white, bg) >= contrast(black, bg) {
        white
    } else {
        black
    }
}

fn font(s: &Screen, sp: &Span) -> Font {
    Font {
        weight: if sp.bold {
            gpui::FontWeight::BOLD
        } else {
            Default::default()
        },
        style: if sp.italic {
            gpui::FontStyle::Italic
        } else {
            Default::default()
        },
        ..s.font.clone()
    }
}

/// A row's text: plain characters of a span in runs at the cell's width,
/// box drawing as shapes, anything else alone at its cell.
#[allow(clippy::too_many_arguments)]
fn text(
    s: &Screen,
    m: &Metrics,
    line: &Line,
    columns: &[(usize, usize)],
    row: Point<Pixels>,
    on_cursor: &dyn Fn(usize) -> bool,
    sprites: &mut Vec<Rect>,
    window: &mut Window,
    cx: &mut App,
) {
    let cw = s.cell;
    // The span a column is in: characters come in order, so it only moves
    // on.
    let mut si = 0;
    // The run being gathered: its span, first column and text.
    let mut run: Option<(usize, usize, String)> = None;
    let flush = |run: &mut Option<(usize, usize, String)>, window: &mut Window, cx: &mut App| {
        if let Some((si, col, t)) = run.take()
            && !t.trim().is_empty()
        {
            let sp = &line.spans[si];
            shaped(
                s,
                &t,
                sp,
                text_colour(s, sp),
                true,
                row + point(cw * col as f32, px(0.)),
                window,
                cx,
            );
        }
    };
    for (col, width, g) in line.graphemes() {
        while si < columns.len() && columns[si].1 <= col {
            si += 1;
        }
        let Some(sp) = line.spans.get(si) else { break };
        let plain = width == 1 && g.len() == 1 && g.as_bytes()[0].is_ascii() && !on_cursor(col);
        if plain {
            match &mut run {
                Some((rs, rc, t)) if *rs == si && *rc + t.len() == col => t.push_str(g),
                _ => {
                    flush(&mut run, window, cx);
                    run = Some((si, col, g.to_string()));
                }
            }
            continue;
        }
        flush(&mut run, window, cx);
        if g.trim().is_empty() {
            continue;
        }
        // The text under a block cursor is drawn in its cell's background.
        let colour = if on_cursor(col) {
            hsla(sp.bg)
        } else {
            text_colour(s, sp)
        };
        let from = row + point(cw * col as f32, px(0.));
        let mut chars = g.chars();
        let sprite = match (chars.next(), chars.next()) {
            (Some(c), None) => termsprite::shapes(
                c,
                f32::from(cw) * width as f32,
                f32::from(s.line_height),
                m.light,
            ),
            _ => None,
        };
        match sprite {
            Some(shapes) => paint_shapes(&shapes, from, colour, sprites, window),
            None => shaped(
                s,
                g,
                sp,
                colour,
                width == 1 && g.is_ascii(),
                from,
                window,
                cx,
            ),
        }
    }
    flush(&mut run, window, cx);
}

/// Shapes and paints text from a point, its characters a cell apart when
/// `fixed`.
#[allow(clippy::too_many_arguments)]
fn shaped(
    s: &Screen,
    t: &str,
    sp: &Span,
    colour: Hsla,
    fixed: bool,
    from: Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let run = TextRun {
        len: t.len(),
        font: font(s, sp),
        color: colour,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window.text_system().shape_line(
        t.to_string().into(),
        s.font_size,
        &[run],
        fixed.then_some(s.cell),
    );
    let _ = line.paint(from, s.line_height, TextAlign::Left, None, window, cx);
}

/// A filled rectangle to paint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub b: Bounds<Pixels>,
    pub c: Hsla,
}

/// Rectangles of a colour that meet, joined: those side by side on a row,
/// then those stacked in a column. A rule across the screen is one
/// rectangle, not two a cell, and each one painted costs.
pub fn coalesce(mut rects: Vec<Rect>) -> Vec<Rect> {
    let near = |a: Pixels, b: Pixels| (f32::from(a) - f32::from(b)).abs() < 0.01;
    let key = |p: Pixels| f32::from(p);
    // Side by side.
    rects.sort_by(|a, b| {
        (key(a.b.origin.y), key(a.b.size.height), key(a.b.origin.x))
            .partial_cmp(&(key(b.b.origin.y), key(b.b.size.height), key(b.b.origin.x)))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut rows: Vec<Rect> = Vec::with_capacity(rects.len());
    for r in rects {
        match rows.last_mut() {
            Some(l)
                if l.c == r.c
                    && near(l.b.origin.y, r.b.origin.y)
                    && near(l.b.size.height, r.b.size.height)
                    && near(l.b.origin.x + l.b.size.width, r.b.origin.x) =>
            {
                l.b.size.width = r.b.origin.x + r.b.size.width - l.b.origin.x;
            }
            _ => rows.push(r),
        }
    }
    // Stacked.
    rows.sort_by(|a, b| {
        (key(a.b.origin.x), key(a.b.size.width), key(a.b.origin.y))
            .partial_cmp(&(key(b.b.origin.x), key(b.b.size.width), key(b.b.origin.y)))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut out: Vec<Rect> = Vec::with_capacity(rows.len());
    for r in rows {
        match out.last_mut() {
            Some(l)
                if l.c == r.c
                    && near(l.b.origin.x, r.b.origin.x)
                    && near(l.b.size.width, r.b.size.width)
                    && near(l.b.origin.y + l.b.size.height, r.b.origin.y) =>
            {
                l.b.size.height = r.b.origin.y + r.b.size.height - l.b.origin.y;
            }
            _ => out.push(r),
        }
    }
    out
}

/// Paints a sprite's lines and fills, its rectangles gathered to be
/// joined with its neighbours'.
fn paint_shapes(
    shapes: &[Shape],
    from: Point<Pixels>,
    colour: Hsla,
    sprites: &mut Vec<Rect>,
    window: &mut Window,
) {
    let to = |(x, y): (f32, f32)| from + point(px(x), px(y));
    for shape in shapes {
        match shape {
            Shape::Rect { x, y, w, h, alpha } => sprites.push(Rect {
                b: Bounds::new(to((*x, *y)), size(px(*w), px(*h))),
                c: colour.opacity(*alpha),
            }),
            Shape::Stroke { points, width } => {
                let mut path = PathBuilder::stroke(px(*width));
                path_through(&mut path, points, &to);
                if let Ok(path) = path.build() {
                    window.paint_path(path, colour);
                }
            }
            Shape::Fill { points } => {
                let mut path = PathBuilder::fill();
                path_through(&mut path, points, &to);
                path.close();
                if let Ok(path) = path.build() {
                    window.paint_path(path, colour);
                }
            }
        }
    }
}

fn path_through(
    path: &mut PathBuilder,
    points: &[(f32, f32)],
    to: &dyn Fn((f32, f32)) -> Point<Pixels>,
) {
    let mut last = None;
    for &p in points {
        match last {
            None => path.move_to(to(p)),
            Some(l) if l == p => continue,
            Some(_) => path.line_to(to(p)),
        }
        last = Some(p);
    }
}

/// A span's underline in its style, strikethrough and overline.
fn lines_through(
    s: &Screen,
    m: &Metrics,
    sp: &Span,
    from: Point<Pixels>,
    w: Pixels,
    text: Hsla,
    window: &mut Window,
) {
    let t = px(m.light);
    let bar = |window: &mut Window, y: Pixels, x: Pixels, len: Pixels, c: Hsla| {
        window.paint_quad(fill(Bounds::new(from + point(x, y), size(len, t)), c));
    };
    let colour = sp.underline_colour.map(hsla).unwrap_or(text);
    let y = m.underline;
    match sp.underline {
        Underline::None => {}
        Underline::Single => bar(window, y, px(0.), w, colour),
        Underline::Double => {
            bar(window, y - t, px(0.), w, colour);
            bar(window, y + t, px(0.), w, colour);
        }
        Underline::Curly => window.paint_underline(
            from + point(px(0.), y - t),
            w,
            &gpui::UnderlineStyle {
                thickness: t,
                color: Some(colour),
                wavy: true,
            },
        ),
        Underline::Dotted => {
            let mut x = px(0.);
            while x < w {
                bar(window, y, x, t.min(w - x), colour);
                x += t * 2.;
            }
        }
        Underline::Dashed => {
            // A dash a cell, as Ghostty draws them, so they keep step
            // with the text.
            let cells = (f32::from(w) / f32::from(s.cell)).round() as usize;
            let dash = s.cell * 0.6;
            for i in 0..cells {
                bar(
                    window,
                    y,
                    s.cell * i as f32 + (s.cell - dash) / 2.,
                    dash,
                    colour,
                );
            }
        }
    }
    if sp.strikethrough {
        bar(window, m.strike, px(0.), w, text);
    }
    if sp.overline {
        bar(window, px(0.), px(0.), w, text);
    }
}
