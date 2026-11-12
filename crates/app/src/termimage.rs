//! What programs write to a terminal besides text, which alacritty_terminal
//! does not keep: the progress they report (OSC 9;4, as Windows Terminal,
//! iTerm2 and the web's terminals show it), the directory the shell is in
//! (OSC 7, and systemd's OSC 3008 context), and the pictures they draw, in
//! sixel (DCS ... q), iTerm2's inline images (OSC 1337;File=) and the kitty
//! graphics protocol (APC G). `Extract` takes them out of the output as it
//! arrives, in order with the text around them, and passes everything else
//! through untouched.

use std::collections::HashMap;

use base64::Engine as _;

/// A program's progress: its state (0 none, 1 going, 2 failed, 3 going
/// with no measure, 4 paused) and how far, out of 100.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub state: u8,
    pub value: u8,
}

/// A picture to show at the cursor: its encoded bytes (PNG, JPEG, GIF or
/// WebP) and its size in pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    pub format: Format,
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
    Gif,
    Webp,
}

/// A piece of the output, in the order it came.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    Text(Vec<u8>),
    Progress(Progress),
    Picture(Picture),
    /// What the terminal answers the program: kitty's replies.
    Reply(Vec<u8>),
    /// The directory the shell says it is in.
    Directory(String),
}

/// Where the scanner is.
#[derive(Debug, Default)]
enum At {
    #[default]
    Text,
    /// After ESC.
    Escape,
    /// Inside a string sequence (OSC, APC or DCS) introduced by the byte
    /// given, and whether its last byte was ESC.
    In(u8, bool),
}

/// The most a sequence is let grow to before it is dropped.
const LIMIT: usize = 64 << 20;

/// Takes pictures and progress out of a terminal's output.
#[derive(Debug, Default)]
pub struct Extract {
    at: At,
    body: Vec<u8>,
    /// A kitty image being sent in chunks: its keys and its data so far.
    kitty: Option<(HashMap<String, String>, Vec<u8>)>,
    /// Kitty images sent to be shown later, by ID.
    stored: HashMap<u32, Picture>,
}

impl Extract {
    /// The pieces of the output given, after what came before; a sequence
    /// cut off at the end waits for the rest.
    pub fn split(&mut self, input: &[u8]) -> Vec<Piece> {
        let mut out = Vec::new();
        let mut text = Vec::new();
        for &b in input {
            match self.at {
                At::Text => {
                    if b == 0x1b {
                        self.at = At::Escape;
                    } else {
                        text.push(b);
                    }
                }
                At::Escape => match b {
                    b']' | b'_' | b'P' => {
                        self.at = At::In(b, false);
                        self.body.clear();
                    }
                    0x1b => text.push(0x1b),
                    _ => {
                        text.extend([0x1b, b]);
                        self.at = At::Text;
                    }
                },
                At::In(kind, esc) => {
                    let end = (esc && b == b'\\') || (kind == b']' && b == 0x07);
                    if end {
                        if esc {
                            self.body.pop();
                        }
                        let body = std::mem::take(&mut self.body);
                        let st: &[u8] = if b == 0x07 { b"\x07" } else { b"\x1b\\" };
                        match self.take(kind, &body) {
                            Some(pieces) => {
                                if !text.is_empty() {
                                    out.push(Piece::Text(std::mem::take(&mut text)));
                                }
                                out.extend(pieces);
                            }
                            None => {
                                text.extend([0x1b, kind]);
                                text.extend(&body);
                                text.extend(st);
                            }
                        }
                        self.at = At::Text;
                    } else {
                        if self.body.len() < LIMIT {
                            self.body.push(b);
                        }
                        self.at = At::In(kind, b == 0x1b);
                    }
                }
            }
        }
        if !text.is_empty() {
            out.push(Piece::Text(text));
        }
        out
    }

