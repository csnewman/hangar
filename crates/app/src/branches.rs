//! The branch widget in an environment's toolbar, as IntelliJ's: the branch
//! a project's repository is on, and its pop-up, which switches to another
//! branch, local or remote, makes one, and holds Update Project, Commit…
//! and Push. Typing narrows the list, as the web's branch picker does.

use std::collections::HashMap;

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, KeyDownEvent,
    ParentElement, Render, SharedString, Styled, Subscription, Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::notification::Notification;
use gpui_component::popover::Popover;
use gpui_component::{Sizable, WindowExt, h_flex, v_flex};
use serde::Deserialize;
use serde_json::json;

use crate::commit::{Repo, repos};
use crate::palette::{self, Palette};
use crate::workspace::{Event, Workspace};

/// What git.branches gives: the branch checked out, the local branches,
/// most recently committed to first, and the remote ones.
#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct Branches {
    #[serde(default)]
    pub current: Option<String>,
    #[serde(default)]
    pub local: Vec<Branch>,
    #[serde(default)]
    pub remote: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Branch {
    pub name: String,
    #[serde(default)]
    pub upstream: Option<String>,
    #[serde(default)]
    pub ahead: Option<i64>,
    #[serde(default)]
    pub behind: Option<i64>,
}

/// What a row of the pop-up does.
#[derive(Debug, Clone, PartialEq)]
pub enum Do {
    /// Update Project: the branch pulled, fast-forward only.
    Update,
    /// Commit…: the Commit window, its message focused.
    Commit,
    Push,
    /// A new branch from where the repository is, named, or asked for.
    NewBranch(Option<String>),
    Checkout(String),
    /// Another of the projects' repositories, by its project and path.
    Repository(String, String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    Heading(&'static str),
    Item {
        label: String,
        /// Ahead and behind what it tracks, or a repository's branch.
        hint: Option<String>,
        /// The branch checked out, or the repository shown.
        current: bool,
        does: Do,
    },
    /// How many more match than are listed.
    More(usize),
}

impl Row {
    pub fn does(&self) -> Option<&Do> {
        match self {
            Row::Item { does, .. } => Some(does),
            _ => None,
        }
    }
}

/// How many of each kind of branch are listed at once; typing narrows them.
const SHOWN: usize = 8;

/// The pop-up's rows for what is typed: the actions, then the local
/// branches and the remote ones, best match first and each list cut short.
/// Typed, the actions that match stay, and New Branch takes the name typed
/// when no local branch has it.
pub fn rows(query: &str, b: &Branches) -> Vec<Row> {
    let q = query.trim();
    let mut out = vec![];
    let action = |label: &str, does: Do| Row::Item {
        label: label.into(),
        hint: None,
        current: false,
        does,
    };
    for (label, does) in [
        ("Update Project", Do::Update),
        ("Commit…", Do::Commit),
        ("Push", Do::Push),
    ] {
        if crate::fuzzy::score(q, label).is_some() {
            out.push(action(label, does));
        }
    }
    if q.is_empty() {
        out.push(action("New Branch…", Do::NewBranch(None)));
    } else if !b.local.iter().any(|l| l.name == q) {
        out.push(action(
            &format!("New Branch “{q}”"),
            Do::NewBranch(Some(q.to_string())),
        ));
    }
    let mut group = |heading: &'static str, names: Vec<(String, Option<String>)>| {
        let mut found: Vec<(i64, usize, String, Option<String>)> = names
            .into_iter()
            .enumerate()
            .filter_map(|(i, (name, hint))| {
                crate::fuzzy::score(q, &name).map(|(s, _)| (s, i, name, hint))
            })
            .collect();
        if found.is_empty() {
            return;
        }
        found.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        out.push(Row::Heading(heading));
        let more = found.len().saturating_sub(SHOWN);
        for (_, _, name, hint) in found.into_iter().take(SHOWN) {
            out.push(Row::Item {
                current: b.current.as_deref() == Some(&name) && heading == "Local",
                label: name.clone(),
                hint,
                does: Do::Checkout(name),
            });
        }
        if more > 0 {
            out.push(Row::More(more));
        }
    };
    group(
        "Local",
        b.local
            .iter()
            .map(|l| {
                let hint = match (l.ahead.unwrap_or(0), l.behind.unwrap_or(0)) {
                    (0, 0) => None,
                    (a, 0) => Some(format!("↑{a}")),
                    (0, d) => Some(format!("↓{d}")),
                    (a, d) => Some(format!("↑{a} ↓{d}")),
                };
                (l.name.clone(), hint)
            })
            .collect(),
    );
    group(
        "Remote",
        b.remote.iter().map(|r| (r.clone(), None)).collect(),
    );
    out
}

/// Where the selection starts: with something typed, on the first branch
/// that matches, as that is what is looked for; else the first row.
pub fn first(rows: &[Row], query: &str) -> Option<usize> {
    let item = |r: &Row| r.does().is_some();
    let branch = |r: &Row| matches!(r.does(), Some(Do::Checkout(_)));
    if !query.trim().is_empty()
        && let Some(i) = rows.iter().position(branch)
    {
        return Some(i);
    }
    rows.iter().position(item)
}

/// Asks the Commit window to open, its message focused.
pub struct OpenCommit;

/// The widget: its button, and the pop-up while it is open.
pub struct BranchWidget {
    workspace: Entity<Workspace>,
    /// Each project's repositories, by its root.
    status: HashMap<String, Vec<Repo>>,
    /// The repository shown, by project and path: until one is chosen, the
    /// first with a branch.
    chosen: Option<(String, String)>,
    popup: Option<Entity<BranchPopup>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<OpenCommit> for BranchWidget {}

impl BranchWidget {
    pub fn new(workspace: Entity<Workspace>, cx: &mut Context<Self>) -> Self {
        let subs = vec![cx.subscribe(&workspace, |w, _, e: &Event, cx| match e {
            Event::Projects => w.reload_all(cx),
            Event::Heard { root, event, .. } if event == "git.changed" => {
                w.reload(root.clone(), cx)
            }
            _ => {}
        })];
        let mut w = BranchWidget {
            workspace,
            status: HashMap::new(),
            chosen: None,
            popup: None,
            _subs: subs,
        };
        w.reload_all(cx);
        w
    }

