//! What the Project window's menu does to files, as IntelliJ's Project view
//! does: making files and folders, renaming, moving to the trash, cutting,
//! copying and pasting, and copying paths, through the Code service's fs.*
//! calls. Names are asked for in IntelliJ's small pop-up; the tree follows
//! the files as they change (fs.changed), so nothing here redraws it.

use std::cell::{Cell, RefCell};
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

    /// Whether it is a project's own folder, which is not renamed, moved
    /// or deleted from the tree.
    pub fn is_root(&self) -> bool {
        self.root.as_deref() == Some(self.abs.as_str())
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

/// What is done with a name typed in a dialog.
type WithName = Rc<dyn Fn(String, &mut Window, &mut App)>;
/// The call a name typed makes: through which connection, its method and
/// its parameters; None when there is nothing to do.
type CallFor = Rc<dyn Fn(&str) -> Option<(Code, &'static str, Value)>>;
/// What is done with a name once its call has answered.
type Made = Rc<dyn Fn(String, &mut Window, &mut App)>;
/// What is done once a dialog is confirmed.
type Then = Rc<dyn Fn(&mut Window, &mut App)>;

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

/// Asks for a name in IntelliJ's small pop-up, then does done with it,
/// trimmed: Enter takes it, Escape or a click elsewhere closes the pop-up.
/// A name with nothing in it is not taken.
pub fn ask_name(
    title: &str,
    initial: &str,
    window: &mut Window,
    cx: &mut App,
    done: impl Fn(String, &mut Window, &mut App) + 'static,
) {
    let done: WithName = Rc::new(done);
    popup(
        title,
        initial,
        Rc::new(RefCell::new(None)),
        window,
        cx,
        Rc::new(move |name, window, cx| {
            window.close_dialog(cx);
            done(name, window, cx);
        }),
    );
}

/// Asks for a name as ask_name does, then makes the call it is for; the
/// pop-up stays until the call answers, saying what went wrong under the
/// name, so it can be changed and tried again. Once made, done has the
/// name.
pub fn ask_call(
    title: &str,
    initial: &str,
    window: &mut Window,
    cx: &mut App,
    call: impl Fn(&str) -> Option<(Code, &'static str, Value)> + 'static,
    done: impl Fn(String, &mut Window, &mut App) + 'static,
) {
    let (call, done): (CallFor, Made) = (Rc::new(call), Rc::new(done));
    let error: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let busy = Rc::new(Cell::new(false));
    let shown = error.clone();
    popup(
        title,
        initial,
        shown,
        window,
        cx,
        Rc::new(move |name, window, cx| {
            if busy.get() {
                return;
            }
            let Some((code, method, params)) = call(&name) else {
                window.close_dialog(cx);
                return;
            };
            busy.set(true);
            let asked = code.call(method, params);
            let (error, busy, done) = (error.clone(), busy.clone(), done.clone());
            let handle = window.window_handle();
            cx.spawn(async move |cx| {
                let result = asked.await;
                let _ = handle.update(cx, |_, window, cx| {
                    busy.set(false);
                    match result {
                        Ok(_) => {
                            window.close_dialog(cx);
                            done(name, window, cx);
                        }
                        Err(e) => {
                            *error.borrow_mut() = Some(e);
                            window.refresh();
                        }
                    }
                });
            })
            .detach();
        }),
    );
}

/// The pop-up: a title, the name, and what went wrong under it.
fn popup(
    title: &str,
    initial: &str,
    error: Rc<RefCell<Option<String>>>,
    window: &mut Window,
    cx: &mut App,
    take: WithName,
) {
    let input = cx.new(|cx| InputState::new(window, cx).default_value(initial.to_string()));
    let handle = window.window_handle();
    {
        let (take, error) = (take.clone(), error.clone());
        cx.subscribe(&input, move |state, e: &InputEvent, cx| match e {
            InputEvent::PressEnter { .. } => {
                let name = state.read(cx).value().trim().to_string();
                if name.is_empty() {
                    return;
                }
                let take = take.clone();
                let _ = handle.update(cx, |_, window, cx| take(name, window, cx));
            }
            // What went wrong was about the name as it was.
            InputEvent::Change if error.borrow_mut().take().is_some() => {
                let _ = handle.update(cx, |_, window, _| window.refresh());
            }
            _ => {}
        })
        .detach();
    }
    let title = title.to_string();
    let first = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if first.replace(false) {
            window.focus(&input.focus_handle(cx), cx);
        }
        let p = palette::of(cx);
        let wrong = error.borrow().clone();
        dialog
            .title(
                div()
                    .text_size(px(12.))
                    .text_color(p.muted)
                    .child(title.clone()),
            )
            .w(px(380.))
            .margin_top(px(90.))
            .close_button(false)
            .overlay(false)
            // Enter is the name's own: the pop-up closes once it is taken.
            .on_ok(|_, _, _| false)
            .child(
                v_flex()
                    .gap(px(6.))
                    .child(Input::new(&input))
                    .when_some(wrong, |d, e| {
                        d.child(div().text_size(px(11.5)).text_color(p.bad).child(e))
                    }),
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
    let done: Then = Rc::new(done);
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
    let at = folder.clone();
    ask_call(
        "New File",
        "",
        window,
        cx,
        move |name| {
            Some((
                code.clone(),
                "fs.create",
                json!({ "path": join(&at, name) }),
            ))
        },
        move |name, window, cx| opened(join(&folder, &name), window, cx),
    );
}

/// Makes a folder, asking its name, then does made with its path; a name
/// with slashes makes each folder on the way.
pub fn new_folder(
    t: &Target,
    window: &mut Window,
    cx: &mut App,
    made: impl Fn(String, &mut Window, &mut App) + 'static,
) {
    let (code, folder) = (t.code.clone(), t.folder());
    let at = folder.clone();
    ask_call(
        "New Folder",
        "",
        window,
        cx,
        move |name| Some((code.clone(), "fs.mkdir", json!({ "path": join(&at, name) }))),
        move |name, window, cx| made(join(&folder, &name), window, cx),
    );
}

/// Renames, asking the new name, then does renamed with the new path.
pub fn rename(
    t: &Target,
    window: &mut Window,
    cx: &mut App,
    renamed: impl Fn(String, &mut Window, &mut App) + 'static,
) {
    let (code, from) = (t.code.clone(), t.abs.clone());
    let parent = Target {
        dir: false,
        ..t.clone()
    }
    .folder();
    let at = parent.clone();
    ask_call(
        "Rename",
        t.name(),
        window,
        cx,
        move |name| {
            let to = join(&at, name);
            (to != from).then(|| (code.clone(), "fs.rename", json!({ "from": from, "to": to })))
        },
        move |name, window, cx| renamed(join(&parent, &name), window, cx),
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

/// The workspace's projects as they would be after a change: a folder
/// added at the end, or a project removed, or moved by one place.
pub fn reordered(roots: &[String], change: Reorder) -> Vec<String> {
    let mut out = roots.to_vec();
    match change {
        Reorder::Add(dir) => {
            if !out.contains(&dir) {
                out.push(dir);
            }
        }
        Reorder::Remove(root) => out.retain(|r| r != &root),
        Reorder::Move(root, by) => {
            if let Some(i) = out.iter().position(|r| r == &root) {
                let j = (i as i64 + by).clamp(0, out.len() as i64 - 1) as usize;
                let r = out.remove(i);
                out.insert(j, r);
            }
        }
    }
    out
}

/// A change to the workspace's projects.
#[derive(Debug, Clone, PartialEq)]
pub enum Reorder {
    Add(String),
    Remove(String),
    Move(String, i64),
}

/// Changes the workspace's projects, saying what went wrong in a
/// notification.
pub fn change_workspace(
    ws: &gpui::Entity<crate::workspace::Workspace>,
    change: Reorder,
    window: &mut Window,
    cx: &mut App,
) {
    let folders = reordered(&ws.read(cx).roots(), change);
    let call = ws.read(cx).set_folders(folders);
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        if let Err(e) = call.await {
            let _ = handle.update(cx, |_, window, cx| {
                window.push_notification(Notification::error(e), cx)
            });
        }
    })
    .detach();
}

/// Trusts a project to run its own code, as its language servers do.
pub fn trust(t: &Target, window: &mut Window, cx: &mut App) {
    run(&t.code, "trust.grant", json!({}), window, cx);
}
