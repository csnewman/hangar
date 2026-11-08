//! The Commit tool window: what has changed in each of the environment's
//! projects, repository by repository as the web UI's Changes view shows
//! it (staged, changed, unversioned), to stage, roll back and commit, and
//! push after. A click shows a file's diff; a double-click opens it. Its
//! Branch view (branch.rs) shows what each branch has changed since it
//! left its base.

use std::collections::HashMap;

use gpui::{
    Context, Entity, FontWeight, Hsla, IntoElement, ParentElement, Render, SharedString, Styled,
    Window, div, prelude::*, px, svg,
};
use gpui_component::input::{InputEvent, Textarea, TextareaState};
use gpui_component::{h_flex, v_flex};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::assets;
use crate::branch::BranchView;
use crate::code::Code;
use crate::diff::{ShowDiff, Spec};
use crate::palette::{self, Palette};
use crate::project::OpenFile;
use crate::ui;
use crate::workspace::{Event, State, Workspace};

/// A changed file, as git.status lists it: its path from the project's
/// folder and porcelain v2's letters for the index and the working tree.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Change {
    pub path: String,
    pub index: String,
    pub worktree: String,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub submodule: bool,
}

/// A repository in a project's folder, and its changes.
#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct Repo {
    /// From the folder: "" for the repository the folder is in.
    pub path: String,
    pub name: String,
    #[serde(default)]
    pub uninitialised: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub ahead: Option<i64>,
    #[serde(default)]
    pub behind: Option<i64>,
    #[serde(default)]
    pub changes: Vec<Change>,
}

/// The repositories git.status found.
pub fn repos(status: &Value) -> Vec<Repo> {
    serde_json::from_value(status["repos"].clone()).unwrap_or_default()
}

pub fn is_staged(c: &Change) -> bool {
    !matches!(c.index.as_str(), "." | "?" | "U")
}

/// A repository's changes: staged, changed in the working tree, and
/// untracked. A file staged and changed again is in the first two.
pub fn sections(r: &Repo) -> (Vec<&Change>, Vec<&Change>, Vec<&Change>) {
    let staged = r.changes.iter().filter(|c| is_staged(c)).collect();
    let changed = r
        .changes
        .iter()
        .filter(|c| c.worktree != "." && c.index != "?")
        .collect();
    let unversioned = r.changes.iter().filter(|c| c.index == "?").collect();
    (staged, changed, unversioned)
}

/// A change's path from its repository's folder, as a list grouped by
/// repository shows it.
pub fn within<'a>(r: &Repo, path: &'a str) -> &'a str {
    if r.path.is_empty() {
        return path;
    }
    path.strip_prefix(&format!("{}/", r.path)).unwrap_or(path)
}

fn usable(r: &Repo) -> bool {
    !r.uninitialised && r.error.is_none()
}

/// A repository to commit in: its project's root, its path there, and its
/// name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub root: String,
    pub repo: String,
    pub name: String,
}

fn target(root: &str, r: &Repo) -> Target {
    Target {
        root: root.into(),
        repo: r.path.clone(),
        name: r.name.clone(),
    }
}

/// Each repository once, with the root of the first project it is in: a
/// project can hold another's repository, a clone within its folder.
pub fn unique(projects: &[(String, Vec<Repo>)]) -> Vec<(&str, &Repo)> {
    let mut seen = std::collections::HashSet::new();
    projects
        .iter()
        .flat_map(|(root, rs)| rs.iter().map(move |r| (root.as_str(), r)))
        .filter(|(root, r)| {
            let abs = if r.path.is_empty() {
                root.trim_end_matches('/').to_string()
            } else {
                format!("{}/{}", root.trim_end_matches('/'), r.path)
            };
            seen.insert(abs)
        })
        .collect()
}

