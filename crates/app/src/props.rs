//! File Properties, as IntelliJ's: a file's line separators, its encoding
//! and byte order mark, whether it is read-only or may be run, its size and
//! permissions; each changed in place, the file converted by the Code
//! service, and read again after.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{App, ParentElement, SharedString, Styled, Window, div, prelude::*, px};
use gpui_component::{WindowExt, h_flex, v_flex};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::code::Code;
use crate::gitlog::chip;
use crate::palette::{self, Palette};

/// What fs.textProps says of a file. Without an encoding it is not text.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct Props {
    pub encoding: Option<String>,
    pub bom: bool,
    /// LF, CRLF, CR, "mixed", or none for a file of one line.
    pub separator: Option<String>,
    pub mode: u32,
    pub size: u64,
    pub modified: f64,
}

impl Props {
    pub fn writable(&self) -> bool {
        self.mode & 0o200 != 0
    }

    pub fn executable(&self) -> bool {
        self.mode & 0o100 != 0
    }
}

/// Permissions as ls shows them: rw-r--r--.
pub fn mode_string(mode: u32) -> String {
    (0..9)
        .map(|i| {
            let bit = 0o400 >> i;
            if mode & bit == 0 {
                '-'
            } else {
                ['r', 'w', 'x'][i % 3]
            }
        })
        .collect()
}

pub const SEPARATORS: [(&str, &str); 3] = [
    ("LF", "LF: Unix and macOS (\\n)"),
    ("CRLF", "CRLF: Windows (\\r\\n)"),
    ("CR", "CR: classic Mac OS (\\r)"),
];
pub const ENCODINGS: [&str; 4] = ["UTF-8", "UTF-16LE", "UTF-16BE", "ISO-8859-1"];

struct Shown {
    code: Code,
    path: String,
    props: Option<Result<Props, String>>,
    busy: bool,
}

type State = Rc<RefCell<Shown>>;

/// Opens a file's properties.
pub fn show(code: Code, path: String, window: &mut Window, cx: &mut App) {
    let name = path.rsplit('/').next().unwrap_or(&path).to_string();
    let state: State = Rc::new(RefCell::new(Shown {
        code,
        path,
        props: None,
        busy: false,
    }));
    read(&state, window, cx);
    let title = format!("File Properties: {name}");
    window.open_dialog(cx, move |dialog, _, cx| {
        let p = palette::of(cx);
        dialog
            .title(title.clone())
            .w(px(520.))
            .child(body(&state, &p))
    });
}

/// Reads the properties again.
fn read(state: &State, window: &mut Window, cx: &mut App) {
    let (code, path) = {
        let s = state.borrow();
        (s.code.clone(), s.path.clone())
    };
    let call = code.call("fs.textProps", json!({ "path": path }));
    let (state, handle) = (state.clone(), window.window_handle());
    cx.spawn(async move |cx| {
        let got = call
            .await
            .and_then(|v| serde_json::from_value::<Props>(v).map_err(|e| e.to_string()));
        let _ = handle.update(cx, |_, window, _| {
            let mut s = state.borrow_mut();
            s.props = Some(got);
            s.busy = false;
            window.refresh();
        });
    })
    .detach();
}

/// Changes the file, then reads it again.
fn change(state: &State, method: &'static str, params: Value, window: &mut Window, cx: &mut App) {
    let call = {
        let mut s = state.borrow_mut();
        if s.busy {
            return;
        }
        s.busy = true;
        let mut params = params;
        params["path"] = Value::String(s.path.clone());
        s.code.call(method, params)
    };
    let (state, handle) = (state.clone(), window.window_handle());
    cx.spawn(async move |cx| {
        let got = call.await;
        let _ = handle.update(cx, |_, window, cx| {
            if let Err(e) = got {
                state.borrow_mut().props = Some(Err(e));
                state.borrow_mut().busy = false;
                window.refresh();
                return;
            }
            read(&state, window, cx);
        });
    })
    .detach();
}

