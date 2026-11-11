//! What the Project window's menu does to files, as IntelliJ's Project view
//! does: checking names typed for files and folders made or renamed,
//! moving to the trash, cutting, copying and pasting, and copying paths,
//! through the Code service's fs.* calls. The tree follows the files as
//! they change (fs.changed), so nothing here redraws it.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    App, AppContext, ClipboardItem, Focusable, FontWeight, IntoElement, ParentElement, Styled,
    Window, div, prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::notification::Notification;
use gpui_component::{WindowExt, h_flex, v_flex};
use serde::Deserialize;
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

    /// Its path as the tree names its row: from its project's root, or
    /// absolute for the machine's.
    pub fn relative_to_tree(&self) -> String {
        match &self.root {
            Some(_) => self.relative(),
            None => self.abs.clone(),
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

/// What is done with a name typed in a dialog.
type WithName = Rc<dyn Fn(String, &mut Window, &mut App)>;
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

/// Asks for a name in a small pop-up, then does done with it, trimmed:
/// Enter takes it, Escape or a click elsewhere closes the pop-up. A name
/// with nothing in it is not taken.
pub fn ask_name(
    title: &str,
    initial: &str,
    window: &mut Window,
    cx: &mut App,
    done: impl Fn(String, &mut Window, &mut App) + 'static,
) {
    let done: WithName = Rc::new(done);
    let input = cx.new(|cx| {
        let mut s = InputState::new(window, cx).default_value(initial.to_string());
        s.set_selected_range(0..stem(initial), cx);
        s
    });
    let handle = window.window_handle();
    cx.subscribe(&input, move |field, e: &InputEvent, cx| {
        if let InputEvent::PressEnter { .. } = e {
            let name = field.read(cx).value().trim().to_string();
            if name.is_empty() {
                return;
            }
            let done = done.clone();
            let _ = handle.update(cx, |_, window, cx| {
                window.close_dialog(cx);
                done(name, window, cx)
            });
        }
    })
    .detach();
    let title = title.to_string();
    let first = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if first.replace(false) {
            window.focus(&input.focus_handle(cx), cx);
        }
        let p = palette::of(cx);
        dialog
            .w(px(360.))
            .p_0()
            .min_h(px(0.))
            .centered()
            .close_button(false)
            .overlay(false)
            // Enter is the name's own: the pop-up closes once it is taken.
            .on_ok(|_, _, _| false)
            .child(
                v_flex()
                    .pb(px(8.))
                    .child(
                        div()
                            .px(px(12.))
                            .pt(px(9.))
                            .pb(px(7.))
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.text)
                            .child(title.clone()),
                    )
                    .child(
                        h_flex()
                            .h(px(32.))
                            .pl(px(1.))
                            .pr(px(4.))
                            .bg(p.surface)
                            .border_t_1()
                            .border_b_1()
                            .border_color(p.border)
                            .child(div().flex_1().child(Input::new(&input).appearance(false))),
                    ),
            )
    });
}

/// How much of a name is selected when it is offered to be changed: up to
/// its extension, as IntelliJ's and VS Code's Rename select it.
pub fn stem(name: &str) -> usize {
    match name.rfind('.') {
        Some(dot) if dot > 0 => dot,
        _ => name.len(),
    }
}

/// What a name typed would run into, given the folders listed so far.
#[derive(Debug, Clone, PartialEq)]
pub enum Check {
    Fine,
    /// A folder to list before it can be said.
    List(String),
    Wrong(String),
}

/// The folder an absolute path is in.
pub fn parent_of(path: &str) -> String {
    match path.trim_end_matches('/').rsplit_once('/') {
        Some(("", _)) | None => "/".into(),
        Some((parent, _)) => parent.into(),
    }
}

/// Where a name typed in a folder puts what is made, as IntelliJ's New
/// File reads it: slashes separate the folders on the way, made if they
/// are not there, "." is the folder it is in and ".." the one above.
pub fn place(folder: &str, name: &str) -> String {
    let mut at: Vec<&str> = folder.split('/').filter(|s| !s.is_empty()).collect();
    for step in name.split('/') {
        match step {
            "" | "." => {}
            ".." => {
                at.pop();
            }
            s => at.push(s),
        }
    }
    format!("/{}", at.join("/"))
}

/// The entries of a folder listed: each one's name and whether it is a
/// folder.
pub type Entries = Vec<(String, bool)>;

/// What is wrong with a name typed for something new in folder, said as it
/// is typed, as IntelliJ's New File says it: a name that is only dots, a
/// way above /, a file where a folder is to be, or something of that name
/// there already. listed has each folder listed so far; own is what is
/// renamed, which may keep its name.
pub fn check(
    folder: &str,
    name: &str,
    own: Option<&str>,
    listed: &dyn Fn(&str) -> Option<Entries>,
) -> Check {
    let steps: Vec<&str> = name.split('/').filter(|s| !s.is_empty()).collect();
    let Some((last, way)) = steps.split_last() else {
        return Check::Fine;
    };
    if matches!(*last, "." | "..") {
        return Check::Wrong(format!("“{last}” is not a name"));
    }
    let mut at = place(folder, "");
    for step in way {
        match *step {
            "." => continue,
            ".." if at == "/" => return Check::Wrong("There is nothing above /".into()),
            ".." => {
                at = parent_of(&at);
                continue;
            }
            _ => {}
        }
        let Some(entries) = listed(&at) else {
            return Check::List(at);
        };
        match entries.iter().find(|(n, _)| n == step) {
            Some((_, false)) => {
                return Check::Wrong(format!("“{step}” is a file, not a directory"));
            }
            Some((_, true)) => at = join(&at, step),
            // Made on the way, so nothing in it can be in the way.
            None => return Check::Fine,
        }
    }
    if own == Some(join(&at, last).as_str()) {
        return Check::Fine;
    }
    let Some(entries) = listed(&at) else {
        return Check::List(at);
    };
    match entries.iter().find(|(n, _)| n == last) {
        Some((_, true)) => Check::Wrong(format!("A directory named “{last}” already exists")),
        Some((_, false)) => Check::Wrong(format!("A file named “{last}” already exists")),
        None => Check::Fine,
    }
}

