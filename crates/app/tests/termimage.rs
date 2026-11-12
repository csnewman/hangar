use base64::Engine as _;
use hangar_app::termimage::{Extract, Format, Piece, Progress, header, sixel};
use hangar_app::terminal::Screen;

/// A PNG of the size given, every pixel red.
fn png(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(w, h, image::Rgba([255, 0, 0, 255]));
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn text(pieces: &[Piece]) -> Vec<u8> {
    pieces
        .iter()
        .filter_map(|p| match p {
            Piece::Text(t) => Some(t.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

#[test]
fn text_and_other_sequences_pass_through_whatever_the_chunks() {
    let mut x = Extract::default();
    let input =
        b"hi \x1b[1mbold\x1b[0m \x1b]0;title\x07 \x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\ \x1bPq";
    // Byte by byte, as a slow connection gives it.
    let mut out = vec![];
    for b in input.iter().take(input.len() - 3) {
        out.extend(x.split(&[*b]));
    }
    assert_eq!(text(&out), &input[..input.len() - 3]);
    // A DCS that is not sixel is passed on whole once it ends.
    let mut x = Extract::default();
    let out = x.split(b"\x1bP=1s\x1b\\after");
    assert_eq!(text(&out), b"\x1bP=1s\x1b\\after");
}

#[test]
fn progress_is_taken_out_in_order_with_the_text() {
    let mut x = Extract::default();
    let out = x.split(b"a\x1b]9;4;1;42\x07b\x1b]9;4;3\x1b\\c\x1b]9;4;0\x07");
    assert_eq!(
        out,
        [
            Piece::Text(b"a".to_vec()),
            Piece::Progress(Progress {
                state: 1,
                value: 42
            }),
            Piece::Text(b"b".to_vec()),
            Piece::Progress(Progress { state: 3, value: 0 }),
            Piece::Text(b"c".to_vec()),
            Piece::Progress(Progress { state: 0, value: 0 }),
        ]
    );
}

#[test]
fn iterm_and_kitty_pictures_are_read_with_their_sizes() {
    let image = png(30, 20);
    let mut x = Extract::default();
    let seq = format!(
        "\x1b]1337;File=name=eA==;size={};inline=1:{}\x07",
        image.len(),
        b64(&image)
    );
    let out = x.split(seq.as_bytes());
    let [Piece::Picture(p)] = &out[..] else {
        panic!("{out:?}")
    };
    assert_eq!((p.format, p.width, p.height), (Format::Png, 30, 20));
    // Not inline: a download, not shown, and not passed on.
    let seq = format!("\x1b]1337;File=name=eA==:{}\x07", b64(&image));
    assert!(x.split(seq.as_bytes()).is_empty());

    // Kitty: raw RGB in two chunks, shown, answered as its ID asks.
    let raw = [10u8, 20, 30].repeat(4);
    let data = b64(&raw);
    let (a, b) = data.split_at(4);
    let out = [
        x.split(format!("\x1b_Ga=T,f=24,s=2,v=2,i=7,m=1;{a}\x1b\\").as_bytes()),
        x.split(format!("\x1b_Gm=0;{b}\x1b\\").as_bytes()),
    ]
    .concat();
    let [Piece::Picture(p), Piece::Reply(r)] = &out[..] else {
        panic!("{out:?}")
    };
    assert_eq!((p.format, p.width, p.height), (Format::Png, 2, 2));
    assert_eq!(r, b"\x1b_Gi=7;OK\x1b\\");
    // A query is answered and shows nothing; quiet asks no answer.
    assert_eq!(
        x.split(format!("\x1b_Ga=q,i=31,s=1,v=1,f=24;{}\x1b\\", b64(&[0, 0, 0])).as_bytes()),
        [Piece::Reply(b"\x1b_Gi=31;OK\x1b\\".to_vec())]
    );
    let out = x.split(format!("\x1b_Ga=T,f=100,q=2;{}\x1b\\", b64(&png(4, 4))).as_bytes());
    assert!(matches!(&out[..], [Piece::Picture(_)]));
    // Sent to keep, then shown by its ID.
    assert_eq!(
        x.split(format!("\x1b_Ga=t,f=100,i=3,q=1;{}\x1b\\", b64(&png(5, 6))).as_bytes()),
        []
    );
    let out = x.split(b"\x1b_Ga=p,i=3,q=1\x1b\\");
    let [Piece::Picture(p)] = &out[..] else {
        panic!("{out:?}")
    };
    assert_eq!((p.width, p.height), (5, 6));
}

#[test]
fn sixel_is_drawn_with_its_colours() {
    // Colour 1 red; a band of 3 columns, the middle one repeated, then a
    // second band one column wide.
    let p = sixel(b"#1;2;100;0;0#1~!2~-~").unwrap();
    assert_eq!((p.width, p.height), (3, 12));
    let img = image::load_from_memory(&p.bytes).unwrap().to_rgba8();
    assert_eq!(img.get_pixel(1, 3).0, [255, 0, 0, 255]);
    assert_eq!(img.get_pixel(0, 8).0, [255, 0, 0, 255]);
    // What no sixel set is clear.
    assert_eq!(img.get_pixel(2, 8).0[3], 0);
    let mut x = Extract::default();
    assert!(matches!(
        &x.split(b"\x1bP0;1;0q\"1;1;3;6#1~~~\x1b\\")[..],
        [Piece::Picture(_)]
    ));
}

#[test]
fn headers_say_a_pictures_format_and_size() {
    assert_eq!(header(&png(7, 9)), Some((Format::Png, 7, 9)));
    let gif = [b"GIF89a".as_slice(), &[10, 0, 4, 0]].concat();
    assert_eq!(header(&gif), Some((Format::Gif, 10, 4)));
    let jpeg = [
        &[0xff, 0xd8, 0xff, 0xe0, 0, 4, 0, 0][..],
        &[0xff, 0xc0, 0, 17, 8, 0, 48, 0, 64, 3][..],
        &[0; 8][..],
    ]
    .concat();
    assert_eq!(header(&jpeg), Some((Format::Jpeg, 64, 48)));
    assert_eq!(header(b"not a picture"), None);
}

#[test]
fn an_anchor_is_found_where_it_was_written_and_moves_with_the_text() {
    let mut s = Screen::new(20, 5);
    s.feed(b"one\r\n  \x1b]8;;hangar-picture:4\x1b\\ \x1b]8;;\x1b\\\r\n");
    assert_eq!(s.anchors("hangar-picture:", 0), [(1, 2, "4".to_string())]);
    // Three more lines scroll it up one, off the top's edge it is found
    // only when asked to look above.
    s.feed(b"a\r\nb\r\nc\r\nd\r\n");
    assert_eq!(s.anchors("hangar-picture:", 0), []);
    assert_eq!(s.anchors("hangar-picture:", 3), [(-1, 2, "4".to_string())]);
    // It is not a link to follow.
    assert_eq!(s.link_at(0, 0), None);
}
