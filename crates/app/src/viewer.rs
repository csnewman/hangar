//! The ways a file is shown other than as text, as the web editor's
//! viewers show it: a picture as itself; a PDF, sound or video by this
//! computer's own app; and any other file that is not text as its bytes,
//! sixteen a row with their offsets and the characters they are.

use std::ops::Range;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use gpui::{
    App, Context, FocusHandle, Focusable, ImageFormat, IntoElement, ParentElement, Render,
    SharedString, Styled, Window, div, img, prelude::*, px, uniform_list,
};
use gpui_component::{h_flex, v_flex};
use serde_json::json;

use crate::code::Code;
use crate::palette::{self, Palette};

/// How a file is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Text,
    Image,
    /// A PDF, sound or video: opened by this computer's own app.
    External,
    Hex,
    /// An HTML file, as a browser shows it, from the environment's preview
    /// server.
    Browser,
}

impl View {
    /// What the Open With menu calls it.
    pub fn name(self) -> &'static str {
        match self {
            View::Text => "Text Editor",
            View::Image => "Image Viewer",
            View::External => "This Computer's App",
            View::Hex => "Hex Viewer",
            View::Browser => "Browser",
        }
    }
}

fn extension(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default()
}

/// A picture's format, by its name.
pub fn image_format(path: &str) -> Option<ImageFormat> {
    Some(match extension(path).as_str() {
        "png" => ImageFormat::Png,
        "jpg" | "jpeg" => ImageFormat::Jpeg,
        "gif" => ImageFormat::Gif,
        "webp" => ImageFormat::Webp,
        "bmp" => ImageFormat::Bmp,
        "ico" => ImageFormat::Ico,
        "tif" | "tiff" => ImageFormat::Tiff,
        "svg" => ImageFormat::Svg,
        _ => return None,
    })
}

/// Whether a file is a PDF, a sound or a video, by its name.
pub fn is_media(path: &str) -> bool {
    matches!(
        extension(path).as_str(),
        "pdf"
            | "mp3"
            | "m4a"
            | "aac"
            | "wav"
            | "flac"
            | "ogg"
            | "oga"
            | "opus"
            | "mp4"
            | "m4v"
            | "mov"
            | "webm"
            | "ogv"
    )
}

/// How a file opens without asking, by its name: a picture as one (but an
/// SVG, which is text, as text), a PDF, sound or video by this computer's
/// app; anything else as text, until its contents say otherwise.
pub fn opens_as(path: &str) -> View {
    if is_media(path) {
        View::External
    } else if image_format(path).is_some() && extension(path) != "svg" {
        View::Image
    } else {
        View::Text
    }
}

/// The ways a file could be shown, for Open With: text and bytes always,
/// a picture's and a PDF's or medium's where it is one.
pub fn ways(path: &str) -> Vec<View> {
    let mut out = vec![View::Text];
    if image_format(path).is_some() {
        out.push(View::Image);
    }
    out.push(View::External);
    out.push(View::Hex);
    if is_html(path) {
        out.push(View::Browser);
    }
    out
}

/// Whether a file is HTML, which a browser shows.
pub fn is_html(path: &str) -> bool {
    matches!(extension(path).as_str(), "html" | "htm")
}

/// Where the preview server shows a file: its address on the environment's
/// numbered host, under the secret, each part of the path escaped.
pub fn preview_url(host: &str, secret: &str, abs: &str) -> String {
    let path: Vec<String> = abs
        .split('/')
        .map(|part| {
            part.bytes()
                .map(|b| match b {
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                        (b as char).to_string()
                    }
                    b => format!("%{b:02X}"),
                })
                .collect()
        })
        .collect();
    format!("{host}{secret}{}", path.join("/"))
}

/// One row of the bytes: its offset, the bytes as hex, and as characters,
/// a dot for each that is not one.
pub fn hex_row(offset: usize, bytes: &[u8]) -> String {
    let mut hex = String::new();
    for i in 0..16 {
        match bytes.get(i) {
            Some(b) => hex.push_str(&format!("{b:02x} ")),
            None => hex.push_str("   "),
        }
        if i == 7 {
            hex.push(' ');
        }
    }
    let text: String = bytes
        .iter()
        .map(|b| {
            if b.is_ascii_graphic() || *b == b' ' {
                *b as char
            } else {
                '.'
            }
        })
        .collect();
    format!("{offset:08x}  {hex} {text}")
}

