//! A VNC (RFB 3.8) client for an environment's Linux desktop, over the
//! client API's desktop WebSocket (internal/clientapi/openapi.yaml) on a
//! thread of its own. The guest's wayvnc asks no password: reaching the
//! socket is the permission. Pixels come as Raw, CopyRect or ZRLE into one
//! framebuffer, BGRA as the window draws it; the desktop's size changing
//! comes as DesktopSize. Pointer and key events and the clipboard go back.

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;

use tungstenite::Message;

/// The desktop's pixels: BGRA, a row after another, and a count of the
/// changes, so the window draws them once each.
#[derive(Debug, Default)]
pub struct Framebuffer {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
    pub version: u64,
}

impl Framebuffer {
    fn resize(&mut self, width: usize, height: usize) {
        self.width = width;
        self.height = height;
        self.pixels = vec![0; width * height * 4];
        self.version += 1;
    }

    /// Writes a rectangle of BGRA pixels, those outside the framebuffer
    /// left out.
    pub fn put(&mut self, x: usize, y: usize, w: usize, h: usize, bgra: &[u8]) {
        for row in 0..h {
            let fy = y + row;
            if fy >= self.height || x >= self.width {
                continue;
            }
            let n = w.min(self.width - x);
            let from = &bgra[row * w * 4..row * w * 4 + n * 4];
            let at = (fy * self.width + x) * 4;
            self.pixels[at..at + n * 4].copy_from_slice(from);
        }
    }

    /// Copies a rectangle from elsewhere in the framebuffer, as CopyRect
    /// asks.
    pub fn copy(&mut self, sx: usize, sy: usize, x: usize, y: usize, w: usize, h: usize) {
        if sx + w > self.width || sy + h > self.height || x + w > self.width || y + h > self.height
        {
            return;
        }
        let mut rect = vec![0u8; w * h * 4];
        for row in 0..h {
            let at = ((sy + row) * self.width + sx) * 4;
            rect[row * w * 4..(row + 1) * w * 4].copy_from_slice(&self.pixels[at..at + w * 4]);
        }
        self.put(x, y, w, h, &rect);
    }
}

/// What the desktop says.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Connected: the desktop's name and size.
    Ready {
        name: String,
        width: usize,
        height: usize,
    },
    /// The framebuffer changed.
    Frame,
    /// Text copied on the desktop.
    Clipboard(String),
    Bell,
    /// The connection ended, and why.
    Ended(String),
}

/// What is sent to the desktop.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Where the pointer is, in the framebuffer's pixels, and which buttons
    /// are down (1 left, 2 middle, 4 right, 8 and 16 the wheel up and down).
    Pointer {
        x: u16,
        y: u16,
        buttons: u8,
    },
    /// A key, by its X keysym, down or up.
    Key {
        keysym: u32,
        down: bool,
    },
    /// Text for the desktop's clipboard.
    Paste(String),
    Close,
}

/// A desktop's connection, closed when this is dropped.
pub struct Connection {
    tx: Sender<Command>,
}

impl Connection {
    /// Connects, calling events with what the desktop says (from the
    /// connection's thread) until it returns false.
    pub fn open(
        url: String,
        auth: String,
        fb: Arc<Mutex<Framebuffer>>,
        events: impl Fn(Event) -> bool + Send + 'static,
    ) -> Connection {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let why = match run(&url, &auth, &rx, &fb, &events) {
                Ok(()) => return,
                Err(e) => e,
            };
            events(Event::Ended(why));
        });
        Connection { tx }
    }

    pub fn send(&self, c: Command) {
        let _ = self.tx.send(c);
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Close);
    }
}

/// The socket's bytes as a stream, read a message at a time; what the
/// window asks is sent while waiting for more.
struct Wire<'a> {
    ws: crate::ws::Socket,
    buf: Vec<u8>,
    at: usize,
    rx: &'a Receiver<Command>,
    live: crate::ws::Liveness,
}

/// How the wire's reading ended, short of an error: the window closed it.
const CLOSED: &str = "closed";

