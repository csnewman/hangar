//! The Commit window's Branch view, as the web UI's Changes window has it:
//! everything each repository's branch has changed since it left its base
//! (the main branch, or one chosen), committed or not, and the commits
//! since. A file shows its diff against the base where the branch left it.

use std::collections::HashMap;

use chrono::{TimeZone, Utc};
use gpui::{
    Context, Entity, FontWeight, IntoElement, ParentElement, Render, SharedString, Styled, Window,
    div, prelude::*, px, svg,
};
use gpui_component::{h_flex, v_flex};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::assets;
use crate::commit::Repo;
use crate::diff::{ShowDiff, Spec};
use crate::gitlog::{Named, status_colour};
use crate::model;
use crate::palette::{self, Palette};
use crate::project::OpenFile;
use crate::workspace::Workspace;

/// A commit on the branch since its base.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct BranchCommit {
    pub hash: String,
    pub subject: String,
    pub author: String,
    pub time: f64,
}

/// What git.branchChanges gives: the base compared with, the commit the
/// branch left it at, the commits since and the files differing.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Changes {
    pub base: String,
    #[serde(rename = "mergeBase")]
    pub merge_base: String,
    pub commits: Vec<BranchCommit>,
    pub changes: Vec<Named>,
}

/// A repository, by its project's root and its path there.
type Key = (String, String);

#[derive(Default)]
struct Shown {
    name: String,
    branch: Option<String>,
    /// The base chosen: None is the main branch.
    base: Option<String>,
    changes: Option<Result<Changes, String>>,
    commits_open: bool,
    /// The branches to compare with, while the list is open.
    picking: Option<Vec<String>>,
}

pub struct BranchView {
    workspace: Entity<Workspace>,
    order: Vec<Key>,
    repos: HashMap<Key, Shown>,
}

impl gpui::EventEmitter<ShowDiff> for BranchView {}
impl gpui::EventEmitter<OpenFile> for BranchView {}

impl BranchView {
    pub fn new(workspace: Entity<Workspace>) -> Self {
        BranchView {
            workspace,
            order: vec![],
            repos: HashMap::new(),
        }
    }

    /// Shows these repositories, each once, and reads again what each
    /// branch has changed.
    pub fn set_repos(&mut self, repos: Vec<(String, Repo)>, cx: &mut Context<Self>) {
        self.order = repos
            .iter()
            .map(|(root, r)| (root.clone(), r.path.clone()))
            .collect();
        self.repos.retain(|k, _| self.order.contains(k));
        for (root, r) in repos {
            let key = (root, r.path.clone());
            let shown = self.repos.entry(key.clone()).or_insert_with(|| Shown {
                commits_open: true,
                ..Default::default()
            });
            shown.name = r.name;
            shown.branch = r.branch;
            self.load(key, cx);
        }
        cx.notify();
    }

    pub fn changes(&self, root: &str, repo: &str) -> Option<&Changes> {
        self.repos
            .get(&(root.to_string(), repo.to_string()))?
            .changes
            .as_ref()?
            .as_ref()
            .ok()
    }

