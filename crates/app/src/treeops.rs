//! What the Project window's menu does to files, as IntelliJ's Project view
//! does: making files and folders, renaming, moving to the trash, cutting,
//! copying and pasting, and copying paths, through the Code service's fs.*
//! calls. Names are asked for in IntelliJ's small pop-up; the tree follows
//! the files as they change (fs.changed), so nothing here redraws it.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use gpui::{
    Anchor, AnyWindowHandle, App, AppContext, ClipboardItem, Entity, Focusable, FontWeight,
    IntoElement, ParentElement, Styled, Window, anchored, deferred, div, prelude::*, px,
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
/// What a name typed does, answered with the path it made.
type Doing = Pin<Box<dyn Future<Output = Result<String, String>>>>;
/// What a name typed is for; None when there is nothing to do.
type Act = Rc<dyn Fn(&str) -> Option<Doing>>;
/// What is done with the path made once it is done.
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
        None,
        Rc::new(RefCell::new(Asking::default())),
        window,
        cx,
        Rc::new(move |name, window, cx| {
            window.close_dialog(cx);
            done(name, window, cx);
        }),
    );
}

/// Asks for a name as ask_name does, checking it as it is typed, then does
/// what it is for; the pop-up stays until that is done, saying what went
/// wrong over the name, so it can be changed and tried again. Once done,
/// done has the path made.
fn ask_doing(
    title: &str,
    initial: &str,
    checks: Checks,
    window: &mut Window,
    cx: &mut App,
    act: impl Fn(&str) -> Option<Doing> + 'static,
    done: impl Fn(String, &mut Window, &mut App) + 'static,
) {
    let (act, done): (Act, Made) = (Rc::new(act), Rc::new(done));
    let state = Rc::new(RefCell::new(Asking::default()));
    let shown = state.clone();
    popup(
        title,
        initial,
        Some(checks),
        shown,
        window,
        cx,
        Rc::new(move |name, window, cx| {
            let Some(doing) = act(&name) else {
                window.close_dialog(cx);
                return;
            };
            state.borrow_mut().busy = true;
            let (state, done) = (state.clone(), done.clone());
            let handle = window.window_handle();
            cx.spawn(async move |cx| {
                let result = doing.await;
                let _ = handle.update(cx, |_, window, cx| {
                    state.borrow_mut().busy = false;
                    match result {
                        Ok(path) => {
                            window.close_dialog(cx);
                            done(path, window, cx);
                        }
                        Err(e) => {
                            state.borrow_mut().wrong = Some(e);
                            window.refresh();
                        }
                    }
                });
            })
            .detach();
        }),
    );
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
fn parent_of(path: &str) -> String {
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

/// How a pop-up checks names as they are typed: against the folder they go
/// in, through its connection; own is what is renamed, which may keep its
/// name, and nested whether a name may name folders on the way.
#[derive(Clone)]
struct Checks {
    code: Code,
    folder: String,
    own: Option<String>,
    nested: bool,
}

/// A pop-up's state: what is wrong with the name as typed, or as last
/// tried, and whether that try is under way.
#[derive(Default)]
struct Asking {
    wrong: Option<String>,
    busy: bool,
}

type AskState = Rc<RefCell<Asking>>;

/// The folders a pop-up has listed, None while asked.
type Listed = Rc<RefCell<HashMap<String, Option<Entries>>>>;

/// What fs.list says of an entry, as far as checking a name needs.
#[derive(Deserialize)]
struct Listing {
    name: String,
    #[serde(default)]
    dir: bool,
}

/// Checks the name as it stands, listing a folder it needs and checking
/// again once it is listed.
fn recheck(
    field: &Entity<InputState>,
    state: &AskState,
    checks: &Checks,
    listed: &Listed,
    handle: AnyWindowHandle,
    cx: &mut App,
) {
    let name = field.read(cx).value().trim().to_string();
    let found = if !checks.nested && name.contains('/') {
        Check::Wrong("A name cannot contain “/”".into())
    } else {
        let l = listed.borrow();
        check(&checks.folder, &name, checks.own.as_deref(), &|f| {
            l.get(f).cloned().flatten()
        })
    };
    let folder = match found {
        Check::Fine => {
            state.borrow_mut().wrong = None;
            return;
        }
        Check::Wrong(e) => {
            state.borrow_mut().wrong = Some(e);
            return;
        }
        Check::List(folder) => folder,
    };
    state.borrow_mut().wrong = None;
    if listed.borrow().contains_key(&folder) {
        return;
    }
    listed.borrow_mut().insert(folder.clone(), None);
    let call = checks.code.call("fs.list", json!({ "path": folder }));
    let (field, state, checks, listed) =
        (field.clone(), state.clone(), checks.clone(), listed.clone());
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
        let _ = handle.update(cx, |_, window, cx| {
            recheck(&field, &state, &checks, &listed, handle, cx);
            window.refresh();
        });
    })
    .detach();
}

