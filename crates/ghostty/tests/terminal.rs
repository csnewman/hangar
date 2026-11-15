//! The terminal as Hangar's app uses it: fed what a shell writes, read
//! back as rows of cells, and what the program asks of it taken as effects.

use hangar_ghostty::{Mouse, Progress, Rgb, Terminal, Wide};

fn text(t: &mut Terminal, row: usize) -> String {
    t.frame().rows[row]
        .cells
        .iter()
        .map(|c| {
            if c.text.is_empty() {
                " ".to_string()
            } else {
                c.text.clone()
            }
        })
        .collect::<String>()
        .trim_end()
        .to_string()
}

#[test]
fn text_and_its_style_are_drawn() {
    let mut t = Terminal::new(20, 3, "").unwrap();
    t.feed(b"plain \x1b[1;31mbold red\x1b[0m\r\n\xe4\xb8\x96x");
    let f = t.frame();
    assert_eq!(f.rows.len(), 3);
    assert_eq!(text(&mut t, 0), "plain bold red");
    let f2 = t.frame();
    let bold = &f2.rows[0].cells[6];
    assert_eq!(bold.text, "b");
    assert!(bold.bold);
    assert!(bold.fg.is_some(), "red has a colour of its own");
    assert!(
        f.rows[0].cells[0].fg.is_none(),
        "plain text takes the screen's"
    );
    // A wide character takes two cells, the second a spacer.
    assert_eq!(f2.rows[1].cells[0].text, "\u{4e16}");
    assert_eq!(f2.rows[1].cells[0].wide, Wide::Wide);
    assert_eq!(f2.rows[1].cells[1].wide, Wide::SpacerTail);
    assert_eq!(f2.rows[1].cells[2].text, "x");
    assert_eq!(f2.cursor, Some((1, 3)));
}

#[test]
fn colours_are_the_ones_set() {
    let mut t = Terminal::new(10, 2, "").unwrap();
    let mut palette = [Rgb::default(); 256];
    palette[1] = Rgb { r: 1, g: 2, b: 3 };
    let (fg, bg) = (
        Rgb { r: 9, g: 9, b: 9 },
        Rgb {
            r: 250,
            g: 250,
            b: 250,
        },
    );
    t.set_colours(fg, bg, &palette);
    t.feed(b"\x1b[31mx");
    let f = t.frame();
    assert_eq!((f.fg, f.bg), (fg, bg));
    assert_eq!(f.rows[0].cells[0].fg, Some(Rgb { r: 1, g: 2, b: 3 }));
    // A program asking the background is told it.
    t.feed(b"\x1b]11;?\x07");
    let reply = String::from_utf8(t.take_effects().reply).unwrap();
    assert!(reply.contains("11;rgb:fafa/fafa/fafa"), "{reply:?}");
}

#[test]
fn it_knows_its_ghostty_version() {
    let v = hangar_ghostty::ghostty_version();
    assert!(v.starts_with("1."), "{v:?}");
}

#[test]
fn it_answers_as_the_version_given() {
    let mut t = Terminal::new(10, 2, "ghostty 1.4.0").unwrap();
    t.feed(b"\x1b[>0q");
    assert_eq!(t.take_effects().reply, b"\x1bP>|ghostty 1.4.0\x1b\\");
}

#[test]
fn effects_are_taken() {
    let mut t = Terminal::new(10, 2, "").unwrap();
    t.feed(b"\x1b]2;the title\x07\x07");
    t.feed(b"\x1b]52;c;aMOpbGxv\x07");
    t.feed(b"\x1b]777;notify;Claude;Done\x07");
    t.feed(b"\x1b]9;4;1;40\x07");
    let e = t.take_effects();
    assert_eq!(e.title.as_deref(), Some("the title"));
    assert!(e.bell);
    assert_eq!(e.copied.as_deref(), Some("h\u{e9}llo"));
    assert_eq!(e.notifications.len(), 1);
    assert_eq!(
        (
            e.notifications[0].title.as_str(),
            e.notifications[0].body.as_str()
        ),
        ("Claude", "Done")
    );
    assert_eq!(e.progress, Some(Progress::Set(40)));
    assert_eq!(t.take_effects(), Default::default(), "taken once");
    // The clipboard is never read.
    t.feed(b"\x1b]52;c;?\x07");
    let reply = String::from_utf8(t.take_effects().reply).unwrap();
    assert!(!reply.contains("aMOp"), "{reply:?}");
}