    fn reload_all(&mut self, cx: &mut Context<Self>) {
        let roots = self.workspace.read(cx).roots();
        self.status.retain(|r, _| roots.contains(r));
        for root in roots {
            self.reload(root, cx);
        }
    }

    fn reload(&mut self, root: String, cx: &mut Context<Self>) {
        let Some(code) = self.workspace.read(cx).code(&root) else {
            return;
        };
        let call = code.call("git.status", json!({}));
        cx.spawn(async move |this, cx| {
            let Ok(status) = call.await else { return };
            let _ = this.update(cx, |w, cx| {
                w.status.insert(root, repos(&status));
                cx.notify();
            });
        })
        .detach();
    }

    /// The projects' repositories that are on a branch, in the workspace's
    /// order: each project's root and the repository.
    pub fn repositories(&self, cx: &App) -> Vec<(String, Repo)> {
        self.workspace
            .read(cx)
            .projects()
            .iter()
            .filter_map(|f| Some((f.root.clone(), self.status.get(&f.root)?)))
            .flat_map(|(root, rs)| {
                rs.iter()
                    .filter(|r| r.branch.is_some())
                    .map(move |r| (root.clone(), r.clone()))
            })
            .collect()
    }

    /// The repository shown.
    pub fn shown(&self, cx: &App) -> Option<(String, Repo)> {
        let all = self.repositories(cx);
        self.chosen
            .as_ref()
            .and_then(|(root, path)| {
                all.iter()
                    .find(|(r, repo)| r == root && &repo.path == path)
                    .cloned()
            })
            .or_else(|| all.into_iter().next())
    }

    pub fn is_open(&self) -> bool {
        self.popup.is_some()
    }

    /// Opens the pop-up on the repository shown, or closes it.
    pub fn set_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !open {
            self.popup = None;
            cx.notify();
            return;
        }
        let Some((root, repo)) = self.shown(cx) else {
            return;
        };
        let others: Vec<(String, Repo)> = self.repositories(cx);
        let me = cx.entity().downgrade();
        let Some(code) = self.workspace.read(cx).code(&root) else {
            return;
        };
        let popup = cx.new(|cx| BranchPopup::new(me, code, root, repo, others, window, cx));
        // The search takes the keys: the pop-up is made after the popover
        // has opened, too late for it to move focus there itself.
        window.focus(&popup.focus_handle(cx), cx);
        self.popup = Some(popup);
        cx.notify();
    }

