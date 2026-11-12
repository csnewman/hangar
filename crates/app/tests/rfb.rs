use std::io::Write;
use std::net::TcpListener;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use hangar_app::rfb::{Command, Connection, Event, Framebuffer, keysym, zrle_tiles};
use tungstenite::Message;

/// Bytes zlib-compressed as one stream, flushed.
fn zlib(raw: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    e.write_all(raw).unwrap();
    e.flush().unwrap();
    e.finish().unwrap()
}

#[test]
fn zrle_tiles_are_drawn_from_their_kinds() {
    // A 2x2 rectangle, one tile: a palette of two, packed a bit a pixel.
    let mut data = vec![2];
    data.extend([10, 20, 30, 40, 50, 60]);
    data.extend([0b0100_0000, 0b1000_0000]);
    let px = zrle_tiles(&data, 2, 2).unwrap();
    assert_eq!(&px[0..4], [10, 20, 30, 255]);
    assert_eq!(&px[4..8], [40, 50, 60, 255]);
    assert_eq!(&px[8..12], [40, 50, 60, 255]);
    assert_eq!(&px[12..16], [10, 20, 30, 255]);
    // Plain runs: three of one colour, one of another.
    let data = [128, 1, 2, 3, 2, 9, 8, 7, 0];
    let px = zrle_tiles(&data, 2, 2).unwrap();
    assert_eq!(&px[8..12], [1, 2, 3, 255]);
    assert_eq!(&px[12..16], [9, 8, 7, 255]);
    // A palette's runs: the first colour for 3, the second once.
    let data = [130, 1, 1, 1, 2, 2, 2, 0x80, 2, 1];
    let px = zrle_tiles(&data, 2, 2).unwrap();
    assert_eq!(&px[0..4], [1, 1, 1, 255]);
    assert_eq!(&px[12..16], [2, 2, 2, 255]);
    assert!(zrle_tiles(&[2, 1], 2, 2).is_err());
}

#[test]
fn keys_are_told_by_their_keysyms() {
    assert_eq!(keysym("a", Some("a")), Some(0x61));
    assert_eq!(keysym("a", Some("A")), Some(0x41));
    assert_eq!(keysym("enter", None), Some(0xff0d));
    assert_eq!(keysym("f5", None), Some(0xffc2));
    assert_eq!(keysym("e", Some("é")), Some(0xe9));
    assert_eq!(keysym("x", Some("€")), Some(0x0100_20ac));
    assert_eq!(keysym("c", None), Some(0x63));
    assert_eq!(keysym("unknownkey", None), None);
}

#[test]
fn a_framebuffer_takes_rectangles_and_copies() {
    let mut f = Framebuffer {
        width: 3,
        height: 2,
        pixels: vec![0; 24],
        version: 0,
    };
    f.put(1, 0, 2, 1, &[1, 1, 1, 255, 2, 2, 2, 255]);
    f.copy(1, 0, 0, 1, 2, 1);
    assert_eq!(&f.pixels[12..20], [1, 1, 1, 255, 2, 2, 2, 255]);
    // Past the edge is left out.
    f.put(2, 1, 2, 1, &[9, 9, 9, 255, 8, 8, 8, 255]);
    assert_eq!(&f.pixels[20..24], [9, 9, 9, 255]);
}

