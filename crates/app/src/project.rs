//! The Project tool window: the environment's projects (its workspace
//! file's folders), each a tree of its files with what git says of them, or
//! the whole machine's files, through the environment's Workspace, and
//! reloaded as the service says its files and repositories change.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::{
    App, Context, Entity, FocusHandle, Focusable, FontWeight, IntoElement, KeyBinding,
    ParentElement, Render, SharedString, Styled, Window, actions, div, prelude::*, px, svg,
};
use gpui_component::menu::{ContextMenuExt, PopupMenu, PopupMenuItem};
use gpui_component::{h_flex, v_flex};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::assets;
use crate::code::Code;
use crate::find::Scope as FindScope;
use crate::palette::{self, Palette};
use crate::treeops::{self, Taken, Target};
use crate::workspace::{Event, PRIMARY, State, Workspace};

/// What the window shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The environment's projects.
    Projects,
    /// The machine's files, from /.
    Filesystem,
}

/// A file or folder as the service lists it.
#[derive(Debug, Clone, Deserialize)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
    #[serde(default)]
    pub ignored: bool,
    #[serde(default)]
    pub link: bool,
}

/// The folders listed so far, and those open.
#[derive(Default)]
struct Tree {
    /// Each listed folder's entries, folders first, by its path: relative
    /// to a project, or absolute for the machine.
    children: HashMap<String, Vec<Entry>>,
    expanded: HashSet<String>,
    errors: HashMap<String, String>,
}

struct Project {
    root: String,
    tree: Tree,
    /// Each changed file's mark, by its path from the root: M, A, D or !.
    git: HashMap<String, char>,
    open: bool,
}

/// Asks for a file to be opened in the editor: its project's root (or /
/// for the machine's), and its path from there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenFile {
    pub root: String,
    pub path: String,
}

impl gpui::EventEmitter<OpenFile> for ProjectView {}

/// Asks for Find in Files on a folder, or Replace in Files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindIn {
    pub scope: FindScope,
    pub replace: bool,
}

impl gpui::EventEmitter<FindIn> for ProjectView {}

/// Asks for the keyboard to go back to the editor: Escape in the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leave;

impl gpui::EventEmitter<Leave> for ProjectView {}

actions!(
    project_tree,
    [
        SelectUp,
        SelectDown,
        SelectFirst,
        SelectLast,
        PageUp,
        PageDown,
        Expand,
        Collapse,
        Open,
        Delete,
        Rename,
        NewFile,
        Copy,
        Cut,
        Paste,
        CopyPath,
        Escape,
    ]
);

/// The tree's keys, as IntelliJ's Project view has them, while it has the
/// keyboard.
pub fn bind_keys(cx: &mut App) {
    let ctx = Some("ProjectTree");
    // Delete, New File, Copy, Cut, Paste, Copy Path.
    #[cfg(target_os = "macos")]
    let keys = [
        "cmd-backspace",
        "cmd-n",
        "cmd-c",
        "cmd-x",
        "cmd-v",
        "cmd-shift-c",
    ];
    #[cfg(not(target_os = "macos"))]
    let keys = [
        "delete",
        "ctrl-n",
        "ctrl-c",
        "ctrl-x",
        "ctrl-v",
        "ctrl-shift-c",
    ];
    cx.bind_keys([
        KeyBinding::new("up", SelectUp, ctx),
        KeyBinding::new("down", SelectDown, ctx),
        KeyBinding::new("home", SelectFirst, ctx),
        KeyBinding::new("end", SelectLast, ctx),
        KeyBinding::new("pageup", PageUp, ctx),
        KeyBinding::new("pagedown", PageDown, ctx),
        KeyBinding::new("right", Expand, ctx),
        KeyBinding::new("left", Collapse, ctx),
        KeyBinding::new("enter", Open, ctx),
        KeyBinding::new("delete", Delete, ctx),
        KeyBinding::new(keys[0], Delete, ctx),
        KeyBinding::new("shift-f6", Rename, ctx),
        KeyBinding::new(keys[1], NewFile, ctx),
        KeyBinding::new(keys[2], Copy, ctx),
        KeyBinding::new(keys[3], Cut, ctx),
        KeyBinding::new(keys[4], Paste, ctx),
        KeyBinding::new(keys[5], CopyPath, ctx),
        KeyBinding::new("escape", Escape, ctx),
    ]);
}

/// A row as the keys move over it: what selecting it selects, and whether
/// it is a folder (a project's own row among them) and open.
#[derive(Debug, Clone)]
struct Visible {
    sel: Selection,
    project: Option<usize>,
    depth: usize,
    dir: bool,
    open: bool,
}

/// What a click in the tree selects: a project's file (by the project's
/// root and its path) or the machine's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub root: Option<String>,
    pub path: String,
}

pub struct ProjectView {
    workspace: Entity<Workspace>,
    projects: Vec<Project>,
    scope: Scope,
    machine: Tree,
    selected: Option<Selection>,
    /// The tree's scroll, and whether the row selected is still to be
    /// scrolled into view, once it is there.
    scroll: gpui::ScrollHandle,
    reveal: Rc<Cell<bool>>,
    /// What Cut or Copy took, to paste.
    taken: Option<Taken>,
    focus: FocusHandle,
    _sub: gpui::Subscription,
}