    /// Runs what a row does.
    fn run(
        &mut self,
        root: String,
        repo: String,
        does: Do,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_open(false, window, cx);
        let (method, params, doing) = match does {
            Do::Commit => {
                cx.emit(OpenCommit);
                return;
            }
            Do::Repository(root, path) => {
                self.chosen = Some((root, path));
                self.set_open(true, window, cx);
                return;
            }
            Do::NewBranch(None) => {
                let me = cx.entity().downgrade();
                crate::treeops::ask_name("New Branch", "", window, cx, move |name, window, cx| {
                    let (root, repo) = (root.clone(), repo.clone());
                    let _ = me.update(cx, |w, cx| {
                        w.run(root, repo, Do::NewBranch(Some(name)), window, cx)
                    });
                });
                return;
            }
            Do::NewBranch(Some(name)) => (
                "git.createBranch",
                json!({ "name": name }),
                format!("Could not create {name}"),
            ),
            Do::Checkout(name) => (
                "git.checkout",
                json!({ "name": name }),
                format!("Could not check out {name}"),
            ),
            Do::Update => ("git.pull", json!({}), "Could not update the project".into()),
            Do::Push => ("git.push", json!({}), "Could not push".into()),
        };
        let Some(code) = self.workspace.read(cx).code(&root) else {
            return;
        };
        let mut params = params;
        params["repo"] = json!(repo);
        let call = code.call(method, params);
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            let _ = this.update_in(cx, |w, window, cx| {
                if let Err(e) = result {
                    window.push_notification(Notification::error(format!("{doing}: {e}")), cx);
                }
                w.reload(root, cx);
            });
        })
        .detach();
    }
}

impl Render for BranchWidget {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let Some((_, repo)) = self.shown(cx) else {
            return div().into_any_element();
        };
        let branch = repo.branch.clone().unwrap_or_default();
        let branch = if branch == "(detached)" {
            "detached HEAD".to_string()
        } else {
            branch
        };
        let counts = match (repo.ahead.unwrap_or(0), repo.behind.unwrap_or(0)) {
            (0, 0) => String::new(),
            (a, 0) => format!(" ↑{a}"),
            (0, b) => format!(" ↓{b}"),
            (a, b) => format!(" ↑{a} ↓{b}"),
        };
        let me = cx.entity().downgrade();
        let popup = self.popup.clone();
        let focus = popup.as_ref().map(|p| p.focus_handle(cx));
        Popover::new("branch-widget")
            .anchor(gpui::Anchor::TopLeft)
            .open(popup.is_some())
            .on_open_change(move |open, window, cx| {
                let _ = me.update(cx, |w, cx| w.set_open(*open, window, cx));
            })
            .when_some(focus, |p, f| p.track_focus(&f))
            // A pill, as the toolbar's others are.
            .trigger(
                Button::new("branch")
                    .ghost()
                    .xsmall()
                    .debug_selector(|| "branch-widget".into())
                    .icon(gpui_component::Icon::empty().path(crate::assets::icon("branch")))
                    .label(format!("{branch}{counts} ▾"))
                    .rounded(px(10.))
                    .border_1()
                    .border_color(p.border)
                    .text_color(p.muted),
            )
            .content(move |_, _, _| match &popup {
                Some(p) => p.clone().into_any_element(),
                None => div().into_any_element(),
            })
            .into_any_element()
    }
}

/// The pop-up: a search box over the actions and the branches.
pub struct BranchPopup {
    widget: gpui::WeakEntity<BranchWidget>,
    root: String,
    repo: Repo,
    /// The projects' repositories, listed to switch to when there are
    /// several.
    others: Vec<(String, Repo)>,
    query: Entity<InputState>,
    branches: Option<Result<Branches, String>>,
    rows: Vec<Row>,
    selected: Option<usize>,
    _subs: Vec<Subscription>,
}

impl BranchPopup {
    fn new(
        widget: gpui::WeakEntity<BranchWidget>,
        code: crate::code::Code,
        root: String,
        repo: Repo,
        others: Vec<(String, Repo)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search branches and actions"));
        let subs =
            vec![
                cx.subscribe_in(&query, window, |v, _, e: &InputEvent, window, cx| match e {
                    InputEvent::Change => v.refresh(cx),
                    InputEvent::PressEnter { .. } => {
                        if let Some(i) = v.selected {
                            v.choose(i, window, cx)
                        }
                    }
                    _ => {}
                }),
            ];
        let mut v = BranchPopup {
            widget,
            root,
            repo,
            others,
            query,
            branches: None,
            rows: vec![],
            selected: None,
            _subs: subs,
        };
        v.load(code, cx);
        v.refresh(cx);
        v
    }

