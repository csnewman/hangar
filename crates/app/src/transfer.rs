//! IntelliJ's Copy… and Move… (F5, F6) in the Project window: the folder to
//! put a file or folder in, made if it is not there, and its name there. A
//! name the folder already has is asked about: replaced (the one there goes
//! to the trash), skipped, or both kept, the new one renamed.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{
    App, AppContext, Entity, Focusable, ParentElement, Styled, Window, div, prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{WindowExt, h_flex, v_flex};
use serde_json::{Value, json};

use crate::palette;
use crate::treeops::Target;
use crate::ui;

/// Copy or Move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Copy,
    Move,
}

impl Op {
    fn word(self) -> &'static str {
        match self {
            Op::Copy => "Copy",
            Op::Move => "Move",
        }
    }

    fn param(self) -> &'static str {
        match self {
            Op::Copy => "copy",
            Op::Move => "move",
        }
    }
}

/// What is done with where the thing went.
type Went = Rc<dyn Fn(String, &mut Window, &mut App)>;

/// The folder typed, from the target's own folder when it is not
/// absolute; without its trailing slash.
pub fn folder_typed(typed: &str, from: &str) -> String {
    let typed = typed.trim();
    let joined = if typed.starts_with('/') {
        typed.to_string()
    } else {
        format!("{}/{typed}", from.trim_end_matches('/'))
    };
    match joined.trim_end_matches('/') {
        "" => "/".into(),
        f => f.to_string(),
    }
}

/// The dialog's state, shared by its inputs and buttons.
struct Asked {
    op: Op,
    target: Target,
    name: Entity<InputState>,
    folder: Entity<InputState>,
    error: RefCell<Option<String>>,
    busy: Cell<bool>,
    went: Went,
}

/// Asks where to copy or move a file or folder, then does it, and does
/// went with where it went.
pub fn ask(
    op: Op,
    t: &Target,
    window: &mut Window,
    cx: &mut App,
    went: impl Fn(String, &mut Window, &mut App) + 'static,
) {
    let parent = Target {
        dir: false,
        ..t.clone()
    }
    .folder();
    // The name selected up to its extension; the folder ready to be typed
    // on from.
    let stem = match t.name().rfind('.') {
        Some(dot) if dot > 0 && !t.dir => dot,
        _ => t.name().len(),
    };
    let name = cx.new(|cx| {
        let mut s = InputState::new(window, cx).default_value(t.name().to_string());
        s.set_selected_range(0..stem, cx);
        s
    });
    let end = parent.len();
    let folder = cx.new(|cx| {
        let mut s = InputState::new(window, cx).default_value(parent);
        s.set_selected_range(end..end, cx);
        s
    });
    let asked = Rc::new(Asked {
        op,
        target: t.clone(),
        name: name.clone(),
        folder: folder.clone(),
        error: RefCell::new(None),
        busy: Cell::new(false),
        went: Rc::new(went),
    });
    let handle = window.window_handle();
    for input in [&name, &folder] {
        let asked = asked.clone();
        cx.subscribe(input, move |_, e: &InputEvent, cx| match e {
            InputEvent::PressEnter { .. } => {
                let asked = asked.clone();
                let _ = handle.update(cx, |_, window, cx| go(&asked, "ask", window, cx));
            }
            InputEvent::Change if asked.error.borrow_mut().take().is_some() => {
                let _ = handle.update(cx, |_, window, _| window.refresh());
            }
            _ => {}
        })
        .detach();
    }
    let first = Cell::new(true);
    let title = format!("{} {}", op.word(), t.name());
    window.open_dialog(cx, move |dialog, window, cx| {
        if first.replace(false) {
            window.focus(&asked.folder.focus_handle(cx), cx);
        }
        let p = palette::of(cx);
        let wrong = asked.error.borrow().clone();
        let field = |label: &str, input: &Entity<InputState>| {
            v_flex()
                .gap(px(3.))
                .child(
                    div()
                        .text_size(px(11.5))
                        .text_color(p.muted)
                        .child(label.to_string()),
                )
                .child(Input::new(input))
        };
        let go_asked = asked.clone();
        dialog
            .title(title.clone())
            .w(px(460.))
            .on_ok(|_, _, _| false)
            .child(
                v_flex()
                    .gap(px(10.))
                    .child(field("New name", &asked.name))
                    .child(field("To folder", &asked.folder))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(p.muted)
                            .child("A path from the file's own folder, or from /. It is made if it is not there."),
                    )
                    .when_some(wrong, |d, e| {
                        d.child(div().text_size(px(11.5)).text_color(p.bad).child(e))
                    })
                    .child(
                        h_flex()
                            .justify_end()
                            .gap(px(8.))
                            .child(
                                ui::button("dialog-cancel", "Cancel", &p)
                                    .on_click(|_, window, cx| window.close_dialog(cx)),
                            )
                            .child(
                                ui::primary("dialog-ok", asked.op.word(), &p)
                                    .debug_selector(|| "dialog-ok".into())
                                    .on_click(move |_, window, cx| {
                                        go(&go_asked, "ask", window, cx)
                                    }),
                            ),
                    ),
            )
    });
}

