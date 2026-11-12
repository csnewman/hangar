//! The terminal's screen: what a shell writes, parsed into the grid and
//! turned into lines of styled spans fast enough to draw every frame; what
//! is selected, and Find through the scrollback.

use std::time::Instant;

use hangar_app::terminal::{LinkKind, Mark, Screen, Select};

#[test]
fn colours_and_attributes_become_spans() {
    let mut s = Screen::new(20, 3);
    s.feed(b"plain \x1b[1;31mred\x1b[0m \x1b[38;2;1;2;3mrgb\x1b[0m\r\n\x1b[7minv\x1b[0m");
    let lines = s.lines();
    assert_eq!(lines[0].text.trim_end(), "plain red rgb");
    let spans = &lines[0].spans;
    assert_eq!(spans[0].len, 6);
    assert!(spans[1].bold && spans[1].fg.r == 0xcd && spans[1].len == 3);
    assert_eq!((spans[3].fg.r, spans[3].fg.g, spans[3].fg.b), (1, 2, 3));
    // Inverse swaps the colours.
    assert_eq!(lines[1].spans[0].bg, lines[0].spans[0].fg);
    assert_eq!(s.cursor(), (1, 3));
}

/// On a light background the sixteen named colours are a light theme's,
/// yellow and white dark enough to read; on a dark one they are a dark
/// theme's. Colours the shell gives exactly are its own either way.
#[test]
fn named_colours_follow_the_background() {
    use alacritty_terminal::vte::ansi::Rgb;
    let rgb = |r, g, b| Rgb { r, g, b };
    let mut s = Screen::new(30, 1);
    s.feed(b"\x1b[33my\x1b[37mw\x1b[38;2;1;2;3mx");
    let fg = |s: &mut Screen| -> Vec<Rgb> { s.lines()[0].spans.iter().map(|sp| sp.fg).collect() };
    s.set_default_colours(rgb(0xd3, 0xe1, 0xe0), rgb(0x1d, 0x2c, 0x2e));
    assert_eq!(
        fg(&mut s)[..3],
        [rgb(0xe5, 0xe5, 0x10), rgb(0xe5, 0xe5, 0xe5), rgb(1, 2, 3)]
    );
    s.set_default_colours(rgb(0x10, 0x20, 0x21), rgb(0xff, 0xff, 0xff));
    assert_eq!(
        fg(&mut s)[..3],
        [rgb(0x94, 0x98, 0x00), rgb(0x55, 0x55, 0x55), rgb(1, 2, 3)]
    );
}

#[test]
fn a_full_screen_program_and_a_resize() {
    let mut s = Screen::new(10, 4);
    // The alternate screen, cleared, text placed at row 3 column 5.
    s.feed(b"before\x1b[?1049h\x1b[2J\x1b[3;5Hhi");
    assert_eq!(s.lines()[2].text, "    hi    ");
    s.feed(b"\x1b[?1049l");
    assert!(s.lines()[0].text.starts_with("before"));
    s.resize(6, 2);
    assert_eq!(s.lines().len(), 2);
    assert_eq!(s.lines()[0].text.chars().count(), 6);
}

#[test]
fn throughput() {
    // Colourful output, like a build log or `ls --color`.
    let mut out = Vec::new();
    for i in 0..200_000 {
        out.extend_from_slice(
            format!(
                "\x1b[3{}mline {i}\x1b[0m some ordinary text after it\r\n",
                i % 8
            )
            .as_bytes(),
        );
    }
    let mut s = Screen::new(200, 50);
    let start = Instant::now();
    s.feed(&out);
    let parse = start.elapsed();
    eprintln!("{} MB parsed in {parse:?}", out.len() >> 20);
    let start = Instant::now();
    for _ in 0..100 {
        std::hint::black_box(s.lines());
    }
    eprintln!(
        "a 200x50 screen made into lines in {:?}",
        start.elapsed() / 100
    );
}

