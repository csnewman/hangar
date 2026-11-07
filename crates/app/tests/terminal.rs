//! The terminal's screen: what a shell writes, parsed into the grid and
//! turned into lines of styled spans fast enough to draw every frame.

use std::time::Instant;

use hangar_app::terminal::Screen;

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