/// The most of a file read in one message.
const PART: usize = 1 << 20;
/// How much of a file the hex view reads at a time.
const HEX_PAGE: usize = 64 << 10;
/// The largest picture read.
const MOST_IMAGE: u64 = 64 << 20;

enum Shown {
    Reading,
    Failed(String),
    Image(Arc<gpui::Image>),
    Bytes(Vec<u8>),
    /// A PDF or medium, as big as it is.
    External,
}

pub struct FileViewer {
    code: Code,
    path: String,
    view: View,
    size: Option<u64>,
    shown: Shown,
    /// Whether this computer's app is being given the file.
    opening: bool,
    focus: FocusHandle,
}

impl Focusable for FileViewer {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Reads part of a file: its bytes and the file's size.
async fn read_part(
    code: &Code,
    path: &str,
    at: usize,
    len: usize,
) -> Result<(Vec<u8>, u64), String> {
    let v = code
        .call(
            "fs.bytes",
            json!({ "path": path, "offset": at, "length": len }),
        )
        .await?;
    let data = BASE64
        .decode(v["data"].as_str().unwrap_or_default())
        .map_err(|e| e.to_string())?;
    Ok((data, v["size"].as_u64().unwrap_or(0)))
}

/// Reads a whole file, up to most bytes.
async fn read_all(code: &Code, path: &str, most: u64) -> Result<Vec<u8>, String> {
    let mut out = vec![];
    loop {
        let (part, size) = read_part(code, path, out.len(), PART).await?;
        if size > most {
            return Err(format!("It is too big to show ({} MB).", size >> 20));
        }
        let done = part.len() < PART || part.is_empty();
        out.extend(part);
        if done || out.len() as u64 >= size {
            return Ok(out);
        }
    }
}

impl FileViewer {
    pub fn new(code: Code, path: &str, view: View, cx: &mut Context<Self>) -> Self {
        let mut v = FileViewer {
            code,
            path: path.to_string(),
            view,
            size: None,
            shown: Shown::Reading,
            opening: false,
            focus: cx.focus_handle(),
        };
        v.read(cx);
        v
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn view(&self) -> View {
        self.view
    }

    fn read(&mut self, cx: &mut Context<Self>) {
        let (code, path, view) = (self.code.clone(), self.path.clone(), self.view);
        cx.spawn(async move |this, cx| {
            let shown = match view {
                View::Image => match read_all(&code, &path, MOST_IMAGE).await {
                    Ok(bytes) => {
                        let size = bytes.len() as u64;
                        let format = image_format(&path).unwrap_or(ImageFormat::Png);
                        let _ = this.update(cx, |v, _| v.size = Some(size));
                        Shown::Image(Arc::new(gpui::Image::from_bytes(format, bytes)))
                    }
                    Err(e) => Shown::Failed(e),
                },
                View::Hex => match read_part(&code, &path, 0, HEX_PAGE).await {
                    Ok((bytes, size)) => {
                        let _ = this.update(cx, |v, _| v.size = Some(size));
                        Shown::Bytes(bytes)
                    }
                    Err(e) => Shown::Failed(e),
                },
                _ => match read_part(&code, &path, 0, 0).await {
                    Ok((_, size)) => {
                        let _ = this.update(cx, |v, _| v.size = Some(size));
                        Shown::External
                    }
                    Err(e) => Shown::Failed(e),
                },
            };
            let _ = this.update(cx, |v, cx| {
                v.shown = shown;
                cx.notify();
            });
        })
        .detach();
    }

    /// Reads the next part of the bytes.
    fn more(&mut self, cx: &mut Context<Self>) {
        let Shown::Bytes(have) = &self.shown else {
            return;
        };
        let (code, path, at) = (self.code.clone(), self.path.clone(), have.len());
        cx.spawn(async move |this, cx| {
            let got = read_part(&code, &path, at, HEX_PAGE).await;
            let _ = this.update(cx, |v, cx| {
                if let (Ok((part, _)), Shown::Bytes(have)) = (got, &mut v.shown) {
                    have.extend(part);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Gives the file to this computer's own app: read into a folder of its
    /// own under the temporary one, then opened as the system opens it.
    fn open_here(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.opening {
            return;
        }
        self.opening = true;
        cx.notify();
        let (code, path) = (self.code.clone(), self.path.clone());
        let name = path.rsplit('/').next().unwrap_or("file").to_string();
        cx.spawn_in(window, async move |this, cx| {
            let got = read_all(&code, &path, u64::MAX).await;
            let written = got.and_then(|bytes| {
                let dir = std::env::temp_dir().join(format!("hangar-open-{}", std::process::id()));
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                let to = dir.join(&name);
                std::fs::write(&to, bytes).map_err(|e| e.to_string())?;
                Ok(to)
            });
            let _ = this.update_in(cx, |v, window, cx| {
                v.opening = false;
                match written {
                    Ok(to) => cx.open_with_system(&to),
                    Err(e) => {
                        use gpui_component::WindowExt;
                        window.push_notification(
                            gpui_component::notification::Notification::error(e),
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn hex(&self, bytes: &[u8], p: &Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        let rows = bytes.len().div_ceil(16);
        let all: Arc<Vec<u8>> = Arc::new(bytes.to_vec());
        let colour = p.text;
        let more = self.size.is_some_and(|s| (bytes.len() as u64) < s);
        v_flex()
            .size_full()
            .child(
                uniform_list("hex", rows, move |range: Range<usize>, _, _| {
                    range
                        .map(|r| {
                            let from = r * 16;
                            let to = (from + 16).min(all.len());
                            div()
                                .px(px(12.))
                                .text_color(colour)
                                .whitespace_nowrap()
                                .child(hex_row(from, &all[from..to]))
                        })
                        .collect()
                })
                .flex_1()
                .font_family(crate::diff::mono())
                .text_size(px(12.)),
            )
            .when(more, |d| {
                d.child(
                    h_flex().p(px(8.)).child(
                        crate::ui::button("hex-more", "Read More", p)
                            .on_click(cx.listener(|v, _, _, cx| v.more(cx))),
                    ),
                )
            })
            .into_any_element()
    }
}

impl Render for FileViewer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let name = self
            .path
            .rsplit('/')
            .next()
            .unwrap_or(&self.path)
            .to_string();
        let size = self
            .size
            .map(|s| crate::docker::bytes(s as f64))
            .unwrap_or_default();
        let body = match &self.shown {
            Shown::Reading => note("Reading…", &p).into_any_element(),
            Shown::Failed(e) => note(&format!("Could not read it: {e}"), &p).into_any_element(),
            Shown::Image(image) => div()
                .size_full()
                .p(px(16.))
                .flex()
                .items_center()
                .justify_center()
                .child(img(image.clone()).max_w_full().max_h_full())
                .into_any_element(),
            Shown::Bytes(bytes) => {
                let bytes = bytes.clone();
                self.hex(&bytes, &p, cx)
            }
            Shown::External => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap(px(10.))
                .child(
                    div()
                        .text_size(px(14.))
                        .text_color(p.text)
                        .child(name.clone()),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(p.muted)
                        .child(size.clone()),
                )
                .child(
                    crate::ui::primary(
                        "open-here",
                        if self.opening {
                            SharedString::from("Opening…")
                        } else {
                            SharedString::from("Open in This Computer's App")
                        },
                        &p,
                    )
                    .debug_selector(|| "open-here".into())
                    .on_click(cx.listener(|v, _, window, cx| v.open_here(window, cx))),
                )
                .into_any_element(),
        };
        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .bg(p.bg)
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(10.))
                    .px(px(12.))
                    .py(px(4.))
                    .border_b_1()
                    .border_color(p.border)
                    .text_size(px(11.))
                    .text_color(p.muted)
                    .child(self.view.name())
                    .when(!size.is_empty(), |d| d.child(size.clone())),
            )
            .child(div().flex_1().min_h_0().child(body))
    }
}

fn note(text: &str, p: &Palette) -> impl IntoElement {
    div()
        .p(px(16.))
        .text_size(px(12.))
        .text_color(p.muted)
        .child(text.to_string())
}