    fn load(&mut self, code: crate::code::Code, cx: &mut Context<Self>) {
        let call = code.call("git.branches", json!({ "repo": self.repo.path }));
        cx.spawn(async move |this, cx| {
            let result = call
                .await
                .and_then(|b| serde_json::from_value::<Branches>(b).map_err(|e| e.to_string()));
            let _ = this.update(cx, |v, cx| {
                v.branches = Some(result);
                v.refresh(cx);
            });
        })
        .detach();
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).value().to_string();
        let empty = Branches::default();
        let branches = match &self.branches {
            Some(Ok(b)) => b,
            _ => &empty,
        };
        let mut rows = vec![];
        // With several repositories, which one the list is about.
        if self.others.len() > 1 && query.trim().is_empty() {
            rows.push(Row::Heading("Repositories"));
            for (root, r) in &self.others {
                rows.push(Row::Item {
                    label: r.name.clone(),
                    hint: r.branch.clone(),
                    current: root == &self.root && r.path == self.repo.path,
                    does: Do::Repository(root.clone(), r.path.clone()),
                });
            }
        }
        rows.extend(self::rows(&query, branches));
        self.selected = first(&rows, &query);
        self.rows = rows;
        cx.notify();
    }

    fn choose(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(does) = self.rows.get(i).and_then(Row::does).cloned() else {
            return;
        };
        let (root, repo) = (self.root.clone(), self.repo.path.clone());
        let widget = self.widget.clone();
        // Run after this pop-up's own update, which the widget closes.
        window.defer(cx, move |window, cx| {
            let _ = widget.update(cx, |w, cx| w.run(root, repo, does, window, cx));
        });
    }

    fn key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let items: Vec<usize> = (0..self.rows.len())
            .filter(|&i| self.rows[i].does().is_some())
            .collect();
        let at = self
            .selected
            .and_then(|s| items.iter().position(|&i| i == s));
        let next = match e.keystroke.key.as_str() {
            "down" => at.map_or(0, |a| (a + 1) % items.len().max(1)),
            "up" => at.map_or(0, |a| (a + items.len().max(1) - 1) % items.len().max(1)),
            "escape" => {
                let widget = self.widget.clone();
                window.defer(cx, move |window, cx| {
                    let _ = widget.update(cx, |w, cx| w.set_open(false, window, cx));
                });
                cx.stop_propagation();
                return;
            }
            _ => return,
        };
        self.selected = items.get(next).copied();
        cx.stop_propagation();
        cx.notify();
    }
}

impl Focusable for BranchPopup {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.read(cx).focus_handle(cx)
    }
}

impl Render for BranchPopup {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let mut list = v_flex().py(px(4.));
        if let Some(Err(e)) = &self.branches {
            list = list.child(note(e.clone(), &p));
        }
        for (i, row) in self.rows.iter().enumerate() {
            list = list.child(self.row(i, row, &p, cx));
        }
        v_flex()
            .w(px(320.))
            .debug_selector(|| "branch-popup".into())
            .text_size(px(12.))
            .capture_key_down(cx.listener(|v, e: &KeyDownEvent, window, cx| v.key(e, window, cx)))
            .child(
                div()
                    .pb(px(4.))
                    .border_b_1()
                    .border_color(p.border)
                    .child(Input::new(&self.query).small().appearance(false)),
            )
            .child(list)
    }
}

impl BranchPopup {
    fn row(&self, i: usize, row: &Row, p: &Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        match row {
            Row::Heading(h) => div()
                .px(px(8.))
                .pt(px(6.))
                .pb(px(2.))
                .text_size(px(10.5))
                .text_color(p.muted)
                .child(*h)
                .into_any_element(),
            Row::More(n) => note(format!("{n} more, type to narrow"), p).into_any_element(),
            Row::Item {
                label,
                hint,
                current,
                ..
            } => {
                let on = self.selected == Some(i);
                let id = SharedString::from(format!("branch-row-{label}"));
                h_flex()
                    .id(("branch-row", i))
                    .debug_selector(move || id.to_string())
                    .gap(px(6.))
                    .px(px(8.))
                    .py(px(3.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .when(on, |d| d.bg(p.accent_soft))
                    .child(div().w(px(12.)).text_color(p.accent).child(if *current {
                        "✓"
                    } else {
                        ""
                    }))
                    .child(div().flex_1().min_w_0().truncate().child(label.clone()))
                    .when_some(hint.clone(), |d, h| {
                        d.child(div().text_color(p.muted).child(h))
                    })
                    .on_click(cx.listener(move |v, _, window, cx| v.choose(i, window, cx)))
                    .into_any_element()
            }
        }
    }
}

fn note(text: impl Into<SharedString>, p: &Palette) -> impl IntoElement {
    div()
        .px(px(8.))
        .py(px(3.))
        .text_color(p.muted)
        .child(text.into())
}
