//! Serves the user's files over HTTP, for the Code tab to show an HTML file
//! as a browser would, its styles, scripts and images beside it loaded as
//! they would be from the disk. It listens on the guest's loopback, which
//! Hangar reaches through the environment's numbered host (<port>-env<short>),
//! and answers only under a secret first segment of the path: the port is
//! open to anyone who can reach the environment's ports, and the files are
//! the user's. /<secret>/home/dev/site/index.html is
//! /home/dev/site/index.html; a folder is its index.html.

use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::OnceCell;

use super::paths::normalize;

/// The longest request head read.
const MAX_HEAD: usize = 16 << 10;

/// What a browser is told each file is, by its extension. Anything else is
/// sent as bytes to download, never as something to run.
fn content_type(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "txt" | "md" => "text/plain; charset=utf-8",
        "xml" => "application/xml; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        "pdf" => "application/pdf",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        _ => "application/octet-stream",
    }
}

pub struct Preview {
    secret: String,
    port: OnceCell<u16>,
}

impl Preview {
    pub fn new() -> Self {
        let mut b = [0u8; 24];
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            use std::io::Read;
            let _ = f.read_exact(&mut b);
        }
        Preview {
            secret: URL_SAFE_NO_PAD.encode(b),
            port: OnceCell::new(),
        }
    }

    /// Listens if the service is not already, and says where.
    pub async fn start(&self) -> std::io::Result<(u16, String)> {
        let port = self
            .port
            .get_or_try_init(|| async {
                let listener = TcpListener::bind("127.0.0.1:0").await?;
                let port = listener.local_addr()?.port();
                let secret = self.secret.clone();
                tokio::spawn(async move {
                    while let Ok((stream, _)) = listener.accept().await {
                        let secret = secret.clone();
                        tokio::spawn(async move {
                            let _ = serve(stream, &secret).await;
                        });
                    }
                });
                Ok::<u16, std::io::Error>(port)
            })
            .await?;
        Ok((*port, self.secret.clone()))
    }
}

enum Answer {
    Text(u16, String),
    File(PathBuf, u64),
}

async fn serve(mut stream: TcpStream, secret: &str) -> std::io::Result<()> {
    let mut head = Vec::new();
    let mut buf = [0u8; 4096];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut buf).await?;
        if n == 0 || head.len() > MAX_HEAD {
            return Ok(());
        }
        head.extend_from_slice(&buf[..n]);
    }
    let line = String::from_utf8_lossy(&head);
    let mut words = line.lines().next().unwrap_or("").split(' ');
    let method = words.next().unwrap_or("").to_owned();
    let target = words.next().unwrap_or("/").to_owned();
    let answer = answer(&target, secret).await;
    let (status, ctype, len, file) = match &answer {
        Answer::Text(s, body) => (*s, "text/plain; charset=utf-8", body.len() as u64, None),
        Answer::File(p, len) => (200, content_type(p), *len, Some(p)),
    };
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Error",
    };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {len}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(header.as_bytes()).await?;
    if method != "HEAD" {
        match (&answer, file) {
            (Answer::Text(_, body), _) => stream.write_all(body.as_bytes()).await?,
            (_, Some(p)) => {
                let mut f = tokio::fs::File::open(p).await?;
                tokio::io::copy(&mut f, &mut stream).await?;
            }
            _ => {}
        }
    }
    stream.shutdown().await
}

async fn answer(target: &str, secret: &str) -> Answer {
    let path = target.split(['?', '#']).next().unwrap_or("/");
    let (given, rest) = match path[1.min(path.len())..].find('/') {
        Some(i) => (&path[1..i + 1], &path[i + 1..]),
        None => (path.get(1..).unwrap_or(""), "/"),
    };
    if !same(given.as_bytes(), secret.as_bytes()) {
        return Answer::Text(404, "Not found.".into());
    }
    let Some(decoded) = percent_decode(rest) else {
        return Answer::Text(400, "Bad path.".into());
    };
    let mut file = normalize(Path::new(&decoded));
    let mut m = match tokio::fs::metadata(&file).await {
        Ok(m) => m,
        Err(_) => return Answer::Text(404, format!("{} does not exist.", file.display())),
    };
    if m.is_dir() {
        file = file.join("index.html");
        m = match tokio::fs::metadata(&file).await {
            Ok(m) => m,
            Err(_) => return Answer::Text(404, format!("{} does not exist.", file.display())),
        };
    }
    if !m.is_file() {
        return Answer::Text(404, format!("{} is not a file.", file.display()));
    }
    Answer::File(file, m.len())
}

/// Whether two secrets are the same, taking as long however much of them
/// matches.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

/// decodeURIComponent: %XX as bytes, which must make UTF-8.
fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}