impl Wire<'_> {
    /// Sends what the window has asked, saying whether it asked to close.
    fn commands(&mut self) -> Result<(), String> {
        loop {
            let msg = match self.rx.try_recv() {
                Ok(Command::Pointer { x, y, buttons }) => {
                    let mut b = vec![5, buttons];
                    b.extend(x.to_be_bytes());
                    b.extend(y.to_be_bytes());
                    b
                }
                Ok(Command::Key { keysym, down }) => {
                    let mut b = vec![4, u8::from(down), 0, 0];
                    b.extend(keysym.to_be_bytes());
                    b
                }
                Ok(Command::Paste(text)) => {
                    // RFB's clipboard is Latin-1: what it cannot hold is
                    // left out.
                    let latin: Vec<u8> = text
                        .chars()
                        .filter_map(|c| u8::try_from(u32::from(c)).ok())
                        .collect();
                    let mut b = vec![6, 0, 0, 0];
                    b.extend((latin.len() as u32).to_be_bytes());
                    b.extend(latin);
                    b
                }
                Ok(Command::Close) | Err(TryRecvError::Disconnected) => {
                    let _ = self.ws.close(None);
                    let _ = self.ws.flush();
                    return Err(CLOSED.into());
                }
                Err(TryRecvError::Empty) => break,
            };
            self.write(&msg)?;
        }
        Ok(())
    }

    fn write(&mut self, b: &[u8]) -> Result<(), String> {
        self.ws
            .send(Message::Binary(b.to_vec().into()))
            .map_err(|e| e.to_string())
    }

    /// Waits until n bytes are there.
    fn fill(&mut self, n: usize) -> Result<(), String> {
        while self.buf.len() - self.at < n {
            self.commands()?;
            self.live.check(&mut self.ws)?;
            let _ = self.ws.flush();
            match self.ws.read() {
                Ok(Message::Binary(b)) => {
                    self.live.heard();
                    if self.at > 0 && self.at == self.buf.len() {
                        self.buf.clear();
                        self.at = 0;
                    }
                    self.buf.extend_from_slice(&b);
                }
                Ok(Message::Close(f)) => {
                    return Err(f
                        .map(|f| f.reason.to_string())
                        .filter(|r| !r.is_empty())
                        .unwrap_or_else(|| "the desktop closed the connection".into()));
                }
                Ok(_) => self.live.heard(),
                Err(e) if crate::ws::idle(&e) => {}
                Err(e) => return Err(e.to_string()),
            }
        }
        Ok(())
    }

    fn take(&mut self, n: usize) -> Result<Vec<u8>, String> {
        self.fill(n)?;
        let out = self.buf[self.at..self.at + n].to_vec();
        self.at += n;
        if self.at > 1 << 20 {
            self.buf.drain(..self.at);
            self.at = 0;
        }
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, String> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
}

/// The encodings asked for, most wanted first: ZRLE, CopyRect, Raw, and
/// DesktopSize (the desktop resized).
const ENCODINGS: [i32; 4] = [16, 1, 0, -223];

fn run(
    url: &str,
    auth: &str,
    rx: &Receiver<Command>,
    fb: &Mutex<Framebuffer>,
    events: &impl Fn(Event) -> bool,
) -> Result<(), String> {
    let ws = crate::ws::connect(url, auth).map_err(|e| match e {
        crate::ws::ConnectError::Refused(_, why) | crate::ws::ConnectError::Unreachable(why) => why,
    })?;
    let mut w = Wire {
        ws,
        buf: Vec::new(),
        at: 0,
        rx,
        live: crate::ws::Liveness::default(),
    };
    let closed = |e: String| if e == CLOSED { Ok(()) } else { Err(e) };
    match session(&mut w, fb, events) {
        Ok(()) => Ok(()),
        Err(e) => closed(e),
    }
}

fn session(
    w: &mut Wire,
    fb: &Mutex<Framebuffer>,
    events: &impl Fn(Event) -> bool,
) -> Result<(), String> {
    // The versions, then no security, as wayvnc offers for the guest.
    let version = w.take(12)?;
    if !version.starts_with(b"RFB ") {
        return Err("not a VNC server".into());
    }
    w.write(b"RFB 003.008\n")?;
    let n = w.u8()? as usize;
    if n == 0 {
        let len = w.u32()? as usize;
        return Err(String::from_utf8_lossy(&w.take(len)?).into_owned());
    }
    let types = w.take(n)?;
    if !types.contains(&1) {
        return Err("the desktop asks for a password, which Hangar's do not".into());
    }
    w.write(&[1])?;
    if w.u32()? != 0 {
        let len = w.u32()? as usize;
        return Err(String::from_utf8_lossy(&w.take(len)?).into_owned());
    }
    // Shared: other viewers stay connected.
    w.write(&[1])?;
    let (width, height) = (w.u16()? as usize, w.u16()? as usize);
    w.take(16)?;
    let len = w.u32()? as usize;
    let name = String::from_utf8_lossy(&w.take(len)?).into_owned();
    // 32 bits a pixel, little-endian, red at 16: BGRA in memory.
    let mut format = vec![0, 0, 0, 0, 32, 24, 0, 1];
    format.extend(255u16.to_be_bytes());
    format.extend(255u16.to_be_bytes());
    format.extend(255u16.to_be_bytes());
    format.extend([16, 8, 0, 0, 0, 0]);
    w.write(&format)?;
    let mut enc = vec![2, 0];
    enc.extend((ENCODINGS.len() as u16).to_be_bytes());
    for e in ENCODINGS {
        enc.extend(e.to_be_bytes());
    }
    w.write(&enc)?;
    fb.lock().unwrap().resize(width, height);
    if !events(Event::Ready {
        name,
        width,
        height,
    }) {
        return Ok(());
    }
    let mut zrle = flate2::Decompress::new(true);
    request(w, false, fb)?;
    loop {
        match w.u8()? {
            0 => {
                w.take(1)?;
                let rects = w.u16()?;
                for _ in 0..rects {
                    rect(w, fb, &mut zrle)?;
                }
                fb.lock().unwrap().version += 1;
                if !events(Event::Frame) {
                    return Ok(());
                }
                request(w, true, fb)?;
            }
            1 => {
                w.take(3)?;
                let n = w.u16()? as usize;
                w.take(n * 6)?;
            }
            2 => {
                if !events(Event::Bell) {
                    return Ok(());
                }
            }
            3 => {
                w.take(3)?;
                let len = w.u32()? as usize;
                let text: String = w.take(len)?.into_iter().map(char::from).collect();
                if !events(Event::Clipboard(text)) {
                    return Ok(());
                }
            }
            t => {
                return Err(format!(
                    "the desktop sent a message of a kind not known ({t})"
                ));
            }
        }
    }
}

