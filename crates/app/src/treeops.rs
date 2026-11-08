//! What the Project window's menu does to files, as IntelliJ's Project view
//! does: making files and folders, renaming, moving to the trash, cutting,
//! copying and pasting, and copying paths, through the Code service's fs.*
//! calls. Names are asked for in a dialog; the tree follows the files as
//! they change (fs.changed), so nothing here redraws it.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    App, AppContext, ClipboardItem, Focusable, IntoElement, ParentElement, Styled, Window, div,
    prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::notification::Notification;
use gpui_component::{WindowExt, h_flex, v_flex};
use serde_json::{Value, json};

use crate::code::Code;
use crate::palette;
use crate::ui;

/// A file or folder the menu acts on, through the connection that reaches
/// it: its project's, or the editor folder's for the machine's files.
#[derive(Clone)]
pub struct Target {
    pub code: Code,
    /// Its project's root; None for a file of the machine's.
    pub root: Option<String>,
    pub abs: String,
    pub dir: bool,
}

impl Target {
    pub fn name(&self) -> &str {
        self.abs.rsplit('/').next().unwrap_or(&self.abs)
    }

    /// The folder something made or pasted here goes in: the folder itself,
    /// or a file's own.
    pub fn folder(&self) -> String {
        if self.dir {
            return self.abs.clone();
        }
        match self.abs.rsplit_once('/') {
            Some(("", _)) | None => "/".into(),
            Some((parent, _)) => parent.into(),
        }
    }

    /// Its path from its project's root, or from / for the machine's.
    pub fn relative(&self) -> String {
        let root = self.root.as_deref().unwrap_or("/").trim_end_matches('/');
        self.abs
            .strip_prefix(&format!("{root}/"))
            .unwrap_or(&self.abs)
            .to_string()
    }
}

/// A path made of a folder and a name typed for something in it.
pub fn join(folder: &str, name: &str) -> String {
    format!(
        "{}/{}",
        folder.trim_end_matches('/'),
        name.trim_matches('/')
    )
}

/// Calls the service, saying what went wrong in a notification.
pub fn run(code: &Code, method: &str, params: Value, window: &mut Window, cx: &mut App) {
    run_then(code, method, params, window, cx, |_, _, _| {});
}

/// Calls the service, then does done with its answer; what went wrong is
/// said in a notification.
pub fn run_then(
    code: &Code,
    method: &str,
    params: Value,
    window: &mut Window,
    cx: &mut App,
    done: impl FnOnce(Value, &mut Window, &mut App) + 'static,
) {
    let call = code.call(method, params);
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let result = call.await;
        let _ = handle.update(cx, |_, window, cx| match result {
            Ok(v) => done(v, window, cx),
            Err(e) => window.push_notification(Notification::error(e), cx),
        });
    })
    .detach();
}

/// Copies text to the clipboard.
pub fn copy(text: String, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(text));
}

/// Asks for a name in a dialog, then does done with it, trimmed; Enter is
/// OK, Escape cancels. A name with nothing in it is not taken.
pub fn ask_name(
    title: &str,
    initial: &str,
    ok: &str,
    window: &mut Window,
    cx: &mut App,
    done: impl Fn(String, &mut Window, &mut App) + 'static,
) {
    let input = cx.new(|cx| InputState::new(window, cx).default_value(initial.to_string()));
    let done: Rc<dyn Fn(String, &mut Window, &mut App)> = Rc::new(done);
    let take = {
        let (input, done) = (input.clone(), done.clone());
        Rc::new(move |window: &mut Window, cx: &mut App| {
            let name = input.read(cx).value().trim().to_string();
            if name.is_empty() {
                return;
            }
            window.close_dialog(cx);
            done(name, window, cx);
        })
    };
    let handle = window.window_handle();
    let on_enter = take.clone();
    cx.subscribe(&input, move |_, e: &InputEvent, cx| {
        if let InputEvent::PressEnter { .. } = e {
            let on_enter = on_enter.clone();
            let _ = handle.update(cx, |_, window, cx| on_enter(window, cx));
        }
    })
    .detach();
    let (title, ok) = (title.to_string(), ok.to_string());
    let first = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if first.replace(false) {
            window.focus(&input.focus_handle(cx), cx);
        }
        let p = palette::of(cx);
        let take = take.clone();
        dialog.title(title.clone()).w(px(420.)).child(
            v_flex()
                .gap(px(10.))
                .child(Input::new(&input))
                .child(buttons(&ok, &p, move |window, cx| take(window, cx))),
        )
    });
}