/// The folders a checker has listed, None while asked.
type Listed = Rc<RefCell<HashMap<String, Option<Entries>>>>;

/// What fs.list says of an entry, as far as checking a name needs.
#[derive(Deserialize)]
struct Listing {
    name: String,
    #[serde(default)]
    dir: bool,
}

/// Checks names typed for something new, or for something renamed,
/// against the folder it goes in, listing each folder a name needs as it
/// is first needed. own is what is renamed, which may keep its name, and
/// nested whether a name may name folders on the way.
#[derive(Clone)]
pub struct Checker {
    code: Code,
    folder: String,
    own: Option<String>,
    nested: bool,
    listed: Listed,
}

impl Checker {
    pub fn new(code: Code, folder: String, own: Option<String>, nested: bool) -> Self {
        Checker {
            code,
            folder,
            own,
            nested,
            listed: Rc::default(),
        }
    }

    /// What is wrong with a name, as far as is known: a folder it needs
    /// that is not listed yet is asked for, and again called once it is,
    /// to check the name as it stands then.
    pub fn wrong(
        &self,
        name: &str,
        cx: &mut App,
        again: impl FnOnce(&mut App) + 'static,
    ) -> Option<String> {
        if !self.nested && name.contains('/') {
            return Some("A name cannot contain “/”".into());
        }
        let found = {
            let l = self.listed.borrow();
            check(&self.folder, name, self.own.as_deref(), &|f| {
                l.get(f).cloned().flatten()
            })
        };
        let folder = match found {
            Check::Fine => return None,
            Check::Wrong(e) => return Some(e),
            Check::List(folder) => folder,
        };
        if self.listed.borrow().contains_key(&folder) {
            return None;
        }
        self.listed.borrow_mut().insert(folder.clone(), None);
        let call = self.code.call("fs.list", json!({ "path": folder }));
        let listed = self.listed.clone();
        cx.spawn(async move |cx| {
            // A folder that cannot be listed has nothing in the way.
            let entries: Entries = call
                .await
                .ok()
                .and_then(|v| serde_json::from_value::<Vec<Listing>>(v).ok())
                .unwrap_or_default()
                .into_iter()
                .map(|e| (e.name, e.dir))
                .collect();
            listed.borrow_mut().insert(folder, Some(entries));
            cx.update(again);
        })
        .detach();
        None
    }
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
                .debug_selector(|| "dialog-ok".into())
                .on_click(move |_, window, cx| on_ok(window, cx)),
        )
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
        move |window, cx| trash_paths(&code, vec![path.clone()], window, cx),
    );
}

/// Moves paths to the trash, kept to be undone.
fn trash_paths(code: &Code, paths: Vec<String>, window: &mut Window, cx: &mut App) {
    let c = code.clone();
    run_then(
        code,
        "fs.trash",
        json!({ "paths": paths }),
        window,
        cx,
        move |v, _, cx| {
            if let Some(done) = crate::filehistory::Done::of_trash(&c, &v) {
                crate::filehistory::record(done, cx);
            }
        },
    );
}

/// Moves files and folders to the trash, once asked: one by its name,
/// several by how many.
pub fn trash_all(all: &[Target], window: &mut Window, cx: &mut App) {
    match all {
        [] => {}
        [one] => trash(one, window, cx),
        many => {
            let code = many[0].code.clone();
            let paths: Vec<String> = many.iter().map(|t| t.abs.clone()).collect();
            confirm(
                &format!("Move {} items to the trash?", many.len()),
                "They go to the environment's trash, from where they can be restored.",
                "Move to Trash",
                window,
                cx,
                move |window, cx| trash_paths(&code, paths.clone(), window, cx),
            );
        }
    }
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
    let (code, cut) = (taken.code.clone(), taken.cut);
    run_then(
        &taken.code,
        "fs.transfer",
        json!({ "op": op, "from": taken.paths, "to": t.folder(), "clash": "rename" }),
        window,
        cx,
        move |v, _, cx| {
            if let Some(done) = crate::filehistory::Done::of_transfer(&code, cut, &v) {
                crate::filehistory::record(done, cx);
            }
        },
    );
}

/// Stages the file or folder in its repository.
pub fn stage(t: &Target, window: &mut Window, cx: &mut App) {
    run(&t.code, "git.stage", json!({ "path": t.abs }), window, cx);
}

/// Discards a file's changes, or deletes it when git does not know it,
/// once asked.
pub fn rollback(t: &Target, window: &mut Window, cx: &mut App) {
    let (code, path, name) = (t.code.clone(), t.abs.clone(), t.name().to_string());
    confirm(
        &format!("Roll back {name}?"),
        "Its changes since the last commit are discarded; a file git does not know is deleted.",
        "Roll Back",
        window,
        cx,
        move |window, cx| run(&code, "git.rollback", json!({ "path": path }), window, cx),
    );
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