impl Focusable for ProjectView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ProjectView {
    /// Shows the workspace's projects, as it connects to them.
    pub fn new(workspace: Entity<Workspace>, cx: &mut Context<Self>) -> Self {
        let sub = cx.subscribe(&workspace, |v, _, e: &Event, cx| v.heard(e, cx));
        let mut view = ProjectView {
            workspace,
            projects: vec![],
            scope: Scope::Projects,
            machine: Tree::default(),
            selected: None,
            scroll: gpui::ScrollHandle::new(),
            reveal: Rc::new(Cell::new(false)),
            taken: None,
            focus: cx.focus_handle(),
            _sub: sub,
        };
        view.sync(cx);
        view
    }

    /// The machine's files alone, from /: the Filesystem tool window.
    pub fn machine(workspace: Entity<Workspace>, cx: &mut Context<Self>) -> Self {
        let mut view = ProjectView::new(workspace, cx);
        view.set_scope(Scope::Filesystem, cx);
        view
    }

    pub fn scope(&self) -> Scope {
        self.scope
    }

    pub fn set_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        self.scope = scope;
        if scope == Scope::Filesystem && !self.machine.children.contains_key("/") {
            self.load_machine("/", cx);
        }
        cx.notify();
    }

    fn heard(&mut self, e: &Event, cx: &mut Context<Self>) {
        match e {
            Event::Projects => self.sync(cx),
            Event::Heard {
                root,
                event,
                params,
            } if event == "fs.changed" && root == PRIMARY => {
                for dir in dirs(params) {
                    if self.machine.children.contains_key(&dir) {
                        self.load_machine(&dir, cx);
                    }
                }
            }
            Event::Heard {
                root,
                event,
                params,
            } if event == "fs.changed" => {
                let Some(i) = self.projects.iter().position(|p| &p.root == root) else {
                    return;
                };
                for dir in dirs(params) {
                    let rel = if &dir == root {
                        Some(String::new())
                    } else {
                        dir.strip_prefix(&format!("{root}/")).map(str::to_string)
                    };
                    if let Some(rel) = rel
                        && self.projects[i].tree.children.contains_key(&rel)
                    {
                        self.load(i, &rel, cx);
                    }
                }
            }
            Event::Heard { root, event, .. } if event == "git.changed" => {
                if let Some(i) = self.projects.iter().position(|p| &p.root == root) {
                    self.git(i, cx);
                }
            }
            _ => {}
        }
    }

    /// Keeps a tree for each of the workspace's projects, in its order,
    /// reading those it has not shown before.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let roots: Vec<String> = self
            .workspace
            .read(cx)
            .projects()
            .iter()
            .map(|f| f.root.clone())
            .collect();
        let mut old = std::mem::take(&mut self.projects);
        for root in roots {
            if let Some(i) = old.iter().position(|p| p.root == root) {
                self.projects.push(old.remove(i));
                continue;
            }
            self.projects.push(Project {
                root,
                tree: Tree::default(),
                git: HashMap::new(),
                open: true,
            });
            let i = self.projects.len() - 1;
            self.load(i, "", cx);
            self.git(i, cx);
        }
        cx.notify();
    }

    fn code(&self, i: usize, cx: &Context<Self>) -> Option<Code> {
        self.workspace.read(cx).code(&self.projects.get(i)?.root)
    }

    /// Lists a project's folder, by its path from the root.
    fn load(&mut self, i: usize, rel: &str, cx: &mut Context<Self>) {
        let Some(code) = self.code(i, cx) else {
            return;
        };
        let call = code.call("fs.list", json!({ "path": rel }));
        let (root, rel) = (self.projects[i].root.clone(), rel.to_string());
        cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |v, cx| {
                let Some(p) = v.projects.iter_mut().find(|p| p.root == root) else {
                    return;
                };
                put(&mut p.tree, &rel, result);
                cx.notify();
            });
        })
        .detach();
    }

    /// Lists a folder of the machine, by its absolute path.
    fn load_machine(&mut self, abs: &str, cx: &mut Context<Self>) {
        let Some(code) = self.workspace.read(cx).primary() else {
            return;
        };
        let call = code.call("fs.list", json!({ "path": abs }));
        let abs = abs.to_string();
        cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |v, cx| {
                put(&mut v.machine, &abs, result);
                cx.notify();
            });
        })
        .detach();
    }

    /// Reads what git says of a project's files.
    fn git(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(code) = self.code(i, cx) else {
            return;
        };
        let call = code.call("git.status", json!({}));
        let root = self.projects[i].root.clone();
        cx.spawn(async move |this, cx| {
            let Ok(status) = call.await else { return };
            let _ = this.update(cx, |v, cx| {
                if let Some(p) = v.projects.iter_mut().find(|p| p.root == root) {
                    p.git = marks(&status);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn toggle_project(&mut self, i: usize, cx: &mut Context<Self>) {
        if let Some(p) = self.projects.get_mut(i) {
            p.open = !p.open;
            cx.notify();
        }
    }

    /// Opens or closes a project's folder, listing it the first time.
    fn toggle(&mut self, i: usize, rel: &str, cx: &mut Context<Self>) {
        let Some(p) = self.projects.get_mut(i) else {
            return;
        };
        if !p.tree.expanded.remove(rel) {
            p.tree.expanded.insert(rel.to_string());
            if !p.tree.children.contains_key(rel) {
                self.load(i, rel, cx);
            }
        }
        cx.notify();
    }

    fn toggle_machine(&mut self, abs: &str, cx: &mut Context<Self>) {
        if !self.machine.expanded.remove(abs) {
            self.machine.expanded.insert(abs.to_string());
            if !self.machine.children.contains_key(abs) {
                self.load_machine(abs, cx);
            }
        }
        cx.notify();
    }

    pub fn selected(&self) -> Option<&Selection> {
        self.selected.as_ref()
    }

    /// The rows shown, in order, as the keys move over them.
    fn visible(&self) -> Vec<Visible> {
        let mut out = vec![];
        let row = |r: Row, root: Option<String>| Visible {
            sel: Selection { root, path: r.path },
            project: r.project,
            depth: r.depth,
            dir: r.dir,
            open: r.open,
        };
        match self.scope {
            Scope::Projects => {
                for (i, proj) in self.projects.iter().enumerate() {
                    out.push(Visible {
                        sel: Selection {
                            root: Some(proj.root.clone()),
                            path: String::new(),
                        },
                        project: Some(i),
                        depth: 0,
                        dir: true,
                        open: proj.open,
                    });
                    if proj.open {
                        let mut rs = vec![];
                        rows(&proj.tree, Some(i), &proj.git, "", 1, &mut rs);
                        out.extend(rs.into_iter().map(|r| row(r, Some(proj.root.clone()))));
                    }
                }
            }
            Scope::Filesystem => {
                let mut rs = vec![];
                rows(&self.machine, None, &HashMap::new(), "/", 0, &mut rs);
                out.extend(rs.into_iter().map(|r| row(r, None)));
            }
        }
        out
    }

    /// Selects the row at an index of the rows shown, scrolling to it.
    fn select_at(&mut self, rows: &[Visible], i: usize, cx: &mut Context<Self>) {
        if let Some(r) = rows.get(i) {
            self.selected = Some(r.sel.clone());
            self.reveal.set(true);
            cx.notify();
        }
    }

    /// Moves the selection by some rows, up for a negative count; with
    /// nothing selected, the first row is.
    fn select_by(&mut self, by: i64, cx: &mut Context<Self>) {
        let rows = self.visible();
        if rows.is_empty() {
            return;
        }
        let at = self.at(&rows);
        let i = match at {
            Some(i) => (i as i64 + by).clamp(0, rows.len() as i64 - 1) as usize,
            None => 0,
        };
        self.select_at(&rows, i, cx);
    }

    fn at(&self, rows: &[Visible]) -> Option<usize> {
        let sel = self.selected.as_ref()?;
        rows.iter().position(|r| &r.sel == sel)
    }

    /// Opens or closes a folder row, a project's own among them.
    fn toggle_row(&mut self, r: &Visible, cx: &mut Context<Self>) {
        match (r.project, r.sel.path.as_str()) {
            (Some(i), "") => self.toggle_project(i, cx),
            (Some(i), path) => self.toggle(i, path, cx),
            (None, path) => self.toggle_machine(path, cx),
        }
    }

    /// Right: a shut folder opens; an open one gives way to its first row.
    fn expand_selected(&mut self, cx: &mut Context<Self>) {
        let rows = self.visible();
        let Some(i) = self.at(&rows) else {
            return self.select_by(0, cx);
        };
        let r = rows[i].clone();
        if r.dir && !r.open {
            self.toggle_row(&r, cx);
        } else if r.dir && rows.get(i + 1).is_some_and(|n| n.depth > r.depth) {
            self.select_at(&rows, i + 1, cx);
        }
    }

    /// Left: an open folder shuts; anything else gives way to its folder.
    fn collapse_selected(&mut self, cx: &mut Context<Self>) {
        let rows = self.visible();
        let Some(i) = self.at(&rows) else {
            return self.select_by(0, cx);
        };
        let r = rows[i].clone();
        if r.dir && r.open {
            self.toggle_row(&r, cx);
        } else if let Some(parent) = rows[..i].iter().rposition(|p| p.depth < r.depth) {
            self.select_at(&rows, parent, cx);
        }
    }

    /// Enter: a file opens in the editor; a folder opens or shuts.
    fn open_selected(&mut self, cx: &mut Context<Self>) {
        let rows = self.visible();
        let Some(i) = self.at(&rows) else {
            return;
        };
        let r = rows[i].clone();
        if r.dir {
            self.toggle_row(&r, cx);
        } else {
            cx.emit(OpenFile {
                root: r.sel.root.clone().unwrap_or_else(|| "/".into()),
                path: r.sel.path.clone(),
            });
        }
    }

    /// What the selected row is, for the menu's actions run from keys.
    fn selected_target(&self, cx: &App) -> Option<Target> {
        let rows = self.visible();
        let r = rows.get(self.at(&rows)?)?;
        self.target(r.sel.root.as_deref(), &r.sel.path, r.dir, cx)
    }

    /// Cut or Copy of the selected row, to paste.
    fn take(&mut self, cut: bool, cx: &mut Context<Self>) {
        if let Some(t) = self.selected_target(cx).filter(|t| !(cut && t.is_root())) {
            self.taken = Some(Taken {
                code: t.code,
                paths: vec![t.abs],
                cut,
            });
        }
    }

    /// How many rows the tree shows at once, for Page Up and Page Down.
    fn page(&self) -> i64 {
        (f32::from(self.scroll.bounds().size.height) / 20.)
            .floor()
            .max(1.) as i64
    }

    /// Shows a file in the tree: the project holding it open (or, in the
    /// machine's files, /), the folders down to it expanded, and it
    /// selected. False when the file is in no project the tree shows.
    pub fn reveal(&mut self, abs: &str, cx: &mut Context<Self>) -> bool {
        let at = self.projects.iter().enumerate().find_map(|(i, p)| {
            let root = p.root.trim_end_matches('/');
            abs.strip_prefix(&format!("{root}/"))
                .map(|rel| (i, p.root.clone(), rel.to_string()))
        });
        match (self.scope, at) {
            (Scope::Projects, None) => return false,
            (Scope::Projects, Some((i, root, rel))) => {
                self.projects[i].open = true;
                let mut dir = String::new();
                for part in rel
                    .split('/')
                    .collect::<Vec<_>>()
                    .split_last()
                    .map_or(&[][..], |(_, d)| d)
                {
                    dir = if dir.is_empty() {
                        part.to_string()
                    } else {
                        format!("{dir}/{part}")
                    };
                    if self.projects[i].tree.expanded.insert(dir.clone())
                        && !self.projects[i].tree.children.contains_key(&dir)
                    {
                        self.load(i, &dir, cx);
                    }
                }
                self.selected = Some(Selection {
                    root: Some(root),
                    path: rel,
                });
            }
            (Scope::Filesystem, _) => {
                let mut dir = String::new();
                for part in abs
                    .trim_start_matches('/')
                    .split('/')
                    .collect::<Vec<_>>()
                    .split_last()
                    .map_or(&[][..], |(_, d)| d)
                {
                    dir = format!("{dir}/{part}");
                    if self.machine.expanded.insert(dir.clone())
                        && !self.machine.children.contains_key(&dir)
                    {
                        self.load_machine(&dir, cx);
                    }
                }
                self.selected = Some(Selection {
                    root: None,
                    path: abs.to_string(),
                });
            }
        }
        self.reveal.set(true);
        cx.notify();
        true
    }

    /// The projects' roots, in order.
    pub fn roots(&self) -> Vec<String> {
        self.projects.iter().map(|p| p.root.clone()).collect()
    }

    /// Opens a project's folders down to rel, listing each, for tests and
    /// for revealing a file.
    pub fn expand(&mut self, root: &str, rel: &str, cx: &mut Context<Self>) {
        let Some(i) = self.projects.iter().position(|p| p.root == root) else {
            return;
        };
        let mut at = String::new();
        for part in rel.split('/').filter(|s| !s.is_empty()) {
            at = if at.is_empty() {
                part.to_string()
            } else {
                format!("{at}/{part}")
            };
            if !self.projects[i].tree.expanded.contains(&at) {
                self.toggle(i, &at.clone(), cx);
            }
        }
    }
}

/// The folders an fs.changed event names, absolute.
fn dirs(v: &Value) -> Vec<String> {
    v["dirs"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|d| d.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Keeps a folder's listing, folders first and then by name, or why it
/// could not be read.
fn put(tree: &mut Tree, path: &str, result: Result<Value, String>) {
    match result.and_then(|v| serde_json::from_value::<Vec<Entry>>(v).map_err(|e| e.to_string())) {
        Ok(mut entries) => {
            // A repository's own store is shown as what git does not track.
            for e in entries.iter_mut().filter(|e| e.dir && e.name == ".git") {
                e.ignored = true;
            }
            entries.sort_by(|a, b| {
                b.dir
                    .cmp(&a.dir)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
            tree.children.insert(path.to_string(), entries);
            tree.errors.remove(path);
        }
        Err(e) => {
            tree.errors.insert(path.to_string(), e);
        }
    }
}

/// Each changed file's mark from git.status: M modified, A added or new,
/// D deleted, ! in conflict.
pub fn marks(status: &Value) -> HashMap<String, char> {
    let mut out = HashMap::new();
    for c in status["changes"].as_array().into_iter().flatten() {
        let Some(path) = c["path"].as_str() else {
            continue;
        };
        let (i, w) = (
            c["index"].as_str().unwrap_or(""),
            c["worktree"].as_str().unwrap_or(""),
        );
        let mark = if i == "U" || w == "U" {
            '!'
        } else if i == "?" || i == "A" || w == "A" {
            'A'
        } else if i == "D" || w == "D" {
            'D'
        } else {
            'M'
        };
        out.insert(path.to_string(), mark);
    }
    out
}

/// One row of the tree as drawn.
struct Row {
    depth: usize,
    name: String,
    dir: bool,
    open: bool,
    ignored: bool,
    mark: Option<char>,
    /// The project's index, or None for the machine's.
    project: Option<usize>,
    path: String,
}

fn rows(
    tree: &Tree,
    project: Option<usize>,
    git: &HashMap<String, char>,
    at: &str,
    depth: usize,
    out: &mut Vec<Row>,
) {
    let Some(entries) = tree.children.get(at) else {
        return;
    };
    for e in entries {
        let path = match (project, at) {
            (Some(_), "") => e.name.clone(),
            (None, "/") => format!("/{}", e.name),
            _ => format!("{at}/{}", e.name),
        };
        let open = e.dir && tree.expanded.contains(&path);
        out.push(Row {
            depth,
            name: e.name.clone(),
            dir: e.dir,
            open,
            ignored: e.ignored,
            mark: git.get(&path).copied(),
            project,
            path: path.clone(),
        });
        if open {
            rows(tree, project, git, &path, depth + 1, out);
        }
    }
}

impl Render for ProjectView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let scope = self.scope;
        let mut list = v_flex();
        // How many rows are in the list so far, and where the selected one
        // is.
        let mut n = 0usize;
        let mut at = None;
        let ws = self.workspace.read(cx);
        let state = ws.state().clone();
        let names: Vec<(String, String)> = self
            .projects
            .iter()
            .map(|proj| {
                let name = ws
                    .projects()
                    .iter()
                    .find(|f| f.root == proj.root)
                    .map(|f| f.name.clone())
                    .unwrap_or_default();
                (name, ws.shown(&proj.root))
            })
            .collect();
        match (&state, scope) {
            (State::Connecting, _) => list = list.child(muted("Connecting…", &p)),
            (State::Reconnecting(why), _) => list = list.child(muted(State::reconnecting(why), &p)),
            (State::Failed(e), _) => {
                list = list.child(muted(format!("Could not reach the Code service: {e}"), &p))
            }
            (State::Ready, Scope::Projects) => {
                if self.projects.is_empty() {
                    list = list.child(muted("The environment has no projects.", &p));
                }
                for (i, proj) in self.projects.iter().enumerate() {
                    // A project's own row is selected as its files are,
                    // with its path from its root empty.
                    let me = Selection {
                        root: Some(proj.root.clone()),
                        path: String::new(),
                    };
                    let selected = self.selected.as_ref() == Some(&me);
                    if selected {
                        at = Some(n);
                    }
                    n += 1;
                    let (on_click, on_right) = (me.clone(), me);
                    list = list.child(
                        h_flex()
                            .id(("project", i))
                            .debug_selector({
                                let root = proj.root.clone();
                                move || format!("project-row-{root}")
                            })
                            .h(px(22.))
                            .gap(px(4.))
                            .px(px(4.))
                            .rounded(px(4.))
                            .cursor_pointer()
                            .when(selected, |d| d.bg(p.selection))
                            .when(!selected, |d| d.hover(|s| s.bg(p.surface_2)))
                            .child(chevron(proj.open, &p))
                            .child(
                                svg()
                                    .path(assets::icon("folder"))
                                    .size(px(14.))
                                    .flex_none()
                                    .text_color(p.accent),
                            )
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(names[i].0.clone()),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(p.muted)
                                    .child(names[i].1.clone()),
                            )
                            .on_click(cx.listener(move |v, _, window, cx| {
                                window.focus(&v.focus, cx);
                                v.selected = Some(on_click.clone());
                                v.toggle_project(i, cx)
                            }))
                            .on_mouse_down(
                                gpui::MouseButton::Right,
                                cx.listener(move |v, _, _, cx| {
                                    v.selected = Some(on_right.clone());
                                    cx.notify();
                                }),
                            )
                            .context_menu({
                                let t = self.target(Some(&proj.root), "", true, cx);
                                let view = cx.entity().downgrade();
                                move |menu, _, cx| match &t {
                                    Some(t) => {
                                        crate::ui::modal(cx);
                                        tree_menu(menu, t, &view, cx)
                                    }
                                    None => menu,
                                }
                            }),
                    );
                    if proj.open {
                        let mut out = Vec::new();
                        rows(&proj.tree, Some(i), &proj.git, "", 1, &mut out);
                        if let Some(e) = proj.tree.errors.get("") {
                            n += 1;
                            list = list.child(muted(e.clone(), &p));
                        }
                        for r in out {
                            if self.is_selected(&r) {
                                at = Some(n);
                            }
                            n += 1;
                            list = list.child(self.row(r, &p, cx));
                        }
                    }
                }
            }
            (State::Ready, Scope::Filesystem) => {
                let mut out = Vec::new();
                rows(&self.machine, None, &HashMap::new(), "/", 0, &mut out);
                if out.is_empty() {
                    list = list.child(muted(
                        self.machine
                            .errors
                            .get("/")
                            .cloned()
                            .unwrap_or("Reading /…".into()),
                        &p,
                    ));
                }
                for r in out {
                    if self.is_selected(&r) {
                        at = Some(n);
                    }
                    n += 1;
                    list = list.child(self.row(r, &p, cx));
                }
            }
        }
        // A row revealed is scrolled into view once it is laid out, which
        // may be after its folder has been listed.
        let (handle, reveal) = (self.scroll.clone(), self.reveal.clone());
        let list = list.on_children_prepainted(move |rows, window, _| {
            let (Some(i), true) = (at, reveal.get()) else {
                return;
            };
            let Some(row) = rows.get(i) else {
                return;
            };
            reveal.set(false);
            let view = handle.bounds();
            let mut offset = handle.offset();
            if row.top() < view.top() {
                offset.y += view.top() - row.top();
            } else if row.bottom() > view.bottom() {
                offset.y -= row.bottom() - view.bottom();
            } else {
                return;
            }
            handle.set_offset(offset);
            window.request_animation_frame();
        });
        v_flex()
            .size_full()
            .min_h_0()
            .text_size(px(12.))
            .key_context("ProjectTree")
            .track_focus(&self.focus)
            .on_action(cx.listener(|v, _: &SelectUp, _, cx| v.select_by(-1, cx)))
            .on_action(cx.listener(|v, _: &SelectDown, _, cx| v.select_by(1, cx)))
            .on_action(cx.listener(|v, _: &SelectFirst, _, cx| v.select_by(i64::MIN / 2, cx)))
            .on_action(cx.listener(|v, _: &SelectLast, _, cx| v.select_by(i64::MAX / 2, cx)))
            .on_action(cx.listener(|v, _: &PageUp, _, cx| v.select_by(-v.page(), cx)))
            .on_action(cx.listener(|v, _: &PageDown, _, cx| v.select_by(v.page(), cx)))
            .on_action(cx.listener(|v, _: &Expand, _, cx| v.expand_selected(cx)))
            .on_action(cx.listener(|v, _: &Collapse, _, cx| v.collapse_selected(cx)))
            .on_action(cx.listener(|v, _: &Open, _, cx| v.open_selected(cx)))
            .on_action(cx.listener(|_, _: &Escape, _, cx| cx.emit(Leave)))
            .on_action(cx.listener(|v, _: &Delete, window, cx| {
                if let Some(t) = v.selected_target(cx).filter(|t| !t.is_root()) {
                    treeops::trash(&t, window, cx);
                }
            }))
            .on_action(cx.listener(|v, _: &Rename, window, cx| {
                if let Some(t) = v.selected_target(cx).filter(|t| !t.is_root()) {
                    let view = cx.entity().downgrade();
                    treeops::rename(&t, window, cx, move |abs, _, cx| {
                        let _ = view.update(cx, |v, cx| v.reveal(&abs, cx));
                    });
                }
            }))
            .on_action(cx.listener(|v, _: &NewFile, window, cx| {
                if let Some(t) = v.selected_target(cx) {
                    let view = cx.entity().downgrade();
                    let root = t.root.clone();
                    treeops::new_file(&t, window, cx, move |abs, _, cx| {
                        let path = match &root {
                            Some(r) => abs
                                .strip_prefix(&format!("{}/", r.trim_end_matches('/')))
                                .unwrap_or(&abs)
                                .to_string(),
                            None => abs.clone(),
                        };
                        let root = root.clone().unwrap_or_else(|| "/".into());
                        let _ = view.update(cx, |v, cx| {
                            v.reveal(&abs, cx);
                            cx.emit(OpenFile { root, path })
                        });
                    });
                }
            }))
            .on_action(cx.listener(|v, _: &Copy, _, cx| v.take(false, cx)))
            .on_action(cx.listener(|v, _: &Cut, _, cx| v.take(true, cx)))
            .on_action(cx.listener(|v, _: &Paste, window, cx| {
                if let (Some(taken), Some(t)) = (v.taken.clone(), v.selected_target(cx)) {
                    treeops::paste(&taken, &t, window, cx);
                    if taken.cut {
                        v.taken = None;
                    }
                }
            }))
            .on_action(cx.listener(|v, _: &CopyPath, _, cx| {
                if let Some(t) = v.selected_target(cx) {
                    treeops::copy(t.abs.clone(), cx);
                }
            }))
            .child(
                scroll("project-tree", list)
                    .track_scroll(&self.scroll)
                    .flex_1()
                    .min_h_0(),
            )
    }
}

fn muted(text: impl Into<SharedString>, p: &Palette) -> impl IntoElement {
    div()
        .px(px(4.))
        .py(px(2.))
        .text_size(px(11.5))
        .text_color(p.muted)
        .child(text.into())
}

fn chevron(open: bool, p: &Palette) -> impl IntoElement {
    svg()
        .path(assets::icon(if open {
            "chevron-down"
        } else {
            "chevron-right"
        }))
        .size(px(10.))
        .flex_none()
        .text_color(p.muted)
}

/// A row's icon: a folder, open or shut, or a file.
fn kind_icon(dir: bool, open: bool, p: &Palette) -> impl IntoElement {
    let (name, colour) = match (dir, open) {
        (true, true) => ("folder-open", p.folder),
        (true, false) => ("folder", p.folder),
        _ => ("file", p.muted),
    };
    svg()
        .path(assets::icon(name))
        .size(px(14.))
        .flex_none()
        .text_color(colour)
}

impl ProjectView {
    /// What the menu acts on for a row: a project's entry by its path from
    /// the root ("" the project itself), or the machine's by its absolute
    /// path, through the connection that reaches it.
    fn target(&self, root: Option<&str>, path: &str, dir: bool, cx: &gpui::App) -> Option<Target> {
        let ws = self.workspace.read(cx);
        let (code, abs) = match root {
            Some(root) if path.is_empty() => (ws.code(root)?, root.to_string()),
            Some(root) => (ws.code(root)?, treeops::join(root, path)),
            None => (ws.primary()?, path.to_string()),
        };
        Some(Target {
            code,
            root: root.map(str::to_string),
            abs,
            dir,
        })
    }

    fn is_selected(&self, r: &Row) -> bool {
        let root = r
            .project
            .and_then(|i| self.projects.get(i))
            .map(|p| p.root.as_str());
        self.selected
            .as_ref()
            .is_some_and(|s| s.root.as_deref() == root && s.path == r.path)
    }

    fn row(&self, r: Row, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let root = r
            .project
            .and_then(|i| self.projects.get(i))
            .map(|p| p.root.clone());
        let sel = Selection {
            root: root.clone(),
            path: r.path.clone(),
        };
        let selected = self.selected.as_ref() == Some(&sel);
        let right = sel.clone();
        let colour = match r.mark {
            Some('M') => p.busy,
            Some('A') => p.good,
            Some('D') | Some('!') => p.bad,
            _ if r.ignored => p.muted,
            _ => p.text,
        };
        let (project, path, dir) = (r.project, r.path.clone(), r.dir);
        let target = self.target(root.as_deref(), &r.path, r.dir, cx);
        let view = cx.entity().downgrade();
        let id = SharedString::from(format!("row-{}-{}", root.as_deref().unwrap_or("/"), r.path));
        h_flex()
            .id(id)
            .h(px(20.))
            .gap(px(4.))
            .pl(px(4. + 12. * r.depth as f32))
            .pr(px(6.))
            .rounded(px(4.))
            .cursor_pointer()
            .when(selected, |d| d.bg(p.selection))
            .when(!selected, |d| d.hover(|s| s.bg(p.surface_2)))
            .child(if r.dir {
                chevron(r.open, p).into_any_element()
            } else {
                div().w(px(10.)).flex_none().into_any_element()
            })
            .child(kind_icon(r.dir, r.open, p))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_color(colour)
                    .child(r.name.clone()),
            )
            .when_some(r.mark, |d, m| {
                d.child(
                    div()
                        .text_size(px(10.5))
                        .text_color(colour)
                        .child(m.to_string()),
                )
            })
            .debug_selector({
                let path = r.path.clone();
                move || format!("tree-row-{path}")
            })
            .on_click(cx.listener(move |v, e: &gpui::ClickEvent, window, cx| {
                window.focus(&v.focus, cx);
                v.selected = Some(sel.clone());
                if dir {
                    match project {
                        Some(i) => v.toggle(i, &path, cx),
                        None => v.toggle_machine(&path, cx),
                    }
                } else if e.click_count() >= 2 {
                    cx.emit(OpenFile {
                        root: sel.root.clone().unwrap_or_else(|| "/".into()),
                        path: path.clone(),
                    });
                }
                cx.notify();
            }))
            // A right-click selects the row, so it is clear what the menu
            // is about.
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(move |v, _, _, cx| {
                    v.selected = Some(right.clone());
                    cx.notify();
                }),
            )
            .context_menu(move |menu, _, cx| match &target {
                Some(t) => {
                    crate::ui::modal(cx);
                    tree_menu(menu, t, &view, cx)
                }
                None => menu,
            })
    }
}

/// Selects what was made at a path in the tree.
type Reveal = Box<dyn Fn(String, &mut Window, &mut gpui::App)>;

/// The Project window's menu for a file or folder, as IntelliJ's: making,
/// clipboard, paths, renaming and the trash, finding in a folder, and git.
fn tree_menu(
    menu: PopupMenu,
    t: &Target,
    view: &gpui::WeakEntity<ProjectView>,
    cx: &gpui::App,
) -> PopupMenu {
    let item = |label: &str, t: &Target, f: fn(&Target, &mut Window, &mut gpui::App)| {
        let t = t.clone();
        PopupMenuItem::new(label.to_string()).on_click(move |_, window, cx| f(&t, window, cx))
    };
    let new_file = {
        let (t, view) = (t.clone(), view.clone());
        PopupMenuItem::new("New File…").on_click(move |_, window, cx| {
            let (root, view) = (t.root.clone(), view.clone());
            treeops::new_file(&t, window, cx, move |abs, _, cx| {
                let path = match &root {
                    Some(r) => abs
                        .strip_prefix(&format!("{}/", r.trim_end_matches('/')))
                        .unwrap_or(&abs)
                        .to_string(),
                    None => abs.clone(),
                };
                let root = root.clone().unwrap_or_else(|| "/".into());
                let _ = view.update(cx, |v, cx| {
                    v.reveal(&abs, cx);
                    cx.emit(OpenFile { root, path })
                });
            });
        })
    };
    // What makes or renames something, then selects it in the tree.
    let made = |label: &str, f: fn(&Target, &mut Window, &mut gpui::App, Reveal)| {
        let (t, view) = (t.clone(), view.clone());
        PopupMenuItem::new(label.to_string()).on_click(move |_, window, cx| {
            let view = view.clone();
            f(
                &t,
                window,
                cx,
                Box::new(move |abs, _, cx| {
                    let _ = view.update(cx, |v, cx| v.reveal(&abs, cx));
                }),
            )
        })
    };
    let take = |label: &str, cut: bool| {
        let (t, view) = (t.clone(), view.clone());
        PopupMenuItem::new(label.to_string()).on_click(move |_, _, cx| {
            let taken = Taken {
                code: t.code.clone(),
                paths: vec![t.abs.clone()],
                cut,
            };
            let _ = view.update(cx, |v, _| v.taken = Some(taken));
        })
    };
    let taken = view.upgrade().and_then(|v| v.read(cx).taken.clone());
    let paste = {
        let t = t.clone();
        let view = view.clone();
        PopupMenuItem::new("Paste")
            .disabled(taken.is_none())
            .on_click(move |_, window, cx| {
                let Some(taken) = view.upgrade().and_then(|v| v.read(cx).taken.clone()) else {
                    return;
                };
                treeops::paste(&taken, &t, window, cx);
                // What was cut is moved once; what was copied can be pasted
                // again.
                if taken.cut {
                    let _ = view.update(cx, |v, _| v.taken = None);
                }
            })
    };
    let copy_path = |label: &str, text: String| {
        PopupMenuItem::new(label.to_string())
            .on_click(move |_, _, cx| treeops::copy(text.clone(), cx))
    };
    let find = |label: &str, replace: bool| {
        let (t, view) = (t.clone(), view.clone());
        let root = t.root.clone();
        PopupMenuItem::new(label.to_string())
            .disabled(root.is_none())
            .on_click(move |_, _, cx| {
                let Some(root) = t.root.clone() else { return };
                let dir = Target {
                    dir: true,
                    abs: t.folder(),
                    ..t.clone()
                }
                .relative();
                let scope = if t.folder() == root {
                    FindScope::Project(root)
                } else {
                    FindScope::Folder { root, dir }
                };
                let _ = view.update(cx, |_, cx| cx.emit(FindIn { scope, replace }));
            })
    };
    let is_root = t.is_root();
    let in_project = t.root.is_some();
    let mut menu = menu
        .item(new_file)
        .item(made("New Folder…", treeops::new_folder))
        .separator()
        .item(take("Cut", true).disabled(is_root))
        .item(take("Copy", false))
        .item(paste)
        .separator()
        .item(copy_path("Copy Absolute Path", t.abs.clone()))
        .item(copy_path("Copy Path From Project Root", t.relative()).disabled(!in_project))
        .item(copy_path("Copy File Name", t.name().to_string()))
        .separator()
        .item(made("Rename…", treeops::rename).disabled(is_root))
        .item(item("Delete…", t, treeops::trash).disabled(is_root))
        .separator()
        .item(find("Find in Folder…", false))
        .item(find("Replace in Folder…", true));
    if in_project {
        menu = menu
            .separator()
            .item(item("Git: Stage", t, treeops::stage))
            .item(item("Git: Add to .gitignore", t, treeops::ignore).disabled(is_root));
    }
    // The workspace's projects: a folder added, a project removed or moved.
    let ws = view.upgrade().map(|v| v.read(cx).workspace.clone());
    let change = |label: &str, change: treeops::Reorder| {
        let ws = ws.clone();
        PopupMenuItem::new(label.to_string()).on_click(move |_, window, cx| {
            if let Some(ws) = &ws {
                treeops::change_workspace(ws, change.clone(), window, cx);
            }
        })
    };
    if is_root {
        let root = t.abs.clone();
        menu = menu
            .separator()
            .item(change("Move Up", treeops::Reorder::Move(root.clone(), -1)))
            .item(change("Move Down", treeops::Reorder::Move(root.clone(), 1)))
            .item(change(
                "Remove from Workspace",
                treeops::Reorder::Remove(root),
            ))
            .item(item("Trust This Folder", t, treeops::trust));
    } else if t.dir {
        menu = menu.separator().item(change(
            "Add to Workspace",
            treeops::Reorder::Add(t.abs.clone()),
        ));
    }
    menu
}

/// A box that scrolls its rows, which keep their own size however many
/// there are: a column the height of its content, inside the box.
pub fn scroll(id: &'static str, rows: gpui::Div) -> gpui::Stateful<gpui::Div> {
    div().id(id).overflow_y_scroll().child(rows)
}