#[test]
fn modes_are_read() {
    let mut t = Terminal::new(10, 2, "").unwrap();
    assert_eq!(t.mouse(), Mouse::None);
    assert!(!t.mode(2004, false));
    t.feed(b"\x1b[?1002h\x1b[?1006h\x1b[?2004h\x1b[?1049h\x1b[>1u");
    assert_eq!(t.mouse(), Mouse::Button);
    assert!(t.mode(1006, false));
    assert!(t.mode(2004, false));
    assert!(t.alternate_screen());
    assert_eq!(t.kitty_keyboard(), 1);
}

#[test]
fn the_scrollback_scrolls() {
    let mut t = Terminal::new(10, 3, "").unwrap();
    for i in 0..10 {
        t.feed(format!("line {i}\r\n").as_bytes());
    }
    assert_eq!(t.scrolled_back(), 0);
    t.scroll(2);
    assert_eq!(t.scrolled_back(), 2);
    assert_eq!(text(&mut t, 0), "line 6");
    t.scroll_to_bottom();
    assert_eq!(t.scrolled_back(), 0);
    assert_eq!(text(&mut t, 0), "line 8");
}

#[test]
fn a_selection_is_marked_and_read() {
    let mut t = Terminal::new(20, 3, "").unwrap();
    t.feed(b"hello world\r\nsecond line");
    let from = t.screen_point(0, 6).unwrap();
    let to = t.screen_point(1, 5).unwrap();
    t.select(from, to);
    assert_eq!(t.selection().as_deref(), Some("world\nsecond"));
    let f = t.frame();
    assert!(f.rows[0].cells[6].selected && !f.rows[0].cells[5].selected);
    let (a, b) = t.word_at(t.screen_point(1, 9).unwrap()).unwrap();
    t.select(a, b);
    assert_eq!(t.selection().as_deref(), Some("line"));
    t.clear_selection();
    assert_eq!(t.selection(), None);
}

#[test]
fn find_goes_from_the_newest_match() {
    let mut t = Terminal::new(20, 4, "").unwrap();
    t.feed(b"an apple\r\nAPPLE pie\r\nno match");
    assert!(t.find("apple", true));
    assert_eq!(t.found(), Some(((1, 0), (1, 4))), "the newest, any case");
    assert!(t.find("apple", true));
    assert_eq!(t.found(), Some(((0, 3), (0, 7))));
    assert_eq!(t.matches().len(), 2);
    assert!(!t.find("pear", true));
    assert!(!t.find("", true));
}

#[test]
fn clearing_the_buffer_keeps_the_cursors_row() {
    let mut t = Terminal::new(10, 4, "").unwrap();
    for i in 0..8 {
        t.feed(format!("old {i}\r\n").as_bytes());
    }
    t.feed(b"$ ");
    t.clear_buffer();
    assert_eq!(text(&mut t, 0), "$");
    assert_eq!(t.cursor(), ((0, 2), true));
    t.scroll(5);
    assert_eq!(t.scrolled_back(), 0, "no scrollback is left");
}

#[test]
fn kitty_graphics_are_answered_unless_turned_off() {
    let mut t = Terminal::new(10, 2, "").unwrap();
    let query = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\";
    t.feed(query);
    let reply = String::from_utf8(t.take_effects().reply).unwrap();
    assert!(reply.contains("i=31;OK"), "{reply:?}");
    t.set_kitty_image_limit(0);
    t.feed(query);
    assert_eq!(
        t.take_effects().reply,
        b"",
        "a query answered with images off"
    );
}

#[test]
fn hyperlinks_are_read() {
    let mut t = Terminal::new(30, 2, "").unwrap();
    t.feed(b"see \x1b]8;;https://example.com/\x07here\x1b]8;;\x07 and \x1b]8;;hangar-picture:7\x07 \x1b]8;;\x07");
    assert_eq!(
        t.hyperlink_at(0, 5).as_deref(),
        Some("https://example.com/")
    );
    assert_eq!(t.hyperlink_at(0, 1), None);
    assert_eq!(
        t.links_named("hangar-picture:", 0, 30, 2),
        [(0, 13, "7".to_string())]
    );
}