    /// What a sequence carried, when it is one taken out; None passes it
    /// on as it came.
    fn take(&mut self, kind: u8, body: &[u8]) -> Option<Vec<Piece>> {
        match kind {
            b']' => {
                if let Some(rest) = body.strip_prefix(b"9;4;") {
                    return Some(progress(rest).map(Piece::Progress).into_iter().collect());
                }
                if let Some(rest) = body.strip_prefix(b"7;") {
                    return Some(
                        file_url_path(rest)
                            .map(Piece::Directory)
                            .into_iter()
                            .collect(),
                    );
                }
                if let Some(rest) = body.strip_prefix(b"3008;") {
                    return Some(
                        context_cwd(rest)
                            .map(Piece::Directory)
                            .into_iter()
                            .collect(),
                    );
                }
                let rest = body.strip_prefix(b"1337;File=")?;
                Some(iterm(rest).map(Piece::Picture).into_iter().collect())
            }
            b'_' => {
                let rest = body.strip_prefix(b"G")?;
                Some(self.kitty(rest))
            }
            b'P' => {
                let q = body
                    .iter()
                    .position(|b| !(b.is_ascii_digit() || *b == b';'))?;
                if body[q] != b'q' {
                    return None;
                }
                Some(
                    sixel(&body[q + 1..])
                        .map(Piece::Picture)
                        .into_iter()
                        .collect(),
                )
            }
            _ => None,
        }
    }

    /// A kitty graphics command: an image sent (in chunks), shown at once or
    /// later, or the program asking whether pictures are drawn; answered as
    /// it asks.
    fn kitty(&mut self, body: &[u8]) -> Vec<Piece> {
        let (keys, data) = match body.iter().position(|b| *b == b';') {
            Some(i) => (&body[..i], &body[i + 1..]),
            None => (body, &b""[..]),
        };
        let keys: HashMap<String, String> = String::from_utf8_lossy(keys)
            .split(',')
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let more = keys.get("m").is_some_and(|m| m == "1");
        // A chunk after the first carries little more than m.
        let (keys, data) = match self.kitty.take() {
            Some((first, mut sofar)) => {
                sofar.extend_from_slice(data);
                (first, sofar)
            }
            None => (keys, data.to_vec()),
        };
        if more {
            self.kitty = Some((keys, data));
            return vec![];
        }
        let get = |k: &str| keys.get(k).map(String::as_str);
        let id: Option<u32> = get("i").and_then(|i| i.parse().ok());
        let quiet = get("q").and_then(|q| q.parse::<u8>().ok()).unwrap_or(0);
        let answer = |ok: Result<(), &str>| -> Vec<Piece> {
            let Some(id) = id else { return vec![] };
            match ok {
                Ok(()) if quiet == 0 => {
                    vec![Piece::Reply(format!("\x1b_Gi={id};OK\x1b\\").into_bytes())]
                }
                Err(e) if quiet < 2 => {
                    vec![Piece::Reply(format!("\x1b_Gi={id};{e}\x1b\\").into_bytes())]
                }
                _ => vec![],
            }
        };
        let action = get("a").unwrap_or("t");
        match action {
            "p" => match id.and_then(|i| self.stored.get(&i)).cloned() {
                Some(p) => [vec![Piece::Picture(p)], answer(Ok(()))].concat(),
                None => answer(Err("ENOENT:no such image")),
            },
            "d" => vec![],
            "t" | "T" | "q" => {
                if get("t").is_some_and(|t| t != "d") {
                    return answer(Err("EINVAL:only direct transmission"));
                }
                if get("o").is_some() {
                    return answer(Err("EINVAL:compression not supported"));
                }
                let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(&data) else {
                    return answer(Err("EINVAL:bad base64"));
                };
                let size = |k: &str| get(k).and_then(|v| v.parse::<u32>().ok());
                let picture = match get("f").unwrap_or("32") {
                    "100" => encoded(raw),
                    f @ ("24" | "32") => {
                        let n = if f == "24" { 3 } else { 4 };
                        size("s")
                            .zip(size("v"))
                            .and_then(|(w, h)| rgba(w, h, &raw, n))
                    }
                    _ => None,
                };
                let Some(picture) = picture else {
                    return answer(Err("EINVAL:could not read the image"));
                };
                match action {
                    "q" => answer(Ok(())),
                    "t" => {
                        if let Some(i) = id {
                            self.stored.insert(i, picture);
                        }
                        answer(Ok(()))
                    }
                    _ => [vec![Piece::Picture(picture)], answer(Ok(()))].concat(),
                }
            }
            _ => answer(Err("EINVAL:unknown action")),
        }
    }
}