/// The repository Amend would amend: the one with anything staged, or,
/// with nothing staged anywhere, the only one there is.
pub fn amendable(projects: &[(String, Vec<Repo>)]) -> Option<Target> {
    let all = unique(projects);
    let staged: Vec<Target> = all
        .iter()
        .filter(|(_, r)| r.changes.iter().any(is_staged))
        .map(|(root, r)| target(root, r))
        .collect();
    if staged.len() == 1 {
        return staged.into_iter().next();
    }
    let ok: Vec<Target> = all
        .iter()
        .filter(|(_, r)| usable(r))
        .map(|(root, r)| target(root, r))
        .collect();
    (staged.is_empty() && ok.len() == 1)
        .then(|| ok.into_iter().next())
        .flatten()
}

/// The commits to make: one in each repository with something staged, a
/// submodule before the repository naming it so its parent sees its new
/// commit to record; amending, the one amendable repository.
pub fn commits(projects: &[(String, Vec<Repo>)], amend: bool) -> Result<Vec<Target>, String> {
    if amend {
        return amendable(projects)
            .map(|t| vec![t])
            .ok_or_else(|| "Amend one repository at a time.".into());
    }
    let mut out: Vec<Target> = unique(projects)
        .into_iter()
        .filter(|(_, r)| r.changes.iter().any(is_staged))
        .map(|(root, r)| target(root, r))
        .collect();
    let depth = |p: &str| {
        if p.is_empty() {
            0
        } else {
            p.split('/').count()
        }
    };
    out.sort_by_key(|t| std::cmp::Reverse(depth(&t.repo)));
    Ok(out)
}

/// A file's name and the folder it is in, from its repository's.
fn split(path: &str) -> (&str, &str) {
    match path.rsplit_once('/') {
        Some((dir, name)) => (name, dir),
        None => (path, ""),
    }
}

pub struct CommitView {
    workspace: Entity<Workspace>,
    /// Each project's repositories, by its root, or why they could not be
    /// read.
    status: HashMap<String, Result<Vec<Repo>, String>>,
    message: Entity<TextareaState>,
    amend: bool,
    /// What is being done, while it is.
    busy: Option<String>,
    error: Option<String>,
    /// The row whose rollback waits to be confirmed.
    confirm: Option<String>,
    selected: Option<String>,
    /// Whether the window shows what the branches have changed rather
    /// than what is uncommitted.
    branch_mode: bool,
    branch: Entity<BranchView>,
    _subs: Vec<gpui::Subscription>,
}

impl gpui::EventEmitter<OpenFile> for CommitView {}

impl gpui::EventEmitter<ShowDiff> for CommitView {}

