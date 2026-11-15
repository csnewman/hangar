//! A terminal's screen kept by Ghostty's emulation, through the Emulator
//! the terminal view uses: what it draws, selects, finds and links, and
//! what it says it is.

use alacritty_terminal::vte::ansi::Rgb;
use hangar_app::termghostty::Ghostty;
use hangar_app::terminal::{CursorStyle, Emulator, LinkKind, Mark, Pointer, Select, Underline};

fn screen(cols: usize, rows: usize) -> Ghostty {
    let mut s = Ghostty::new(cols, rows);
    s.resize(cols, rows, (8, 16));
    s.set_default_colours(
        Rgb {
            r: 0xd4,
            g: 0xd4,
            b: 0xd4,
        },
        Rgb {
            r: 0x1e,
            g: 0x1e,
            b: 0x1e,
        },
    );
    s
}

#[test]
fn styled_text_is_drawn_in_runs() {
    let mut s = screen(20, 2);
    s.feed(b"plain \x1b[1mbold\x1b[0m");
    let lines = s.lines();
    assert!(
        lines[0].text.starts_with("plain bold"),
        "{:?}",
        lines[0].text
    );
    let first = lines[0].spans[0];
    assert_eq!(first.len, 6);
    assert!(!first.bold);
    assert!(lines[0].spans[1].bold);
    assert_eq!(lines[0].spans[1].len, 4);
    assert_eq!(s.cursor(), (0, 10));
    assert!(s.cursor_shown());
}

/// Faint text is halfway from its colour to its background.
#[test]
fn faint_text_fades_towards_its_background() {
    let mut s = screen(20, 1);
    s.feed(b"\x1b[2mhint\x1b[0m text");
    let spans = &s.lines()[0].spans;
    assert_eq!(
        spans[0].fg,
        Rgb {
            r: 0x79,
            g: 0x79,
            b: 0x79
        }
    );
    assert_eq!(spans[1].fg.r, 0xd4, "the rest as it was");
}

/// Underlines keep their style and colour, and struck-through and
/// overlined text is so; each column names where its character starts.
#[test]
fn every_line_through_the_text_is_kept() {
    let mut s = screen(20, 1);
    s.feed("\x1b[4:3;58:2::1:2:3ma\x1b[0;4:5mb\x1b[0;9mc\x1b[0;53md\x1b[0m\u{4e16}e".as_bytes());
    let line = s.lines().remove(0);
    let sp = &line.spans;
    assert_eq!(sp[0].underline, Underline::Curly);
    assert_eq!(
        sp[0].underline_colour.map(|c| (c.r, c.g, c.b)),
        Some((1, 2, 3))
    );
    assert_eq!(sp[1].underline, Underline::Dashed);
    assert!(sp[2].strikethrough);
    assert!(sp[3].overline);
    assert_eq!(&line.cells[..7], &[0, 1, 2, 3, 4, 4, 7]);
}

/// The cursor is drawn as the program asks, in the colour it sets.
#[test]
fn the_cursor_is_drawn_as_asked() {
    let mut s = screen(10, 1);
    s.feed(b"\x1b[4 q\x1b]12;#00ff00\x07");
    s.lines();
    let look = s.cursor_look();
    assert_eq!((look.style, look.blinking), (CursorStyle::Underline, false));
    assert_eq!(look.colour.map(|c| (c.r, c.g, c.b)), Some((0, 255, 0)));
}

/// The bell, a full reset and the pointer the program asks for are said.
#[test]
fn the_bell_a_reset_and_the_pointer_are_said() {
    let mut s = screen(20, 2);
    assert_eq!(s.pointer(), Pointer::Text);
    s.feed(b"\x07\x1b]22;pointer\x07");
    let e = s.effects();
    assert!(e.bell && !e.reset);
    assert_eq!(s.pointer(), Pointer::Pointer);
    s.feed(b"\x1bc");
    assert!(s.effects().reset);
}

/// A program's link that wraps onto the next row is one link, across both.
#[test]
fn a_programs_link_follows_its_text_onto_the_next_row() {
    let mut s = screen(10, 3);
    s.feed(b"see \x1b]8;;https://example.com/\x07a long link\x1b]8;;\x07 ok");
    let link = s.link_at(1, 2).expect("the link's second row");
    match &link.kind {
        LinkKind::Embedded { text, .. } => assert_eq!(text, "a long link"),
        other => panic!("{other:?}"),
    }
    // Hovered, it is underlined on both rows.
    s.set_hover(Some(&link));
    let lines = s.lines();
    let underlined = |row: usize| -> String {
        let line = &lines[row];
        let mut at = 0;
        let mut out = String::new();
        for sp in &line.spans {
            if sp.underline != Underline::None {
                out += &line.text[at..at + sp.len];
            }
            at += sp.len;
        }
        out
    };
    assert_eq!(underlined(0), "a long");
    assert_eq!(underlined(1), " link");
}

