//! The terminal as Hangar's app uses it: fed what a shell writes, read
//! back as rows of cells, and what the program asks of it taken as effects.

use hangar_ghostty::{Layer, Mouse, Progress, Rgb, Terminal, Wide};

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
fn it_knows_its_versions() {
    // Ghostty's, from the source, and the library's, from the release
    // pinned, neither with a build's suffix.
    let v = hangar_ghostty::ghostty_version();
    assert!(v.starts_with("1.") && !v.contains('+'), "{v:?}");
    assert_eq!(hangar_ghostty::vt_version(), "1.0.1");
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

#[test]
fn a_picture_is_placed_where_the_cursor_was() {
    let mut t = Terminal::new(20, 4, "").unwrap();
    t.resize(20, 4, 8, 16);
    t.feed(b"ab\r\n  ");
    // Two by two pixels, RGBA, shown at the cursor.
    t.feed(b"\x1b_Ga=T,f=32,s=2,v=2,i=5,q=2;/wAA/wD/AP8AAP//////gA==\x1b\\");
    let f = t.frame();
    assert_eq!(f.placements.len(), 1, "{:?}", f.placements);
    let p = f.placements[0];
    assert_eq!((p.image, p.row, p.col), (5, 1, 2));
    assert_eq!((p.width, p.height), (2, 2));
    assert_eq!(p.source, (0, 0, 2, 2));
    assert_eq!(p.layer, Layer::AboveText);
    let pic = t.picture(5).expect("its pixels");
    assert_eq!((pic.width, pic.height), (2, 2));
    assert_eq!(&pic.rgba[..4], &[255, 0, 0, 255]);
    assert_eq!(&pic.rgba[12..], &[255, 255, 255, 128]);
    // Scrolled up with the text, and gone once deleted.
    t.feed(b"\r\n\r\n\r\n");
    assert_eq!(t.frame().placements[0].row, 0);
    t.feed(b"\x1b_Ga=d,d=A,q=2\x1b\\");
    assert!(t.frame().placements.is_empty());
}

#[test]
fn rgb_and_png_pictures_are_read_as_rgba() {
    let mut t = Terminal::new(20, 4, "").unwrap();
    t.resize(20, 4, 8, 16);
    t.feed(b"\x1b_Ga=t,f=24,s=2,v=1,i=1,q=2;ChQeKDI8\x1b\\");
    let rgb = t.picture(1).expect("the RGB picture");
    assert_eq!(rgb.rgba, [10, 20, 30, 255, 40, 50, 60, 255]);
    // A PNG, sized by its own header, its placement in cells and pixels.
    let mut png = vec![];
    {
        let mut e = png::Encoder::new(&mut png, 3, 1);
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        let mut w = e.write_header().unwrap();
        w.write_image_data(&[1, 2, 3, 4, 5, 6, 7, 8, 9]).unwrap();
    }
    let b64 = base64(&png);
    t.feed(format!("\x1b_Ga=T,f=100,i=2,c=4,r=2,z=-5,q=2;{b64}\x1b\\").as_bytes());
    let pic = t.picture(2).expect("the PNG decoded");
    assert_eq!((pic.width, pic.height), (3, 1));
    assert_eq!(pic.rgba, [1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255]);
    let f = t.frame();
    let p = f.placements.iter().find(|p| p.image == 2).expect("placed");
    assert_eq!((p.width, p.height), (32, 32), "four cells by two");
    assert_eq!((p.layer, p.z), (Layer::BelowText, -5));
    assert!(t.picture(9).is_none(), "no such image");
}

#[test]
fn an_animation_moves_on_when_ticked() {
    let mut t = Terminal::new(10, 2, "").unwrap();
    t.resize(10, 2, 8, 16);
    assert_eq!(t.tick_animations(0), None, "nothing playing");
    // A red pixel, then a blue frame shown 100 ms after, looping.
    t.feed(b"\x1b_Ga=T,f=24,s=1,v=1,i=3,q=2;/wAA\x1b\\");
    t.feed(b"\x1b_Ga=f,i=3,f=24,s=1,v=1,z=100,q=2;AAD/\x1b\\");
    t.feed(b"\x1b_Ga=a,i=3,s=3,v=1,q=2\x1b\\");
    let first = t.frame().placements[0].generation;
    let next = t.tick_animations(1000).expect("playing");
    assert!(next > 1000, "{next}");
    let mut now = next;
    for _ in 0..4 {
        if t.picture(3).unwrap().rgba == [0, 0, 255, 255] {
            break;
        }
        now = t.tick_animations(now).unwrap_or(now + 100);
    }
    assert_eq!(
        t.picture(3).unwrap().rgba,
        [0, 0, 255, 255],
        "the blue frame"
    );
    assert_ne!(t.frame().placements[0].generation, first);
}

fn base64(bytes: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}