/// The pop-up, as IntelliJ's New File: a title, the name in a plain field
/// across it, and what is wrong with the name in a balloon over it.
fn popup(
    title: &str,
    initial: &str,
    checks: Option<Checks>,
    state: AskState,
    window: &mut Window,
    cx: &mut App,
    take: WithName,
) {
    // A name given is selected up to its extension, as IntelliJ's Rename
    // selects it.
    let stem = match initial.rfind('.') {
        Some(dot) if dot > 0 => dot,
        _ => initial.len(),
    };
    let input = cx.new(|cx| {
        let mut s = InputState::new(window, cx).default_value(initial.to_string());
        s.set_selected_range(0..stem, cx);
        s
    });
    let handle = window.window_handle();
    let listed: Listed = Rc::default();
    if let Some(checks) = &checks {
        // The folder is listed as the pop-up opens, so the first name typed
        // is checked at once.
        recheck(&input, &state, checks, &listed, handle, cx);
    }
    {
        let (take, state) = (take.clone(), state.clone());
        cx.subscribe(&input, move |field, e: &InputEvent, cx| match e {
            InputEvent::PressEnter { .. } => {
                let name = field.read(cx).value().trim().to_string();
                let s = state.borrow();
                // A name said to be wrong is not taken, as IntelliJ's is not.
                if name.is_empty() || s.wrong.is_some() || s.busy {
                    return;
                }
                drop(s);
                let take = take.clone();
                let _ = handle.update(cx, |_, window, cx| take(name, window, cx));
            }
            InputEvent::Change => {
                state.borrow_mut().wrong = None;
                if let Some(checks) = &checks {
                    recheck(&field, &state, checks, &listed, handle, cx);
                }
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
        let wrong = state.borrow().wrong.clone();
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
                            .relative()
                            .h(px(32.))
                            .pl(px(1.))
                            .pr(px(4.))
                            .bg(p.surface)
                            .border_t_1()
                            .border_b_1()
                            .border_color(p.border)
                            .when(wrong.is_some(), |d| d.border_1().border_color(p.bad))
                            .child(div().flex_1().child(Input::new(&input).appearance(false)))
                            .when_some(wrong, |d, e| d.child(balloon(e, &p))),
                    ),
            )
    });
}

/// What is wrong with a name, in a balloon just over its field, as
/// IntelliJ shows it; drawn over the pop-up's title and past its edge.
fn balloon(text: String, p: &palette::Palette) -> impl IntoElement {
    div().absolute().top(px(-5.)).left(px(6.)).child(
        deferred(
            anchored().anchor(Anchor::BottomLeft).child(
                div().rounded(px(5.)).bg(p.bg).child(
                    div()
                        .debug_selector(|| "name-wrong".into())
                        .max_w(px(420.))
                        .px(px(8.))
                        .py(px(4.))
                        .rounded(px(5.))
                        .border_1()
                        .border_color(p.bad)
                        .bg(p.bad.opacity(0.08))
                        .text_size(px(12.))
                        .text_color(p.text)
                        .child(text),
                ),
            ),
        )
        .with_priority(BALLOON),
    )
}

/// Over the dialog the balloon is in, and over the app's menus.
const BALLOON: usize = 101;

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

/// Makes an empty file, asking its name as IntelliJ's New File does: a
/// name with slashes makes the folders on the way. opened has its path.
pub fn new_file(
    t: &Target,
    window: &mut Window,
    cx: &mut App,
    opened: impl Fn(String, &mut Window, &mut App) + 'static,
) {
    let (code, folder) = (t.code.clone(), t.folder());
    let checks = Checks {
        code: code.clone(),
        folder: folder.clone(),
        own: None,
        nested: true,
    };
    ask_doing(
        "New File",
        "",
        checks,
        window,
        cx,
        move |name| {
            let (code, path) = (code.clone(), place(&folder, name));
            let parent = parent_of(&path);
            let way = (parent != place(&folder, "")).then_some(parent);
            Some(Box::pin(async move {
                if let Some(way) = way {
                    code.call("fs.mkdir", json!({ "path": way })).await?;
                }
                code.call("fs.create", json!({ "path": path })).await?;
                Ok(path)
            }))
        },
        opened,
    );
}

/// Makes a directory, asking its name as IntelliJ's New Directory does,
/// then does made with its path; a name with slashes makes each folder on
/// the way.
pub fn new_folder(
    t: &Target,
    window: &mut Window,
    cx: &mut App,
    made: impl Fn(String, &mut Window, &mut App) + 'static,
) {
    let (code, folder) = (t.code.clone(), t.folder());
    let checks = Checks {
        code: code.clone(),
        folder: folder.clone(),
        own: None,
        nested: true,
    };
    ask_doing(
        "New Directory",
        "",
        checks,
        window,
        cx,
        move |name| {
            let (code, path) = (code.clone(), place(&folder, name));
            Some(Box::pin(async move {
                code.call("fs.mkdir", json!({ "path": path })).await?;
                Ok(path)
            }))
        },
        made,
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
    let parent = parent_of(&from);
    let checks = Checks {
        code: code.clone(),
        folder: parent.clone(),
        own: Some(from.clone()),
        nested: false,
    };
    ask_doing(
        "Rename",
        t.name(),
        checks,
        window,
        cx,
        move |name| {
            let (code, from, to) = (code.clone(), from.clone(), join(&parent, name));
            (to != from).then(|| -> Doing {
                Box::pin(async move {
                    code.call("fs.rename", json!({ "from": from, "to": to }))
                        .await?;
                    Ok(to)
                })
            })
        },
        renamed,
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
                move |window, cx| run(&code, "fs.trash", json!({ "paths": paths }), window, cx),
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