/// A VNC server over a WebSocket, as the client API's desktop socket is:
/// it greets, offers no security, says the desktop is 4x2, sends a raw
/// rectangle and a ZRLE one, then text for the clipboard, and hands back
/// what the client sends after.
#[test]
fn a_desktop_is_connected_to_drawn_and_driven() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (heard_tx, heard) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let (tcp, _) = listener.accept().unwrap();
        let mut ws = tungstenite::accept(tcp).unwrap();
        let mut send = |b: &[u8]| ws.send(Message::Binary(b.to_vec().into())).unwrap();
        send(b"RFB 003.008\n");
        drop(send);
        let mut got = Vec::new();
        let want =
            |ws: &mut tungstenite::WebSocket<std::net::TcpStream>, n: usize, got: &mut Vec<u8>| {
                while got.len() < n {
                    if let Message::Binary(b) = ws.read().unwrap() {
                        got.extend_from_slice(&b);
                    }
                }
                got.drain(..n).collect::<Vec<u8>>()
            };
        assert_eq!(want(&mut ws, 12, &mut got), b"RFB 003.008\n");
        ws.send(Message::Binary(vec![1, 1].into())).unwrap();
        assert_eq!(want(&mut ws, 1, &mut got), [1]);
        ws.send(Message::Binary(vec![0, 0, 0, 0].into())).unwrap();
        assert_eq!(want(&mut ws, 1, &mut got), [1]);
        let mut init = vec![0, 4, 0, 2];
        init.extend([0; 16]);
        init.extend(4u32.to_be_bytes());
        init.extend(b"test");
        ws.send(Message::Binary(init.into())).unwrap();
        // SetPixelFormat, SetEncodings (four), a full update asked.
        want(&mut ws, 20, &mut got);
        want(&mut ws, 4 + 16, &mut got);
        assert_eq!(want(&mut ws, 10, &mut got), [3, 0, 0, 0, 0, 0, 0, 4, 0, 2]);
        let mut update = vec![0, 0, 0, 2];
        // Raw: the top row, 4x1.
        update.extend([0, 0, 0, 0, 0, 4, 0, 1]);
        update.extend(0i32.to_be_bytes());
        for i in 0..4u8 {
            update.extend([i, i, i, 0]);
        }
        // ZRLE: the bottom row, one colour.
        update.extend([0, 0, 0, 1, 0, 4, 0, 1]);
        update.extend(16i32.to_be_bytes());
        let z = zlib(&[1, 7, 8, 9]);
        update.extend((z.len() as u32).to_be_bytes());
        update.extend(z);
        let mut cut = vec![3, 0, 0, 0];
        cut.extend(2u32.to_be_bytes());
        cut.extend(b"hi");
        update.extend(cut);
        ws.send(Message::Binary(update.into())).unwrap();
        // What the client sends next: an incremental update asked, then
        // the pointer and a key.
        assert_eq!(want(&mut ws, 10, &mut got)[1], 1);
        heard_tx.send(want(&mut ws, 6, &mut got)).unwrap();
        heard_tx.send(want(&mut ws, 8, &mut got)).unwrap();
        let _ = ws.read();
    });

    let fb = Arc::new(Mutex::new(Framebuffer::default()));
    let (tx, rx) = mpsc::channel();
    let conn = Connection::open(
        format!("ws://127.0.0.1:{port}/desktop"),
        "Bearer t".into(),
        fb.clone(),
        move |e| tx.send(e).is_ok(),
    );
    let next = || rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        next(),
        Event::Ready {
            name: "test".into(),
            width: 4,
            height: 2
        }
    );
    assert_eq!(next(), Event::Frame);
    {
        let f = fb.lock().unwrap();
        assert_eq!(&f.pixels[4..8], [1, 1, 1, 255]);
        assert_eq!(&f.pixels[16..20], [7, 8, 9, 255]);
        assert_eq!(&f.pixels[28..32], [7, 8, 9, 255]);
    }
    assert_eq!(next(), Event::Clipboard("hi".into()));
    conn.send(Command::Pointer {
        x: 3,
        y: 1,
        buttons: 1,
    });
    conn.send(Command::Key {
        keysym: 0x61,
        down: true,
    });
    assert_eq!(
        heard.recv_timeout(Duration::from_secs(5)).unwrap(),
        [5, 1, 0, 3, 0, 1]
    );
    assert_eq!(
        heard.recv_timeout(Duration::from_secs(5)).unwrap(),
        [4, 1, 0, 0, 0, 0, 0, 0x61]
    );
}
