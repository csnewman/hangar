//! A terminal's screen kept by Ghostty's emulation, through the Emulator
//! the terminal view uses: what it draws, selects, finds and links, and
//! what it says it is.

use alacritty_terminal::vte::ansi::Rgb;
use hangar_app::termghostty::Ghostty;
use hangar_app::terminal::{Emulator, LinkKind, Mark, Select};

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
