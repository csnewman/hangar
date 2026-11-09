//! The terminal's screen: what a shell writes, parsed into the grid and
//! turned into lines of styled spans fast enough to draw every frame; what
//! is selected, and Find through the scrollback.

use std::time::Instant;

use hangar_app::terminal::{Mark, Screen, Select};

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