/// The text of a line's cells that carry a mark.
fn marked(s: &mut Screen, row: usize, mark: Mark) -> String {
    let line = &s.lines()[row];
    let mut at = 0;
    let mut out = String::new();
    for span in &line.spans {
        if span.mark == mark {
            out.push_str(&line.text[at..at + span.len]);
        }
        at += span.len;
    }
    out
}

#[test]
fn the_mouse_selects_cells_words_and_lines() {
    let mut s = Screen::new(30, 3);
    s.feed(b"cargo build --release\r\nsecond line");
    // A drag from the 'b' of build to the 'd'.
    s.select(0, 6, false, Select::Cells);
    s.select_to(0, 10, true);
    assert_eq!(s.selection().as_deref(), Some("build"));
    assert_eq!(marked(&mut s, 0, Mark::Selected), "build");
    // A double click takes the word, a triple click the line.
    s.select(0, 14, false, Select::Words);
    assert_eq!(s.selection().as_deref(), Some("--release"));
    // A whole line comes with its newline, as other terminals copy it.
    s.select(1, 2, false, Select::Lines);
    assert_eq!(s.selection().as_deref(), Some("second line\n"));
    // Down across lines, as the shell wrote them.
    s.select(0, 18, false, Select::Cells);
    s.select_to(1, 5, true);
    assert_eq!(s.selection().as_deref(), Some("ase\nsecond"));
    s.clear_selection();
    assert_eq!(s.selection(), None);
    // A click that drags nowhere selects nothing.
    s.select(0, 3, false, Select::Cells);
    assert_eq!(s.selection(), None);
}

#[test]
fn find_goes_through_the_scrollback_newest_first() {
    let mut s = Screen::new(20, 3);
    for i in 0..10 {
        s.feed(format!("line {i} ok\r\n").as_bytes());
    }
    s.feed(b"error: one\r\nfine\r\nERROR two");
    assert!(s.find("error", true), "any case, without a capital");
    assert_eq!(
        marked(&mut s, 2, Mark::Current),
        "ERROR",
        "the newest first"
    );
    assert_eq!(marked(&mut s, 0, Mark::Match), "error", "the others marked");
    assert!(s.find("error", true));
    assert_eq!(marked(&mut s, 0, Mark::Current), "error", "up, the older");
    assert!(s.find("error", true));
    assert_eq!(marked(&mut s, 2, Mark::Current), "ERROR", "round again");
    assert!(s.find("ERROR", true), "a capital matches only itself");
    assert!(!s.find("nowhere", true));
    assert!(!s.found());

    // An old line is scrolled to.
    assert!(s.find("line 2 ", true));
    let row = (0..3)
        .find(|r| !marked(&mut s, *r, Mark::Current).is_empty())
        .expect("in view");
    assert_eq!(marked(&mut s, row, Mark::Current), "line 2 ");
    // Text is found as typed, not as a pattern.
    s.feed(b"\r\na.b (x)");
    assert!(s.find("(x)", true));
    assert!(!s.find("a*b", true));
    s.find("", true);
    assert_eq!(marked(&mut s, 2, Mark::Match), "");
}

/// What a program asks of the keyboard and mouse is kept: the Kitty
/// keyboard protocol, mouse reports, and the wheel as arrows on its own
/// screen.
#[test]
fn a_programs_asks_are_kept() {
    let mut s = Screen::new(20, 4);
    assert!(!s.key_modes(false).disambiguate);
    s.feed(b"\x1b[>1u");
    assert!(
        s.key_modes(false).disambiguate,
        "Kitty's disambiguate pushed"
    );
    s.feed(b"\x1b[<u");
    assert!(!s.key_modes(false).disambiguate, "and popped");

    assert!(!s.mouse_modes().any());
    s.feed(b"\x1b[?1000h\x1b[?1006h");
    let m = s.mouse_modes();
    assert!(m.clicks && m.sgr && !m.drags);
    s.feed(b"\x1b[?1002h");
    assert!(s.mouse_modes().drags);

    assert!(!s.wheel_is_arrows(), "the shell's own screen scrolls back");
    s.feed(b"\x1b[?1049h");
    assert!(
        s.wheel_is_arrows(),
        "a full-screen program's moves its cursor"
    );
}