fn body(state: &State, p: &Palette) -> gpui::Div {
    let s = state.borrow();
    let props = match &s.props {
        None => return note("Reading…", p.muted),
        Some(Err(e)) => return note(e, p.bad),
        Some(Ok(props)) => props.clone(),
    };
    let row = |label: &str| {
        h_flex().gap(px(10.)).items_start().child(
            div()
                .w(px(130.))
                .flex_none()
                .text_color(p.muted)
                .child(label.to_string()),
        )
    };
    let option = |id: String, label: String, on: bool, method: &'static str, params: Value| {
        let state = state.clone();
        chip(SharedString::from(id.clone()), label, on, p)
            .debug_selector(move || id.clone())
            .when(!on, |d| {
                d.on_click(move |_, window, cx| change(&state, method, params.clone(), window, cx))
            })
    };
    let mut out = v_flex().gap(px(10.)).text_size(px(12.));
    match &props.encoding {
        Some(encoding) => {
            let mut seps = h_flex().gap(px(4.)).flex_wrap();
            for (sep, about) in SEPARATORS {
                let on = props.separator.as_deref() == Some(sep);
                seps = seps.child(option(
                    format!("props-sep-{sep}"),
                    about.to_string(),
                    on,
                    "fs.convert",
                    json!({ "separator": sep }),
                ));
            }
            let sep_note = match props.separator.as_deref() {
                Some("mixed") => Some("Mixed: choosing one makes every line end so."),
                None => Some("One line, ended by none."),
                _ => None,
            };
            out = out.child(
                row("Line separators").child(
                    v_flex()
                        .gap(px(4.))
                        .child(seps)
                        .when_some(sep_note, |d, n| {
                            d.child(div().text_size(px(11.)).text_color(p.muted).child(n))
                        }),
                ),
            );
            let mut encs = h_flex().gap(px(4.)).flex_wrap();
            for e in ENCODINGS {
                encs = encs.child(option(
                    format!("props-enc-{e}"),
                    e.to_string(),
                    encoding == e,
                    "fs.convert",
                    json!({ "encoding": e }),
                ));
            }
            out = out.child(row("Encoding").child(encs));
            // UTF-16 has its mark; ISO-8859-1 has none to have.
            let bom_fixed = encoding == "ISO-8859-1" || (props.bom && encoding != "UTF-8");
            out =
                out.child(
                    row("Byte order mark").child(
                        h_flex()
                            .gap(px(4.))
                            .child(if bom_fixed {
                                chip(
                                    "props-bom-fixed",
                                    if props.bom { "With" } else { "Without" }.into(),
                                    true,
                                    p,
                                )
                            } else {
                                option(
                                    if props.bom {
                                        "props-bom-off"
                                    } else {
                                        "props-bom-on"
                                    }
                                    .into(),
                                    if props.bom { "Remove It" } else { "Add One" }.into(),
                                    false,
                                    "fs.convert",
                                    json!({ "bom": !props.bom }),
                                )
                            })
                            .child(div().text_size(px(11.)).text_color(p.muted).child(
                                if props.bom {
                                    "The file has one."
                                } else {
                                    "The file has none."
                                },
                            )),
                    ),
                );
        }
        None => out = out.child(row("Contents").child("Not text.")),
    }
    out = out
        .child(row("Read-only").child(option(
            "props-writable".into(),
            if props.writable() {
                "Make Read-only".into()
            } else {
                "Make Writable".into()
            },
            false,
            "fs.setMode",
            json!({ "writable": !props.writable() }),
        )))
        .child(row("Executable").child(option(
            "props-exec".into(),
            if props.executable() {
                "Remove Executable Permission".into()
            } else {
                "Make Executable".into()
            },
            false,
            "fs.setMode",
            json!({ "exec": !props.executable() }),
        )))
        .child(
            row("Permissions").child(
                div()
                    .font_family(crate::diff::mono())
                    .child(mode_string(props.mode)),
            ),
        )
        .child(row("Size").child(crate::docker::bytes(props.size as f64)))
        .child(row("Modified").child(crate::gitlog::when(props.modified, chrono::Local::now())));
    if s.busy {
        out = out.child(
            div()
                .text_size(px(11.))
                .text_color(p.muted)
                .child("Changing…"),
        );
    }
    out
}

fn note(text: &str, colour: gpui::Hsla) -> gpui::Div {
    div().text_color(colour).child(text.to_string())
}