/// OSC 7's directory: the path of a file:// URL, its %-escapes decoded.
fn file_url_path(b: &[u8]) -> Option<String> {
    let url = std::str::from_utf8(b).ok()?.strip_prefix("file://")?;
    let path = &url[url.find('/')?..];
    unescape(path, b'%', 1)
}

/// The cwd field of systemd's OSC 3008 context, whose fields are key=value
/// pairs between semicolons, a value's own special bytes as \xNN.
fn context_cwd(b: &[u8]) -> Option<String> {
    let fields = std::str::from_utf8(b).ok()?;
    let cwd = fields.split(';').find_map(|f| f.strip_prefix("cwd="))?;
    unescape(cwd, b'\\', 2)
}

/// Text with each escaped byte -- `lead`, `skip` bytes of its own, then two
/// hex digits -- put back.
fn unescape(s: &str, lead: u8, skip: usize) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == lead
            && let Some(hex) = s.get(i + skip..i + skip + 2)
            && let Ok(v) = u8::from_str_radix(hex, 16)
        {
            out.push(v);
            i += skip + 2;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// OSC 9;4's state and value: "1;40", "3", "0".
fn progress(rest: &[u8]) -> Option<Progress> {
    let text = std::str::from_utf8(rest).ok()?;
    let mut parts = text.split(';');
    let state: u8 = parts.next()?.trim().parse().ok()?;
    let value: u8 = parts
        .next()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0)
        .min(100);
    (state <= 4).then_some(Progress { state, value })
}

/// iTerm2's inline image: "inline=1;size=N:BASE64"; one not inline is a
/// download, which is not shown.
fn iterm(rest: &[u8]) -> Option<Picture> {
    let colon = rest.iter().position(|b| *b == b':')?;
    let args = String::from_utf8_lossy(&rest[..colon]);
    let inline = args.split(';').any(|a| a.trim() == "inline=1");
    if !inline {
        return None;
    }
    let raw = base64::engine::general_purpose::STANDARD
        .decode(&rest[colon + 1..])
        .ok()?;
    encoded(raw)
}

/// An encoded picture, its format and size read from its header.
pub fn encoded(bytes: Vec<u8>) -> Option<Picture> {
    let (format, width, height) = header(&bytes)?;
    Some(Picture {
        format,
        bytes,
        width,
        height,
    })
}

/// A picture's format and size, from the start of its bytes.
pub fn header(b: &[u8]) -> Option<(Format, u32, u32)> {
    let be32 =
        |i: usize| -> Option<u32> { Some(u32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?)) };
    let le16 = |i: usize| -> Option<u32> {
        Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?) as u32)
    };
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some((Format::Png, be32(16)?, be32(20)?));
    }
    if b.starts_with(b"GIF8") {
        return Some((Format::Gif, le16(6)?, le16(8)?));
    }
    if b.starts_with(b"RIFF") && b.get(8..12) == Some(b"WEBP") {
        return match b.get(12..16)? {
            b"VP8X" => {
                let le24 = |i: usize| -> Option<u32> {
                    let s = b.get(i..i + 3)?;
                    Some(s[0] as u32 | (s[1] as u32) << 8 | (s[2] as u32) << 16)
                };
                Some((Format::Webp, le24(24)? + 1, le24(27)? + 1))
            }
            b"VP8 " => Some((Format::Webp, le16(26)? & 0x3fff, le16(28)? & 0x3fff)),
            b"VP8L" => {
                let s = b.get(21..25)?;
                let bits = u32::from_le_bytes(s.try_into().ok()?);
                Some((
                    Format::Webp,
                    (bits & 0x3fff) + 1,
                    ((bits >> 14) & 0x3fff) + 1,
                ))
            }
            _ => None,
        };
    }
    if b.starts_with(&[0xff, 0xd8]) {
        // The first start-of-frame marker says the size.
        let mut i = 2;
        while i + 9 < b.len() {
            if b[i] != 0xff {
                i += 1;
                continue;
            }
            let marker = b[i + 1];
            let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
            if matches!(marker, 0xc0..=0xcf) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
                let h = u16::from_be_bytes([b[i + 5], b[i + 6]]) as u32;
                let w = u16::from_be_bytes([b[i + 7], b[i + 8]]) as u32;
                return Some((Format::Jpeg, w, h));
            }
            i += 2 + len;
        }
    }
    None
}