    fn load(&mut self, key: Key, cx: &mut Context<Self>) {
        let Some(code) = self.workspace.read(cx).code(&key.0) else {
            return;
        };
        let Some(shown) = self.repos.get(&key) else {
            return;
        };
        let mut params = json!({ "repo": key.1 });
        if let Some(b) = &shown.base {
            params["base"] = Value::String(b.clone());
        }
        let base = shown.base.clone();
        let call = code.call("git.branchChanges", params);
        cx.spawn(async move |this, cx| {
            let result = call
                .await
                .and_then(|v| serde_json::from_value::<Changes>(v).map_err(|e| e.to_string()));
            let _ = this.update(cx, |v, cx| {
                if let Some(s) = v.repos.get_mut(&key)
                    && s.base == base
                {
                    s.changes = Some(result);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Opens the list of branches to compare with, or closes it.
    fn pick(&mut self, key: Key, cx: &mut Context<Self>) {
        let Some(s) = self.repos.get_mut(&key) else {
            return;
        };
        if s.picking.take().is_some() {
            cx.notify();
            return;
        }
        let Some(code) = self.workspace.read(cx).code(&key.0) else {
            return;
        };
        let call = code.call("git.branches", json!({ "repo": key.1 }));
        cx.spawn(async move |this, cx| {
            let Ok(b) = call.await else { return };
            let current = b["current"].as_str().unwrap_or_default().to_string();
            let mut names: Vec<String> = b["local"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|l| l["name"].as_str().map(str::to_string))
                .filter(|n| n != &current)
                .collect();
            names.extend(
                b["remote"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|r| r.as_str().map(str::to_string)),
            );
            let _ = this.update(cx, |v, cx| {
                if let Some(s) = v.repos.get_mut(&key) {
                    s.picking = Some(names);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn set_base(&mut self, key: Key, base: Option<String>, cx: &mut Context<Self>) {
        if let Some(s) = self.repos.get_mut(&key) {
            s.picking = None;
            if s.base != base {
                s.base = base;
                s.changes = None;
                self.load(key, cx);
            }
            cx.notify();
        }
    }

    fn repo(
        &self,
        key: &Key,
        alone: bool,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let s = &self.repos[key];
        let id = format!("{}:{}", key.0, key.1);
        let mut out = v_flex().pb(px(6.));
        if !alone {
            out = out.child(
                div()
                    .px(px(4.))
                    .pt(px(2.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(s.name.clone()),
            );
        }
        let base_label = match (&s.changes, &s.base) {
            (Some(Ok(c)), _) => c.base.clone(),
            (_, Some(b)) => b.clone(),
            _ => "the main branch".into(),
        };
        let k = key.clone();
        let compared = h_flex()
            .flex_wrap()
            .gap(px(5.))
            .px(px(4.))
            .min_h(px(22.))
            .text_size(px(11.))
            .text_color(p.muted)
            .when_some(s.branch.clone(), |d, b| {
                d.child(div().text_color(p.text).child(format!("⑂ {b}")))
            })
            .child("compared with")
            .child(
                h_flex()
                    .id(SharedString::from(format!("base-{id}")))
                    .gap(px(2.))
                    .px(px(5.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(p.border)
                    .cursor_pointer()
                    .text_color(p.text)
                    .hover(|st| st.bg(p.surface_2))
                    .child(base_label)
                    .child(
                        svg()
                            .path(assets::icon("chevron-down"))
                            .size(px(9.))
                            .text_color(p.muted),
                    )
                    .on_click(cx.listener(move |v, _, _, cx| v.pick(k.clone(), cx))),
            );
        out = out.child(compared);
        if let Some(Ok(c)) = &s.changes {
            // The changes are everything since that commit, so later
            // commits on the base are not among them.
            out = out.child(muted(
                format!(
                    "Branched from {} at {}",
                    c.base,
                    &c.merge_base[..c.merge_base.len().min(7)]
                ),
                p,
            ));
        }
        if let Some(names) = &s.picking {
            let mut list = v_flex()
                .mx(px(4.))
                .mb(px(4.))
                .py(px(2.))
                .rounded(px(5.))
                .border_1()
                .border_color(p.border)
                .bg(p.surface);
            let k = key.clone();
            list = list.child(
                menu_row(
                    format!("pick-main-{id}"),
                    "The main branch",
                    s.base.is_none(),
                    p,
                )
                .on_click(cx.listener(move |v, _, _, cx| v.set_base(k.clone(), None, cx))),
            );
            for (i, n) in names.iter().enumerate() {
                let (k, name) = (key.clone(), n.clone());
                list =
                    list.child(
                        menu_row(
                            format!("pick-{id}-{i}"),
                            n,
                            s.base.as_deref() == Some(n.as_str()),
                            p,
                        )
                        .on_click(cx.listener(move |v, _, _, cx| {
                            v.set_base(k.clone(), Some(name.clone()), cx)
                        })),
                    );
            }
            out = out.child(list);
        }
        let c = match &s.changes {
            None => return out.child(muted("Reading…", p)),
            Some(Err(e)) => return out.child(muted(e.clone(), p)),
            Some(Ok(c)) => c,
        };

        let k = key.clone();
        let open = s.commits_open;
        out = out.child(
            h_flex()
                .id(SharedString::from(format!("commits-{id}")))
                .gap(px(5.))
                .px(px(4.))
                .h(px(20.))
                .text_size(px(11.))
                .text_color(p.muted)
                .cursor_pointer()
                .child(
                    svg()
                        .path(assets::icon(if open {
                            "chevron-down"
                        } else {
                            "chevron-right"
                        }))
                        .size(px(9.))
                        .text_color(p.muted),
                )
                .child("Commits")
                .child(c.commits.len().to_string())
                .on_click(cx.listener(move |v, _, _, cx| {
                    if let Some(s) = v.repos.get_mut(&k) {
                        s.commits_open = !s.commits_open;
                        cx.notify();
                    }
                })),
        );
        if open {
            if c.commits.is_empty() {
                out = out.child(muted("No commits yet.", p));
            }
            let now = Utc::now();
            for bc in &c.commits {
                let when = Utc
                    .timestamp_opt(bc.time as i64, 0)
                    .single()
                    .map(|t| model::ago(t, now))
                    .unwrap_or_default();
                out = out.child(
                    h_flex()
                        .h(px(20.))
                        .gap(px(6.))
                        .pl(px(10.))
                        .pr(px(4.))
                        .child(
                            div()
                                .flex_none()
                                .size(px(7.))
                                .rounded(px(4.))
                                .border_1()
                                .border_color(p.accent),
                        )
                        .child(div().min_w_0().truncate().child(bc.subject.clone()))
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(11.))
                                .text_color(p.muted)
                                .child(format!("{}, {when}", bc.author)),
                        ),
                );
            }
        }

        out = out.child(
            h_flex()
                .gap(px(5.))
                .px(px(4.))
                .h(px(20.))
                .text_size(px(11.))
                .text_color(p.muted)
                .child("Files changed")
                .child(c.changes.len().to_string()),
        );
        if c.changes.is_empty() {
            out = out.child(muted(format!("Nothing differs from {}.", c.base), p));
        }
        for (i, f) in c.changes.iter().enumerate() {
            out = out.child(self.file(key, c, i, f, p, cx));
        }
        out
    }

    fn file(
        &self,
        key: &Key,
        c: &Changes,
        i: usize,
        f: &Named,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let colour = status_colour(&f.status, p);
        let shown = if key.1.is_empty() {
            f.path.as_str()
        } else {
            f.path
                .strip_prefix(&format!("{}/", key.1))
                .unwrap_or(&f.path)
        };
        let (name, dir) = match shown.rsplit_once('/') {
            Some((d, n)) => (n.to_string(), d.to_string()),
            None => (shown.to_string(), String::new()),
        };
        let where_ = match &f.from {
            Some(from) => format!("from {from}"),
            None => dir,
        };
        let spec = Spec {
            root: key.0.clone(),
            path: f.path.clone(),
            against: c.merge_base.clone(),
            from: f.from.clone(),
            to: None,
            other: None,
        };
        let deleted = f.status == "D";
        let open = OpenFile {
            root: key.0.clone(),
            path: f.path.clone(),
        };
        h_flex()
            .id(SharedString::from(format!(
                "branch-file-{}:{}:{i}",
                key.0, key.1
            )))
            .h(px(20.))
            .gap(px(6.))
            .pl(px(10.))
            .pr(px(4.))
            .rounded(px(4.))
            .cursor_pointer()
            .hover(|s| s.bg(p.surface_2))
            .child(
                div()
                    .w(px(10.))
                    .flex_none()
                    .text_size(px(10.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(colour)
                    .child(f.status.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .max_w(px(220.))
                    .truncate()
                    .text_color(colour)
                    .child(name),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_size(px(11.))
                    .text_color(p.muted)
                    .child(where_),
            )
            .on_click(cx.listener(move |_, e: &gpui::ClickEvent, _, cx| {
                if e.click_count() >= 2 && !deleted {
                    cx.emit(open.clone());
                } else {
                    cx.emit(ShowDiff(spec.clone()));
                }
            }))
    }
}

fn menu_row(id: String, label: &str, on: bool, p: &Palette) -> gpui::Stateful<gpui::Div> {
    h_flex()
        .id(SharedString::from(id))
        .h(px(20.))
        .gap(px(6.))
        .px(px(8.))
        .cursor_pointer()
        .hover(|s| s.bg(p.surface_2))
        .child(
            div()
                .w(px(10.))
                .text_color(p.accent)
                .child(if on { "✓" } else { "" }),
        )
        .child(label.to_string())
}

fn muted(text: impl Into<SharedString>, p: &Palette) -> impl IntoElement {
    div()
        .px(px(10.))
        .py(px(2.))
        .text_size(px(11.5))
        .text_color(p.muted)
        .child(text.into())
}

impl Render for BranchView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let mut list = v_flex();
        if self.order.is_empty() {
            list = list.child(muted(
                "No git repository in the environment's projects.",
                &p,
            ));
        }
        let alone = self.order.len() == 1;
        for key in self.order.clone() {
            list = list.child(self.repo(&key, alone, &p, cx));
        }
        crate::project::scroll("branch-changes", list)
            .flex_1()
            .min_h_0()
    }
}