/// Asks to go ahead with something that cannot be taken back here, then
/// does done.
pub fn confirm(
    title: &str,
    body: &str,
    ok: &str,
    window: &mut Window,
    cx: &mut App,
    done: impl Fn(&mut Window, &mut App) + 'static,
) {
    let done: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(done);
    let (title, body, ok) = (title.to_string(), body.to_string(), ok.to_string());
    window.open_dialog(cx, move |dialog, _, cx| {
        let p = palette::of(cx);
        let done = done.clone();
        dialog.title(title.clone()).w(px(420.)).child(
            v_flex()
                .gap(px(12.))
                .child(div().text_color(p.muted).child(body.clone()))
                .child(buttons(&ok, &p, move |window, cx| {
                    window.close_dialog(cx);
                    done(window, cx);
                })),
        )
    });
}

/// Cancel and the action, at the dialog's foot.
fn buttons(
    ok: &str,
    p: &palette::Palette,
    on_ok: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    h_flex()
        .justify_end()
        .gap(px(8.))
        .child(
            ui::button("dialog-cancel", "Cancel", p)
                .on_click(|_, window, cx| window.close_dialog(cx)),
        )
        .child(
            ui::primary("dialog-ok", ok.to_string(), p)
                .on_click(move |_, window, cx| on_ok(window, cx)),
        )
}

/// Makes an empty file in folder, asking its name, then does opened with
/// its path.
pub fn new_file(
    t: &Target,
    window: &mut Window,
    cx: &mut App,
    opened: impl Fn(String, &mut Window, &mut App) + 'static,
) {
    let (code, folder) = (t.code.clone(), t.folder());
    let opened = Rc::new(opened);
    ask_name(
        "New File",
        "",
        "Create",
        window,
        cx,
        move |name, window, cx| {
            let path = join(&folder, &name);
            let opened = opened.clone();
            let made = path.clone();
            run_then(
                &code,
                "fs.create",
                json!({ "path": path }),
                window,
                cx,
                move |_, window, cx| opened(made, window, cx),
            );
        },
    );
}

/// Makes a folder, asking its name; a name with slashes makes each folder
/// on the way.
pub fn new_folder(t: &Target, window: &mut Window, cx: &mut App) {
    let (code, folder) = (t.code.clone(), t.folder());
    ask_name(
        "New Folder",
        "",
        "Create",
        window,
        cx,
        move |name, window, cx| {
            run(
                &code,
                "fs.mkdir",
                json!({ "path": join(&folder, &name) }),
                window,
                cx,
            );
        },
    );
}

pub fn rename(t: &Target, window: &mut Window, cx: &mut App) {
    let (code, from) = (t.code.clone(), t.abs.clone());
    let parent = Target {
        dir: false,
        ..t.clone()
    }
    .folder();
    ask_name(
        "Rename",
        t.name(),
        "Rename",
        window,
        cx,
        move |name, window, cx| {
            let to = join(&parent, &name);
            if to != from {
                run(
                    &code,
                    "fs.rename",
                    json!({ "from": from, "to": to }),
                    window,
                    cx,
                );
            }
        },
    );
}

/// Moves to the environment's trash, from where it can be restored.
pub fn trash(t: &Target, window: &mut Window, cx: &mut App) {
    let (code, path) = (t.code.clone(), t.abs.clone());
    let what = if t.dir { "folder" } else { "file" };
    confirm(
        &format!("Move {} to the trash?", t.name()),
        &format!("The {what} goes to the environment's trash, from where it can be restored."),
        "Move to Trash",
        window,
        cx,
        move |window, cx| run(&code, "fs.trash", json!({ "paths": [path] }), window, cx),
    );
}

/// What Cut or Copy took, to paste: the paths, through their connection,
/// and whether pasting moves them.
#[derive(Clone)]
pub struct Taken {
    pub code: Code,
    pub paths: Vec<String>,
    pub cut: bool,
}

/// Pastes what was taken into the target's folder; a name the folder has
/// is given a new one rather than written over.
pub fn paste(taken: &Taken, t: &Target, window: &mut Window, cx: &mut App) {
    let op = if taken.cut { "move" } else { "copy" };
    run(
        &taken.code,
        "fs.transfer",
        json!({ "op": op, "from": taken.paths, "to": t.folder(), "clash": "rename" }),
        window,
        cx,
    );
}

/// Stages the file or folder in its repository.
pub fn stage(t: &Target, window: &mut Window, cx: &mut App) {
    run(&t.code, "git.stage", json!({ "path": t.abs }), window, cx);
}

/// Adds it to its repository's .gitignore.
pub fn ignore(t: &Target, window: &mut Window, cx: &mut App) {
    run(
        &t.code,
        "git.ignore",
        json!({ "path": t.abs, "dir": t.dir }),
        window,
        cx,
    );
}
