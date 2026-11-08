//! The Project tool window: the environment's projects (its workspace
//! file's folders), each a tree of its files with what git says of them, or
//! the whole machine's files, through the environment's Workspace, and
//! reloaded as the service says its files and repositories change.

use std::collections::{HashMap, HashSet};

use gpui::{
    Context, Entity, FontWeight, IntoElement, ParentElement, Render, SharedString, Styled, Window,
    div, prelude::*, px, svg,
};
use gpui_component::{h_flex, v_flex};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::assets;
use crate::code::Code;
use crate::palette::{self, Palette};
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
    _sub: gpui::Subscription,
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
            _sub: sub,
        };
        view.sync(cx);
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
        let seg = |id: &'static str, label: &'static str, s: Scope, cx: &mut Context<Self>| {
            let on = scope == s;
            div()
                .id(id)
                .px(px(6.))
                .cursor_pointer()
                .when(on, |d| d.bg(p.accent_soft).text_color(p.accent))
                .when(!on, |d| d.text_color(p.muted))
                .child(label)
                .on_click(cx.listener(move |v, _, _, cx| v.set_scope(s, cx)))
        };
        let switch = h_flex()
            .mb(px(6.))
            .self_start()
            .rounded(px(5.))
            .border_1()
            .border_color(p.border)
            .overflow_hidden()
            .text_size(px(11.))
            .child(seg("scope-projects", "Projects", Scope::Projects, cx))
            .child(div().w(px(1.)).h_full().bg(p.border))
            .child(seg("scope-machine", "Filesystem", Scope::Filesystem, cx));

        let mut list = v_flex()
            .id("project-tree")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll();
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
                    list = list.child(
                        h_flex()
                            .id(("project", i))
                            .h(px(22.))
                            .gap(px(4.))
                            .px(px(4.))
                            .rounded(px(4.))
                            .cursor_pointer()
                            .hover(|s| s.bg(p.surface_2))
                            .child(chevron(proj.open, &p))
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
                            .on_click(cx.listener(move |v, _, _, cx| v.toggle_project(i, cx))),
                    );
                    if proj.open {
                        let mut out = Vec::new();
                        rows(&proj.tree, Some(i), &proj.git, "", 1, &mut out);
                        if let Some(e) = proj.tree.errors.get("") {
                            list = list.child(muted(e.clone(), &p));
                        }
                        for r in out {
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
                    list = list.child(self.row(r, &p, cx));
                }
            }
        }
        v_flex()
            .size_full()
            .min_h_0()
            .text_size(px(12.))
            .child(switch)
            .child(list)
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

impl ProjectView {
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
        let colour = match r.mark {
            Some('M') => p.busy,
            Some('A') => p.good,
            Some('D') | Some('!') => p.bad,
            _ if r.ignored => p.muted,
            _ => p.text,
        };
        let (project, path, dir) = (r.project, r.path.clone(), r.dir);
        let id = SharedString::from(format!("row-{}-{}", root.as_deref().unwrap_or("/"), r.path));
        h_flex()
            .id(id)
            .h(px(20.))
            .gap(px(4.))
            .pl(px(4. + 12. * r.depth as f32))
            .pr(px(6.))
            .rounded(px(4.))
            .cursor_pointer()
            .when(selected, |d| d.bg(p.sidebar_active))
            .when(!selected, |d| d.hover(|s| s.bg(p.surface_2)))
            .child(if r.dir {
                chevron(r.open, p).into_any_element()
            } else {
                div().w(px(10.)).flex_none().into_any_element()
            })
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
            .on_click(cx.listener(move |v, e: &gpui::ClickEvent, _, cx| {
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
    }
}