/// Copies or moves, asking first about a name the folder has.
fn go(asked: &Rc<Asked>, clash: &str, window: &mut Window, cx: &mut App) {
    if asked.busy.get() {
        return;
    }
    let name = asked.name.read(cx).value().trim().to_string();
    if name.is_empty() || name.contains('/') {
        *asked.error.borrow_mut() = Some("A name, without a slash.".into());
        window.refresh();
        return;
    }
    let from = Target {
        dir: false,
        ..asked.target.clone()
    }
    .folder();
    let to = folder_typed(&asked.folder.read(cx).value(), &from);
    asked.busy.set(true);
    let call = asked.target.code.call(
        "fs.transfer",
        json!({
            "op": asked.op.param(),
            "from": [asked.target.abs],
            "to": to,
            "name": name,
            "clash": clash,
        }),
    );
    let asked = asked.clone();
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let result = call.await;
        let _ = handle.update(cx, |_, window, cx| {
            asked.busy.set(false);
            match result {
                Err(e) => {
                    *asked.error.borrow_mut() = Some(e);
                    window.refresh();
                }
                Ok(r) if !clashes(&r).is_empty() => {
                    window.close_dialog(cx);
                    ask_clash(&asked, &name, &to, window, cx);
                }
                Ok(r) => {
                    window.close_dialog(cx);
                    let moved = matches!(asked.op, Op::Move);
                    if let Some(done) =
                        crate::filehistory::Done::of_transfer(&asked.target.code, moved, &r)
                    {
                        crate::filehistory::record(done, cx);
                    }
                    if let Some(went) = r["done"][0]["to"].as_str() {
                        (asked.went)(went.to_string(), window, cx);
                    }
                }
            }
        });
    })
    .detach();
}

fn clashes(r: &Value) -> Vec<String> {
    r["clashes"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| c.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Asks what to do with a name the folder has, then goes on with it.
fn ask_clash(asked: &Rc<Asked>, name: &str, to: &str, window: &mut Window, cx: &mut App) {
    let title = format!("{name} already exists in {to}");
    let doing = match asked.op {
        Op::Copy => "Copying",
        Op::Move => "Moving",
    };
    let asked = asked.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let p = palette::of(cx);
        let choice = |id: &'static str, label: &'static str, clash: &'static str, primary: bool| {
            let asked = asked.clone();
            let b = if primary {
                ui::primary(id, label, &p)
            } else {
                ui::button(id, label, &p)
            };
            b.debug_selector(move || id.to_string())
                .on_click(move |_, window, cx| {
                    window.close_dialog(cx);
                    go(&asked, clash, window, cx)
                })
        };
        dialog.title(title.clone()).w(px(440.)).child(
            v_flex()
                .gap(px(12.))
                .child(div().text_color(p.muted).child(format!(
                    "{doing} would put another there. What is replaced goes to the trash."
                )))
                .child(
                    h_flex()
                        .justify_end()
                        .gap(px(8.))
                        .child(
                            ui::button("dialog-cancel", "Cancel", &p)
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(choice("clash-skip", "Skip", "skip", false))
                        .child(choice("clash-both", "Keep Both", "rename", false))
                        .child(choice("clash-replace", "Replace", "replace", true)),
                ),
        )
    });
}