/// A link is found where it is clicked: one the program wrote (OSC 8), with
/// the text it showed; an address in the text, whole when it wraps onto the
/// next line; or a file with a line, as compilers write them.
#[test]
fn links_are_found_where_they_are() {
    let url = |s: &str| Some(LinkKind::Url(s.to_string()));
    let kind = |s: &mut Screen, row, col| s.link_at(row, col).map(|l| l.kind);
    let mut s = Screen::new(30, 4);
    s.feed(b"see https://example.com/a?b=1 now\r\n");
    assert_eq!(kind(&mut s, 0, 8), url("https://example.com/a?b=1"));
    assert_eq!(kind(&mut s, 0, 1), None, "not on the link");
    s.feed(b"\x1b]8;;https://hangar.dev/docs\x1b\\the docs\x1b]8;;\x1b\\ here");
    assert_eq!(
        kind(&mut s, 2, 2),
        Some(LinkKind::Embedded {
            uri: "https://hangar.dev/docs".into(),
            text: "the docs".into()
        })
    );
    assert_eq!(kind(&mut s, 2, 10), None, "after the link");

    let mut s = Screen::new(20, 3);
    s.feed(b"go to https://example.com/a/long/path ok");
    let long = "https://example.com/a/long/path";
    assert_eq!(kind(&mut s, 0, 10), url(long));
    assert_eq!(kind(&mut s, 1, 3), url(long), "from its wrapped half");

    let mut s = Screen::new(60, 3);
    s.feed(b"error at src/store/redis.rs:31:9: no\r\n  --> ../lib.rs:4\r\n");
    assert_eq!(
        kind(&mut s, 0, 12),
        Some(LinkKind::Path {
            path: "src/store/redis.rs".into(),
            line: 31,
            column: 9
        })
    );
    assert_eq!(
        kind(&mut s, 1, 8),
        Some(LinkKind::Path {
            path: "../lib.rs".into(),
            line: 4,
            column: 0
        })
    );
    assert_eq!(kind(&mut s, 0, 2), None, "a word is not a path");
}

/// The link under the pointer is underlined, and only it.
#[test]
fn the_hovered_link_is_underlined() {
    let mut s = Screen::new(40, 2);
    s.feed(b"open http://localhost:5173/ now");
    let underlined = |s: &mut Screen| -> String {
        let line = s.lines().remove(0);
        let (mut at, mut out) = (0, String::new());
        for sp in &line.spans {
            if sp.underline {
                out += &line.text[at..at + sp.len];
            }
            at += sp.len;
        }
        out
    };
    assert_eq!(underlined(&mut s), "");
    let link = s.link_at(0, 10);
    s.set_hover(link.as_ref());
    assert_eq!(underlined(&mut s), "http://localhost:5173/");
    s.set_hover(None);
    assert_eq!(underlined(&mut s), "");
}

/// Select All takes the scrollback and the screen; Clear Buffer leaves the
/// cursor's line, the prompt, at the top, and nothing before it.
#[test]
fn select_all_and_clear_the_buffer() {
    let mut s = Screen::new(20, 3);
    s.feed(b"one\r\ntwo\r\nthree\r\nfour\r\n$ ls");
    s.select_all();
    assert_eq!(
        s.selection().as_deref(),
        Some("one\ntwo\nthree\nfour\n$ ls")
    );

    s.clear_buffer();
    assert_eq!(s.selection(), None, "the selection goes with the text");
    let text: Vec<String> = s
        .lines()
        .iter()
        .map(|l| l.text.trim_end().to_string())
        .collect();
    assert_eq!(text, ["$ ls", "", ""]);
    assert_eq!(s.cursor(), (0, 4), "the cursor stays on its line");
    s.scroll(5);
    assert_eq!(s.scrolled_back(), 0, "no scrollback is left");
    s.select_all();
    assert_eq!(s.selection().as_deref(), Some("$ ls"));
}