/// Asks for what has changed (or all of it) on the whole desktop.
fn request(w: &mut Wire, incremental: bool, fb: &Mutex<Framebuffer>) -> Result<(), String> {
    let (width, height) = {
        let f = fb.lock().unwrap();
        (f.width as u16, f.height as u16)
    };
    let mut b = vec![3, u8::from(incremental), 0, 0, 0, 0];
    b.extend(width.to_be_bytes());
    b.extend(height.to_be_bytes());
    w.write(&b)
}

/// Reads one rectangle of an update into the framebuffer.
fn rect(
    w: &mut Wire,
    fb: &Mutex<Framebuffer>,
    zrle: &mut flate2::Decompress,
) -> Result<(), String> {
    let (x, y) = (w.u16()? as usize, w.u16()? as usize);
    let (rw, rh) = (w.u16()? as usize, w.u16()? as usize);
    let encoding = w.u32()? as i32;
    match encoding {
        0 => {
            let mut px = w.take(rw * rh * 4)?;
            opaque(&mut px);
            fb.lock().unwrap().put(x, y, rw, rh, &px);
        }
        1 => {
            let (sx, sy) = (w.u16()? as usize, w.u16()? as usize);
            fb.lock().unwrap().copy(sx, sy, x, y, rw, rh);
        }
        16 => {
            let len = w.u32()? as usize;
            let data = w.take(len)?;
            let mut out = Vec::new();
            inflate(zrle, &data, &mut out)?;
            let px = zrle_tiles(&out, rw, rh)?;
            fb.lock().unwrap().put(x, y, rw, rh, &px);
        }
        -223 => fb.lock().unwrap().resize(rw, rh),
        e => {
            return Err(format!(
                "the desktop sent pixels in an encoding not asked for ({e})"
            ));
        }
    }
    Ok(())
}

/// Sets every pixel's unused fourth byte, which the window takes as alpha.
fn opaque(bgra: &mut [u8]) {
    for p in bgra.as_chunks_mut::<4>().0 {
        p[3] = 255;
    }
}

/// Inflates a ZRLE rectangle's data with the connection's one zlib stream.
fn inflate(z: &mut flate2::Decompress, data: &[u8], out: &mut Vec<u8>) -> Result<(), String> {
    let mut input = data;
    let mut chunk = vec![0u8; 64 << 10];
    loop {
        let (before_in, before_out) = (z.total_in(), z.total_out());
        z.decompress(input, &mut chunk, flate2::FlushDecompress::Sync)
            .map_err(|e| e.to_string())?;
        let used = (z.total_in() - before_in) as usize;
        let made = (z.total_out() - before_out) as usize;
        out.extend_from_slice(&chunk[..made]);
        input = &input[used..];
        if input.is_empty() && made < chunk.len() {
            return Ok(());
        }
        if used == 0 && made == 0 {
            return Err("the desktop's compressed pixels ended early".into());
        }
    }
}