/// The view moves from prompt to prompt the shell marked (OSC 133).
#[test]
fn the_view_moves_by_prompts() {
    let mut s = screen(20, 3);
    for cmd in ["one", "two"] {
        s.feed(format!("\x1b]133;A\x07$ {cmd}\r\n").as_bytes());
        for i in 0..5 {
            s.feed(format!("{cmd} {i}\r\n").as_bytes());
        }
    }
    s.scroll_prompts(-2);
    assert_eq!(s.lines()[0].text.trim_end(), "$ one");
    s.scroll_prompts(1);
    assert_eq!(s.lines()[0].text.trim_end(), "$ two");
}

/// The scrollback keeps thousands of lines, as Ghostty's own does.
#[test]
fn the_scrollback_keeps_thousands_of_lines() {
    let mut s = screen(80, 24);
    for i in 0..5000 {
        s.feed(format!("line {i}, with some more text after it\r\n").as_bytes());
    }
    s.scroll(10_000);
    assert_eq!(s.scrolled_back(), 5000 + 1 - 24);
    assert_eq!(
        s.lines()[0].text.trim_end(),
        "line 0, with some more text after it"
    );
}

#[test]
fn a_drag_selects_the_cells_it_passes() {
    let mut s = screen(20, 2);
    s.feed(b"hello world");
    // From the left half of h to the right half of o: all five.
    s.select(0, 0, false, Select::Cells);
    assert_eq!(s.selection(), None, "nothing before the pointer moves");
    s.select_to(0, 4, true);
    assert_eq!(s.selection().as_deref(), Some("hello"));
    // Back past the start, from the right half of w: "hello w" less h.
    s.select(0, 6, true, Select::Cells);
    s.select_to(0, 1, false);
    assert_eq!(s.selection().as_deref(), Some("ello w"));
    let lines = s.lines();
    let marks: Vec<Mark> = lines[0].spans.iter().map(|sp| sp.mark).collect();
    assert!(marks.contains(&Mark::Selected), "{marks:?}");
}

/// A rectangle selects the same columns of each row between.
#[test]
fn a_rectangle_selects_columns() {
    let mut s = screen(10, 3);
    s.feed(b"abcdef\r\nghijkl\r\nmnopqr");
    s.select(0, 1, false, Select::Block);
    s.select_to(2, 3, true);
    assert_eq!(s.selection().as_deref(), Some("bcd\nhij\nnop"));
}

#[test]
fn a_double_click_drag_selects_by_words() {
    let mut s = screen(30, 2);
    s.feed(b"one two three four");
    s.select(0, 5, false, Select::Words);
    assert_eq!(s.selection().as_deref(), Some("two"));
    s.select_to(0, 10, false);
    assert_eq!(s.selection().as_deref(), Some("two three"));
    s.select(0, 1, false, Select::Lines);
    assert_eq!(s.selection().as_deref(), Some("one two three four"));
}