impl CommitView {
    pub fn new(workspace: Entity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let message = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 10)
                .placeholder("Commit message")
        });
        let branch = cx.new(|_| BranchView::new(workspace.clone()));
        let subs = vec![
            cx.subscribe(&branch, |_, _, e: &ShowDiff, cx| cx.emit(e.clone())),
            cx.subscribe(&branch, |_, _, e: &OpenFile, cx| cx.emit(e.clone())),
            cx.subscribe(&workspace, |v, _, e: &Event, cx| match e {
                Event::Projects => v.reload_all(cx),
                Event::Heard { root, event, .. } if event == "git.changed" => {
                    v.reload(root.clone(), cx)
                }
                _ => {}
            }),
            cx.subscribe_in(
                &message,
                window,
                |v, _, e: &InputEvent, window, cx| match e {
                    InputEvent::PressEnter {
                        secondary: true, ..
                    } => v.commit(false, window, cx),
                    InputEvent::Change => cx.notify(),
                    _ => {}
                },
            ),
        ];
        let mut view = CommitView {
            workspace,
            status: HashMap::new(),
            message,
            amend: false,
            busy: None,
            error: None,
            confirm: None,
            selected: None,
            branch_mode: false,
            branch,
            _subs: subs,
        };
        view.reload_all(cx);
        view
    }

    /// The projects' repositories, in the workspace's order.
    pub fn projects(&self, cx: &gpui::App) -> Vec<(String, Vec<Repo>)> {
        self.workspace
            .read(cx)
            .projects()
            .iter()
            .filter_map(|f| match self.status.get(&f.root) {
                Some(Ok(rs)) => Some((f.root.clone(), rs.clone())),
                _ => None,
            })
            .collect()
    }

    pub fn message(&self) -> &Entity<TextareaState> {
        &self.message
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    fn reload_all(&mut self, cx: &mut Context<Self>) {
        let roots: Vec<String> = self
            .workspace
            .read(cx)
            .projects()
            .iter()
            .map(|f| f.root.clone())
            .collect();
        self.status.retain(|r, _| roots.contains(r));
        for root in roots {
            self.reload(root, cx);
        }
        cx.notify();
    }

    /// Has the Branch view read again what each repository's branch has
    /// changed, while it is shown.
    fn sync_branch(&mut self, cx: &mut Context<Self>) {
        if !self.branch_mode {
            return;
        }
        let projects = self.projects(cx);
        let repos: Vec<(String, Repo)> = unique(&projects)
            .into_iter()
            .filter(|(_, r)| usable(r))
            .map(|(root, r)| (root.to_string(), r.clone()))
            .collect();
        self.branch.update(cx, |b, cx| b.set_repos(repos, cx));
    }

    pub fn set_branch_mode(&mut self, on: bool, cx: &mut Context<Self>) {
        self.branch_mode = on;
        self.sync_branch(cx);
        cx.notify();
    }

    pub fn branch_view(&self) -> &Entity<BranchView> {
        &self.branch
    }

    fn reload(&mut self, root: String, cx: &mut Context<Self>) {
        let Some(code) = self.workspace.read(cx).code(&root) else {
            return;
        };
        let call = code.call("git.status", json!({}));
        cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |v, cx| {
                v.status.insert(root, result.map(|s| repos(&s)));
                v.sync_branch(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn code(&self, root: &str, cx: &gpui::App) -> Option<Code> {
        self.workspace.read(cx).code(root)
    }

    /// Runs git methods in a project one after another, showing doing
    /// meanwhile and the first error, then reads its status again.
    fn act(
        &mut self,
        root: &str,
        doing: &str,
        calls: Vec<(&'static str, Value)>,
        cx: &mut Context<Self>,
    ) {
        let Some(code) = self.code(root, cx) else {
            return;
        };
        self.busy = Some(doing.into());
        self.error = None;
        self.confirm = None;
        let root = root.to_string();
        cx.spawn(async move |this, cx| {
            let mut failed = None;
            for (method, params) in calls {
                if let Err(e) = code.call(method, params).await {
                    failed = Some(e);
                    break;
                }
            }
            let _ = this.update(cx, |v, cx| {
                v.busy = None;
                v.error = failed;
                v.reload(root, cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub fn stage(&mut self, root: &str, path: &str, cx: &mut Context<Self>) {
        self.act(
            root,
            "Staging…",
            vec![("git.stage", json!({ "path": path }))],
            cx,
        );
    }

    pub fn unstage(&mut self, root: &str, path: &str, cx: &mut Context<Self>) {
        self.act(
            root,
            "Unstaging…",
            vec![("git.unstage", json!({ "path": path }))],
            cx,
        );
    }

    fn rollback(&mut self, root: &str, path: &str, cx: &mut Context<Self>) {
        self.act(
            root,
            "Rolling back…",
            vec![("git.rollback", json!({ "path": path }))],
            cx,
        );
    }

    fn stage_all(&mut self, root: &str, paths: Vec<String>, cx: &mut Context<Self>) {
        let calls = paths
            .into_iter()
            .map(|p| ("git.stage", json!({ "path": p })))
            .collect();
        self.act(root, "Staging…", calls, cx);
    }

    fn unstage_all(&mut self, root: &str, paths: Vec<String>, cx: &mut Context<Self>) {
        let calls = paths
            .into_iter()
            .map(|p| ("git.unstage", json!({ "path": p })))
            .collect();
        self.act(root, "Unstaging…", calls, cx);
    }

    /// Whether Commit would do anything.
    fn can_commit(&self, cx: &gpui::App) -> bool {
        let projects = self.projects(cx);
        if self.amend {
            return amendable(&projects).is_some();
        }
        !self.message.read(cx).value().trim().is_empty()
            && unique(&projects)
                .iter()
                .any(|(_, r)| r.changes.iter().any(is_staged))
    }

    /// Commits what is staged with the message, then pushes each
    /// repository committed in when push is set.
    pub fn commit(&mut self, push: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() || !self.can_commit(cx) {
            return;
        }
        let targets = match commits(&self.projects(cx), self.amend) {
            Ok(t) => t,
            Err(e) => {
                self.error = Some(e);
                cx.notify();
                return;
            }
        };
        let calls: Vec<(Code, Target)> = targets
            .into_iter()
            .filter_map(|t| Some((self.code(&t.root, cx)?, t)))
            .collect();
        let message = self.message.read(cx).value().trim_end().to_string();
        let amend = self.amend;
        self.busy = Some("Committing…".into());
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let mut failed = None;
            for (code, t) in &calls {
                let params = json!({ "message": message, "amend": amend, "repo": t.repo });
                if let Err(e) = code.call("git.commit", params).await {
                    failed = Some(format!("{}: {e}", t.name));
                    break;
                }
            }
            let committed = failed.is_none();
            if committed && push {
                let _ = this.update(cx, |v, cx| {
                    v.busy = Some("Pushing…".into());
                    cx.notify();
                });
                for (code, t) in &calls {
                    if let Err(e) = code.call("git.push", json!({ "repo": t.repo })).await {
                        failed = Some(format!("{}: {e}", t.name));
                        break;
                    }
                }
            }
            let _ = this.update_in(cx, |v, window, cx| {
                v.busy = None;
                v.error = failed;
                if committed {
                    v.amend = false;
                    v.message.update(cx, |m, cx| m.set_value("", window, cx));
                }
                v.reload_all(cx);
            });
        })
        .detach();
    }

    /// Turns Amend on or off; on, an empty message starts as the last
    /// commit's.
    fn toggle_amend(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.amend = !self.amend;
        cx.notify();
        if !self.amend || !self.message.read(cx).value().trim().is_empty() {
            return;
        }
        let Some(t) = amendable(&self.projects(cx)) else {
            return;
        };
        let Some(code) = self.code(&t.root, cx) else {
            return;
        };
        let call = code.call("git.lastMessage", json!({ "repo": t.repo }));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Value::String(last)) = call.await else {
                return;
            };
            let _ = this.update_in(cx, |v, window, cx| {
                if v.amend && v.message.read(cx).value().trim().is_empty() {
                    v.message.update(cx, |m, cx| m.set_value(last, window, cx));
                }
            });
        })
        .detach();
    }
}

impl Render for CommitView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let mode = self.branch_mode;
        let seg = |id: &'static str, label: &'static str, on: bool, cx: &mut Context<Self>| {
            div()
                .id(id)
                .px(px(6.))
                .cursor_pointer()
                .when(on == mode, |d| d.bg(p.accent_soft).text_color(p.accent))
                .when(on != mode, |d| d.text_color(p.muted))
                .child(label)
                .on_click(cx.listener(move |v, _, _, cx| v.set_branch_mode(on, cx)))
        };
        let switch = h_flex()
            .mb(px(6.))
            .self_start()
            .flex_none()
            .rounded(px(5.))
            .border_1()
            .border_color(p.border)
            .overflow_hidden()
            .text_size(px(11.))
            .child(seg("mode-local", "Local", false, cx))
            .child(div().w(px(1.)).h_full().bg(p.border))
            .child(seg("mode-branch", "Branch", true, cx));
        if mode {
            return v_flex()
                .size_full()
                .min_h_0()
                .text_size(px(12.))
                .child(switch)
                .child(self.branch.clone())
                .into_any_element();
        }
        let ws = self.workspace.read(cx);
        let state = ws.state().clone();
        let folders: Vec<(String, String)> = ws
            .projects()
            .iter()
            .map(|f| (f.root.clone(), f.name.clone()))
            .collect();
        let several = folders.len() > 1;
        let projects = self.projects(cx);
        let first: std::collections::HashSet<(String, String)> = unique(&projects)
            .into_iter()
            .map(|(root, r)| (root.to_string(), r.path.clone()))
            .collect();

        let mut list = v_flex()
            .id("commit-changes")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll();
        let mut any = false;
        match &state {
            State::Connecting => list = list.child(muted("Connecting…", &p)),
            State::Failed(e) => {
                list = list.child(muted(format!("Could not reach the Code service: {e}"), &p))
            }
            State::Ready => {
                for (root, name) in &folders {
                    if several {
                        list = list.child(
                            div()
                                .px(px(4.))
                                .pt(px(4.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(name.clone()),
                        );
                    }
                    match self.status.get(root) {
                        None => list = list.child(muted("Reading…", &p)),
                        Some(Err(e)) => list = list.child(muted(e.clone(), &p)),
                        Some(Ok(repos)) if repos.is_empty() => {
                            list = list.child(muted("Not a git repository.", &p))
                        }
                        Some(Ok(repos)) => {
                            // A repository an earlier project holds is
                            // listed there.
                            let own: Vec<&Repo> = repos
                                .iter()
                                .filter(|r| first.contains(&(root.clone(), r.path.clone())))
                                .collect();
                            if own.is_empty() {
                                let names: Vec<&str> =
                                    repos.iter().map(|r| r.name.as_str()).collect();
                                list = list.child(muted(
                                    format!(
                                        "Holds {}, which {} listed above.",
                                        names.join(", "),
                                        if names.len() == 1 { "is" } else { "are" }
                                    ),
                                    &p,
                                ));
                            }
                            let one = own.len() == 1;
                            for r in own {
                                any |= !r.changes.is_empty();
                                list = list.child(self.repo(root, r, one, &p, cx));
                            }
                        }
                    }
                }
                if folders.is_empty() {
                    list = list.child(muted("The environment has no projects.", &p));
                }
            }
        }
        // Without a repository there is nothing to change or commit.
        let has_repo = !first.is_empty();
        if state == State::Ready && !any && has_repo {
            list = list.child(muted("Nothing has changed.", &p));
        }

        let staged_files: usize = unique(&projects)
            .iter()
            .map(|(_, r)| r.changes.iter().filter(|c| is_staged(c)).count())
            .sum();
        let can = self.can_commit(cx);
        let label = if self.amend {
            "Amend".to_string()
        } else {
            match staged_files {
                0 => "Commit".to_string(),
                1 => "Commit 1 file".to_string(),
                n => format!("Commit {n} files"),
            }
        };
        let amend = self.amend;
        let check = h_flex()
            .id("commit-amend")
            .gap(px(5.))
            .text_color(p.muted)
            .when(!has_repo, |d| d.opacity(0.5))
            .child(
                div()
                    .size(px(12.))
                    .rounded(px(3.))
                    .border_1()
                    .border_color(if amend { p.accent } else { p.border_strong })
                    .when(amend, |d| d.bg(p.accent))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(9.))
                    .text_color(p.on_accent)
                    .when(amend, |d| d.child("✓")),
            )
            .child("Amend")
            .when(has_repo, |d| {
                d.cursor_pointer()
                    .on_click(cx.listener(|v, _, window, cx| v.toggle_amend(window, cx)))
            });
        let (commit, push) = if can && self.busy.is_none() {
            (
                ui::primary("commit-go", label, &p)
                    .on_click(cx.listener(|v, _, window, cx| v.commit(false, window, cx))),
                ui::button("commit-push", "Commit and Push", &p)
                    .on_click(cx.listener(|v, _, window, cx| v.commit(true, window, cx))),
            )
        } else {
            (
                ui::disabled("commit-go", label, &p),
                ui::disabled("commit-push", "Commit and Push", &p),
            )
        };

        let foot = v_flex()
            .gap(px(6.))
            .pt(px(6.))
            .border_t_1()
            .border_color(p.border)
            .child(h_flex().child(check))
            .child(Textarea::new(&self.message).disabled(!has_repo))
            .child(h_flex().flex_wrap().gap(px(8.)).child(commit).child(push))
            .when_some(self.busy.clone(), |d, b| d.child(muted(b, &p)))
            .when_some(self.error.clone(), |d, e| {
                d.child(
                    div()
                        .px(px(4.))
                        .text_size(px(11.5))
                        .text_color(p.bad)
                        .child(e),
                )
            });

        v_flex()
            .size_full()
            .min_h_0()
            .text_size(px(12.))
            .child(switch)
            .child(list)
            .child(foot)
            .into_any_element()
    }
}

impl CommitView {
    fn repo(
        &self,
        root: &str,
        r: &Repo,
        alone: bool,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut out = v_flex().pb(px(4.));
        let mut head = h_flex()
            .gap(px(6.))
            .px(px(4.))
            .h(px(22.))
            .when(!alone, |d| {
                d.child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(r.name.clone()),
                )
            });
        if let Some(b) = &r.branch {
            head = head.child(div().text_color(p.muted).child(format!("⑂ {b}")));
        }
        let ahead_behind = match (r.ahead.unwrap_or(0), r.behind.unwrap_or(0)) {
            (0, 0) => None,
            (a, 0) => Some(format!("↑{a}")),
            (0, b) => Some(format!("↓{b}")),
            (a, b) => Some(format!("↑{a} ↓{b}")),
        };
        if let Some(ab) = ahead_behind {
            head = head.child(div().text_color(p.muted).child(ab));
        }
        out = out.child(head);
        if r.uninitialised {
            return out.child(muted("Not checked out.", p));
        }
        if let Some(e) = &r.error {
            return out.child(muted(e.clone(), p));
        }
        let (staged, changed, unversioned) = sections(r);
        let all = |cs: &[&Change]| cs.iter().map(|c| c.path.clone()).collect::<Vec<_>>();
        if !staged.is_empty() {
            let paths = all(&staged);
            let rt = root.to_string();
            out = out.child(self.section(
                root,
                r,
                "Staged",
                &staged,
                true,
                Some((
                    "Unstage all",
                    Box::new(move |v: &mut Self, cx: &mut Context<Self>| {
                        v.unstage_all(&rt, paths.clone(), cx)
                    }),
                )),
                p,
                cx,
            ));
        }
        if !changed.is_empty() {
            let mut paths = all(&changed);
            paths.extend(all(&unversioned));
            let rt = root.to_string();
            out = out.child(self.section(
                root,
                r,
                "Changes",
                &changed,
                false,
                Some((
                    "Stage all",
                    Box::new(move |v: &mut Self, cx: &mut Context<Self>| {
                        v.stage_all(&rt, paths.clone(), cx)
                    }),
                )),
                p,
                cx,
            ));
        }
        if !unversioned.is_empty() {
            out = out.child(self.section(
                root,
                r,
                "Unversioned files",
                &unversioned,
                false,
                None,
                p,
                cx,
            ));
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn section(
        &self,
        root: &str,
        r: &Repo,
        title: &str,
        list: &[&Change],
        staged: bool,
        bulk: Option<(&'static str, BulkAction)>,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = format!("{root}:{}:{title}", r.path);
        let mut head = h_flex()
            .gap(px(6.))
            .px(px(4.))
            .h(px(20.))
            .text_size(px(11.))
            .text_color(p.muted)
            .child(title.to_string())
            .child(list.len().to_string())
            .child(div().flex_1());
        if let Some((label, run)) = bulk {
            head = head.child(
                div()
                    .id(SharedString::from(format!("bulk-{id}")))
                    .cursor_pointer()
                    .text_color(p.accent)
                    .hover(|s| s.underline())
                    .child(label)
                    .on_click(cx.listener(move |v, _, _, cx| run(v, cx))),
            );
        }
        let mut out = v_flex().child(head);
        for c in list {
            out = out.child(self.row(root, r, c, staged, p, cx));
        }
        out
    }

    fn row(
        &self,
        root: &str,
        r: &Repo,
        c: &Change,
        staged: bool,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let letter = if staged { &c.index } else { &c.worktree };
        let shown = if letter == "?" { "U" } else { letter.as_str() };
        let colour = letter_colour(letter, p);
        let (name, dir) = split(within(r, &c.path));
        let key = format!("{}:{root}:{}", if staged { 's' } else { 'w' }, c.path);
        let selected = self.selected.as_ref() == Some(&key);
        let confirming = self.confirm.as_ref() == Some(&key);
        let deleted = letter == "D";
        let from = c.from.clone();
        let tip = match &c.from {
            Some(from) => format!("{from} → {}", c.path),
            None => c.path.clone(),
        };

        let (rt, path) = (root.to_string(), c.path.clone());
        let stage_button = icon_button(
            format!("stage-{key}"),
            if staged { "minus" } else { "plus" },
            p,
        )
        .on_click(cx.listener({
            let (rt, path) = (rt.clone(), path.clone());
            move |v, _, _, cx| {
                cx.stop_propagation();
                if staged {
                    v.unstage(&rt, &path, cx)
                } else {
                    v.stage(&rt, &path, cx)
                }
            }
        }));
        let rollback = if c.submodule {
            None
        } else if confirming {
            Some(
                div()
                    .id(SharedString::from(format!("discard-{key}")))
                    .px(px(4.))
                    .rounded(px(3.))
                    .cursor_pointer()
                    .text_size(px(11.))
                    .text_color(p.bad)
                    .hover(|s| s.bg(p.surface_2))
                    .child(if c.index == "?" {
                        "Delete?"
                    } else {
                        "Discard?"
                    })
                    .on_click(cx.listener({
                        let (rt, path) = (rt.clone(), path.clone());
                        move |v, _, _, cx| {
                            cx.stop_propagation();
                            v.rollback(&rt, &path, cx)
                        }
                    }))
                    .into_any_element(),
            )
        } else {
            Some(
                icon_button(format!("rollback-{key}"), "undo", p)
                    .on_click(cx.listener({
                        let key = key.clone();
                        move |v, _, _, cx| {
                            cx.stop_propagation();
                            v.confirm = Some(key.clone());
                            cx.notify();
                        }
                    }))
                    .into_any_element(),
            )
        };

        h_flex()
            .id(SharedString::from(format!("change-{key}")))
            .group("change")
            .h(px(20.))
            .gap(px(6.))
            .pl(px(10.))
            .pr(px(4.))
            .rounded(px(4.))
            .cursor_pointer()
            .when(selected, |d| d.bg(p.sidebar_active))
            .when(!selected, |d| d.hover(|s| s.bg(p.surface_2)))
            .child(
                div()
                    .w(px(10.))
                    .flex_none()
                    .text_size(px(10.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(colour)
                    .child(shown.to_string()),
            )
            .child(
                div()
                    .flex_none()
                    .max_w(px(220.))
                    .truncate()
                    .text_color(colour)
                    .child(name.to_string()),
            )
            .when(c.submodule, |d| {
                d.child(div().text_color(p.muted).child("new commits"))
            })
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_size(px(11.))
                    .text_color(p.muted)
                    .child(dir.to_string()),
            )
            .child(
                h_flex()
                    .gap(px(2.))
                    .flex_none()
                    .when(!confirming, |d| {
                        d.invisible().group_hover("change", |s| s.visible())
                    })
                    .child(stage_button)
                    .children(rollback),
            )
            .on_click(cx.listener(move |v, e: &gpui::ClickEvent, _, cx| {
                v.selected = Some(key.clone());
                if e.click_count() >= 2 && !deleted {
                    cx.emit(OpenFile {
                        root: rt.clone(),
                        path: path.clone(),
                    });
                } else {
                    let mut spec = Spec::working(&rt, &path);
                    spec.from = from.clone();
                    cx.emit(ShowDiff(spec));
                }
                cx.notify();
            }))
            .tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
            })
    }
}

type BulkAction = Box<dyn Fn(&mut CommitView, &mut Context<CommitView>)>;

fn letter_colour(letter: &str, p: &Palette) -> Hsla {
    match letter {
        "M" | "R" | "C" | "T" => p.busy,
        "A" | "?" => p.good,
        "D" | "U" => p.bad,
        _ => p.muted,
    }
}

fn icon_button(id: String, icon: &str, p: &Palette) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(id))
        .size(px(18.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.))
        .cursor_pointer()
        .hover(|s| s.bg(p.border))
        .child(
            svg()
                .path(assets::icon(icon))
                .size(px(12.))
                .text_color(p.muted),
        )
}

fn muted(text: impl Into<SharedString>, p: &Palette) -> impl IntoElement {
    div()
        .px(px(4.))
        .py(px(2.))
        .text_size(px(11.5))
        .text_color(p.muted)
        .child(text.into())
}