/// A ZRLE rectangle's tiles, 64 pixels square, as BGRA: each raw, one
/// colour, a palette packed into bits, or runs of a colour or of a
/// palette's colours. A pixel is three bytes (B, G, R).
pub fn zrle_tiles(data: &[u8], width: usize, height: usize) -> Result<Vec<u8>, String> {
    let mut out = vec![0u8; width * height * 4];
    let mut r = Reader { data, at: 0 };
    for ty in (0..height).step_by(64) {
        for tx in (0..width).step_by(64) {
            let (tw, th) = (64.min(width - tx), 64.min(height - ty));
            let mut tile = vec![[0u8; 4]; tw * th];
            let sub = r.u8()?;
            match sub {
                0 => {
                    for p in tile.iter_mut() {
                        *p = r.pixel()?;
                    }
                }
                1 => {
                    let c = r.pixel()?;
                    tile.fill(c);
                }
                2..=16 => {
                    let palette: Vec<[u8; 4]> =
                        (0..sub).map(|_| r.pixel()).collect::<Result<_, _>>()?;
                    let bits = match sub {
                        2 => 1,
                        3..=4 => 2,
                        _ => 4,
                    };
                    for row in 0..th {
                        let mut byte = 0u8;
                        let mut left = 0;
                        for col in 0..tw {
                            if left == 0 {
                                byte = r.u8()?;
                                left = 8;
                            }
                            left -= bits;
                            let i = (byte >> left) & ((1 << bits) - 1);
                            tile[row * tw + col] = *palette
                                .get(i as usize)
                                .ok_or("a palette index past its end")?;
                        }
                    }
                }
                128 => {
                    let mut at = 0;
                    while at < tile.len() {
                        let c = r.pixel()?;
                        let n = r.run()?;
                        for p in tile.iter_mut().skip(at).take(n) {
                            *p = c;
                        }
                        at += n;
                    }
                }
                130..=255 => {
                    let palette: Vec<[u8; 4]> = (0..sub - 128)
                        .map(|_| r.pixel())
                        .collect::<Result<_, _>>()?;
                    let mut at = 0;
                    while at < tile.len() {
                        let i = r.u8()?;
                        let c = *palette
                            .get((i & 0x7f) as usize)
                            .ok_or("a palette index past its end")?;
                        let n = if i & 0x80 != 0 { r.run()? } else { 1 };
                        for p in tile.iter_mut().skip(at).take(n) {
                            *p = c;
                        }
                        at += n;
                    }
                }
                s => return Err(format!("a ZRLE tile of a kind not known ({s})")),
            }
            for row in 0..th {
                for col in 0..tw {
                    let at = ((ty + row) * width + tx + col) * 4;
                    out[at..at + 4].copy_from_slice(&tile[row * tw + col]);
                }
            }
        }
    }
    Ok(out)
}

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn u8(&mut self) -> Result<u8, String> {
        let b = *self.data.get(self.at).ok_or("ZRLE data ended early")?;
        self.at += 1;
        Ok(b)
    }

    /// A compressed pixel: three bytes, B G R, made opaque.
    fn pixel(&mut self) -> Result<[u8; 4], String> {
        let b = self
            .data
            .get(self.at..self.at + 3)
            .ok_or("ZRLE data ended early")?;
        self.at += 3;
        Ok([b[0], b[1], b[2], 255])
    }

    /// A run's length: bytes added up while they are 255, then one more.
    fn run(&mut self) -> Result<usize, String> {
        let mut n = 1usize;
        loop {
            let b = self.u8()?;
            n += b as usize;
            if b != 255 {
                return Ok(n);
            }
        }
    }
}

/// The X keysym for a key the window gives, by its name or its character.
pub fn keysym(key: &str, ch: Option<&str>) -> Option<u32> {
    let named = match key {
        "enter" => 0xff0d,
        "backspace" => 0xff08,
        "tab" => 0xff09,
        "escape" => 0xff1b,
        "delete" => 0xffff,
        "home" => 0xff50,
        "left" => 0xff51,
        "up" => 0xff52,
        "right" => 0xff53,
        "down" => 0xff54,
        "pageup" => 0xff55,
        "pagedown" => 0xff56,
        "end" => 0xff57,
        "insert" => 0xff63,
        "space" => 0x20,
        f if f.starts_with('f') && f.len() <= 3 => match f[1..].parse::<u32>() {
            Ok(n @ 1..=12) => 0xffbe + n - 1,
            _ => 0,
        },
        _ => 0,
    };
    if named != 0 {
        return Some(named);
    }
    let c = match ch.filter(|c| c.chars().count() == 1) {
        Some(c) => c.chars().next()?,
        None if key.chars().count() == 1 => key.chars().next()?,
        None => return None,
    };
    let cp = u32::from(c);
    Some(
        if (0x20..=0x7e).contains(&cp) || (0xa0..=0xff).contains(&cp) {
            cp
        } else {
            0x0100_0000 + cp
        },
    )
}

/// The keysyms of the modifiers held, pressed before a key and let go after.
pub const SHIFT: u32 = 0xffe1;
pub const CONTROL: u32 = 0xffe3;
pub const ALT: u32 = 0xffe9;