/// Raw pixels, n bytes each (RGB or RGBA), as a PNG.
fn rgba(width: u32, height: u32, raw: &[u8], n: usize) -> Option<Picture> {
    if width == 0 || height == 0 || raw.len() < (width * height) as usize * n {
        return None;
    }
    let pixels: Vec<u8> = if n == 4 {
        raw[..(width * height * 4) as usize].to_vec()
    } else {
        raw.chunks(3)
            .take((width * height) as usize)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect()
    };
    png(width, height, pixels)
}

/// RGBA pixels as a PNG picture.
fn png(width: u32, height: u32, pixels: Vec<u8>) -> Option<Picture> {
    let img = image::RgbaImage::from_raw(width, height, pixels)?;
    let mut bytes = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut bytes),
        image::ImageFormat::Png,
    )
    .ok()?;
    Some(Picture {
        format: Format::Png,
        bytes,
        width,
        height,
    })
}

/// The largest sixel picture drawn, each way.
const SIXEL_MAX: usize = 4096;

/// A sixel picture's data (after its "q"), drawn: its colours set by
/// "#n;2;r;g;b" (percentages) or chosen by "#n", "!n" repeating the next
/// sixel, "$" back to the start of the band and "-" down to the next; what
/// no sixel sets is clear.
pub fn sixel(data: &[u8]) -> Option<Picture> {
    let mut palette: Vec<[u8; 4]> = DEFAULT_SIXEL.to_vec();
    palette.resize(256, [0, 0, 0, 255]);
    let mut colour = 0usize;
    let (mut x, mut y) = (0usize, 0usize);
    let (mut width, mut height) = (0usize, 0usize);
    let mut pixels: Vec<[u8; 4]> = Vec::new();
    let mut stride = 0usize;
    let mut i = 0;
    let number = |i: &mut usize| -> Vec<usize> {
        let mut out = vec![];
        let mut n: Option<usize> = None;
        while *i < data.len() {
            let c = data[*i];
            if c.is_ascii_digit() {
                n = Some(
                    n.unwrap_or(0)
                        .saturating_mul(10)
                        .saturating_add((c - b'0') as usize),
                );
            } else if c == b';' {
                out.push(n.take().unwrap_or(0));
            } else {
                break;
            }
            *i += 1;
        }
        out.push(n.unwrap_or(0));
        out
    };
    let put = |x: usize, y: usize, c: [u8; 4], pixels: &mut Vec<[u8; 4]>, stride: &mut usize| {
        if x >= SIXEL_MAX || y >= SIXEL_MAX {
            return;
        }
        if x >= *stride {
            // Wider: the rows laid out again at the new width.
            let wide = (x + 1).next_power_of_two().min(SIXEL_MAX);
            let rows = pixels.len().checked_div(*stride).unwrap_or(0);
            let mut grown = vec![[0u8; 4]; wide * rows];
            for r in 0..rows {
                grown[r * wide..r * wide + *stride]
                    .copy_from_slice(&pixels[r * *stride..(r + 1) * *stride]);
            }
            *pixels = grown;
            *stride = wide;
        }
        let rows = pixels.len() / *stride;
        if y >= rows {
            pixels.resize(*stride * (y + 1), [0; 4]);
        }
        pixels[y * *stride + x] = c;
    };
    let mut repeat = 1usize;
    while i < data.len() {
        let c = data[i];
        match c {
            b'"' => {
                i += 1;
                let p = number(&mut i);
                // The picture's size, when given, sets its least extent.
                if let (Some(&w), Some(&h)) = (p.get(2), p.get(3)) {
                    width = width.max(w.min(SIXEL_MAX));
                    height = height.max(h.min(SIXEL_MAX));
                }
                continue;
            }
            b'#' => {
                i += 1;
                let p = number(&mut i);
                colour = p[0].min(255);
                if p.len() >= 5 {
                    let pct = |v: usize| (v.min(100) * 255 / 100) as u8;
                    palette[colour] = match p[1] {
                        1 => hls(p[2], p[3], p[4]),
                        _ => [pct(p[2]), pct(p[3]), pct(p[4]), 255],
                    };
                }
                continue;
            }
            b'!' => {
                i += 1;
                repeat = number(&mut i)[0].clamp(1, SIXEL_MAX);
                continue;
            }
            b'$' => x = 0,
            b'-' => {
                x = 0;
                y += 6;
            }
            0x3f..=0x7e => {
                let bits = c - 0x3f;
                for _ in 0..repeat {
                    for b in 0..6 {
                        if bits & (1 << b) != 0 {
                            put(x, y + b, palette[colour], &mut pixels, &mut stride);
                            height = height.max(y + b + 1);
                        }
                    }
                    x += 1;
                    width = width.max(x);
                }
                repeat = 1;
            }
            _ => {}
        }
        i += 1;
    }
    if width == 0 || height == 0 || width > SIXEL_MAX || height > SIXEL_MAX {
        return None;
    }
    let mut out = Vec::with_capacity(width * height * 4);
    for row in 0..height {
        for col in 0..width {
            let p = if col < stride {
                pixels.get(row * stride + col).copied().unwrap_or([0; 4])
            } else {
                [0; 4]
            };
            out.extend(p);
        }
    }
    png(width as u32, height as u32, out)
}