#[test]
fn links_are_found_across_wrapped_rows() {
    let mut s = screen(20, 4);
    s.feed(b"see https://example.com/a/long/path here");
    // The address starts on the first row and wraps onto the second.
    let link = s.link_at(1, 3).expect("the address on the second row");
    assert_eq!(
        link.kind,
        LinkKind::Url("https://example.com/a/long/path".into())
    );
    assert_eq!(s.link_at(0, 1), None, "plain text");
    s.feed(b"\r\nsrc/main.rs:12:3 failed");
    let row = 2;
    match s.link_at(row, 4).map(|l| l.kind) {
        Some(LinkKind::Path { path, line, column }) => {
            assert_eq!((path.as_str(), line, column), ("src/main.rs", 12, 3))
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_programs_own_link_is_its_text_and_address() {
    let mut s = screen(30, 2);
    s.feed(b"open \x1b]8;;https://example.com/x\x07the page\x1b]8;;\x07 to read");
    match s.link_at(0, 7).map(|l| l.kind) {
        Some(LinkKind::Embedded { uri, text }) => {
            assert_eq!(
                (uri.as_str(), text.as_str()),
                ("https://example.com/x", "the page")
            )
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn find_marks_its_matches() {
    let mut s = screen(20, 3);
    s.feed(b"cat dog\r\ndog cat");
    assert!(s.find("dog", true));
    let lines = s.lines();
    let marked = |row: usize, mark: Mark| lines[row].spans.iter().any(|sp| sp.mark == mark);
    assert!(marked(1, Mark::Current), "the newest is the one found");
    assert!(marked(0, Mark::Match));
    assert!(!s.find("bird", true));
}

#[test]
fn it_says_it_is_ghostty() {
    let mut s = screen(20, 2);
    let (name, version) = s.identity().expect("a name");
    assert_eq!(name, "ghostty");
    assert!(version.starts_with("1."), "{version}");
    s.feed(b"\x1b[>0q");
    let reply = String::from_utf8(s.effects().reply).unwrap();
    assert_eq!(reply, format!("\x1bP>|ghostty {version}\x1b\\"));
    assert!(s.takes_kitty_graphics());
}

#[test]
fn a_programs_copy_and_title_are_effects() {
    let mut s = screen(20, 2);
    s.feed(b"\x1b]0;my title\x07\x1b]52;c;aGk=\x07");
    let e = s.effects();
    assert_eq!(e.title.as_deref(), Some("my title"));
    assert_eq!(e.copied.as_deref(), Some("hi"));
}

#[test]
fn modes_reach_the_keys_and_mouse() {
    let mut s = screen(20, 2);
    s.feed(b"\x1b[?1h\x1b[?1000h\x1b[?1006h\x1b[?2004h\x1b[>1u");
    let k = s.key_modes(false);
    assert!(k.app_cursor && k.disambiguate && !k.all_as_escapes);
    let m = s.mouse_modes();
    assert!(m.clicks && !m.drags && !m.motion && m.sgr);
    assert!(s.bracketed_paste());
    assert!(!s.wheel_is_arrows());
    s.feed(b"\x1b[?1049h\x1b[?1007h");
    assert!(s.wheel_is_arrows());
    assert!(!s.reports_focus() && !s.reports_theme());
    s.feed(b"\x1b[?1004h\x1b[?2031h");
    assert!(s.reports_focus() && s.reports_theme());
}

#[test]
fn a_programs_pictures_are_kept_and_placed() {
    let mut s = screen(20, 4);
    // Two by two pixels, RGBA, four cells wide and a row tall.
    s.feed(b"\x1b_Ga=T,f=32,s=2,v=2,i=7,c=4,r=1,q=2;/wAA/wD/AP8AAP//////gA==\x1b\\");
    s.lines();
    let placed = s.placements();
    assert_eq!(placed.len(), 1, "{placed:?}");
    let p = placed[0];
    assert_eq!((p.image, p.row, p.col), (7, 0, 0));
    assert_eq!((p.width, p.height), (32, 16), "in the cells' pixels");
    let pic = s.picture(7).expect("its pixels");
    assert_eq!(&pic.rgba[..4], &[255, 0, 0, 255]);
    // Files on this machine are never read for a program's picture.
    s.feed(b"\x1b_Ga=T,t=f,i=8,q=2;L2V0Yy9wYXNzd2Q=\x1b\\");
    s.lines();
    assert!(s.picture(8).is_none());
}

/// A picture a program shows with kitty's Unicode placeholders is placed
/// over the placeholder cells, a row at a time, and the cells are blank.
#[test]
fn a_picture_is_placed_where_its_placeholders_are() {
    let mut s = screen(20, 4);
    // Image 9, two by two pixels, to be shown four cells wide and two
    // rows tall wherever its placeholders are (U=1).
    s.feed(b"\x1b_Ga=T,U=1,f=32,s=2,v=2,i=9,c=4,r=2,q=2;/wAA/wD/AP8AAP//////gA==\x1b\\");
    // Each placeholder cell names the image by its colour, and its row and
    // column by diacritics.
    let marks = ['\u{305}', '\u{30d}', '\u{30e}', '\u{310}'];
    let mut text = String::from("\x1b[38;5;9m");
    for row in 0..2 {
        for col in 0..4 {
            text.extend(['\u{10EEEE}', marks[row], marks[col]]);
        }
        text.push_str("\r\n");
    }
    text.push_str("\x1b[39mafter");
    s.feed(text.as_bytes());
    let lines = s.lines();
    assert_eq!(lines[0].text.trim_end(), "", "{:?}", lines[0].text);
    assert_eq!(lines[1].text.trim_end(), "");
    assert_eq!(lines[2].text.trim_end(), "after");
    // Ghostty gives it a row at a time; the rows are drawn as one.
    let placed = s.placements();
    assert_eq!(placed.len(), 1, "{placed:?}");
    let p = placed[0];
    assert_eq!((p.image, p.row, p.col), (9, 0, 0));
    assert_eq!((p.width, p.height), (32, 32), "four cells by two rows");
    assert_eq!(p.source, (0, 0, 2, 2), "the whole image");
}