/// A sixel colour given as hue, lightness and saturation.
fn hls(h: usize, l: usize, s: usize) -> [u8; 4] {
    let (h, l, s) = (
        ((h + 240) % 360) as f32,
        l.min(100) as f32 / 100.,
        s.min(100) as f32 / 100.,
    );
    let c = (1. - (2. * l - 1.).abs()) * s;
    let x = c * (1. - ((h / 60.) % 2. - 1.).abs());
    let m = l - c / 2.;
    let (r, g, b) = match h as u32 {
        0..=59 => (c, x, 0.),
        60..=119 => (x, c, 0.),
        120..=179 => (0., c, x),
        180..=239 => (0., x, c),
        240..=299 => (x, 0., c),
        _ => (c, 0., x),
    };
    let v = |f: f32| ((f + m) * 255.).round().clamp(0., 255.) as u8;
    [v(r), v(g), v(b), 255]
}

/// The VT340's sixteen colours, which sixel starts with.
const DEFAULT_SIXEL: [[u8; 4]; 16] = [
    [0, 0, 0, 255],
    [51, 51, 204, 255],
    [204, 36, 36, 255],
    [51, 204, 51, 255],
    [204, 51, 204, 255],
    [51, 204, 204, 255],
    [204, 204, 51, 255],
    [120, 120, 120, 255],
    [69, 69, 69, 255],
    [87, 87, 153, 255],
    [153, 69, 69, 255],
    [87, 153, 87, 255],
    [153, 87, 153, 255],
    [87, 153, 153, 255],
    [153, 153, 87, 255],
    [204, 204, 204, 255],
];
