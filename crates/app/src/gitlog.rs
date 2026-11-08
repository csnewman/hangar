//! The Git tool window: a repository's history, as IntelliJ's Log tab and
//! the web UI's Git Log show it, with its commit graph, and the commit
//! chosen beside it: its files, each opening its diff, and its message.

use std::collections::HashMap;
use std::ops::Range;

use chrono::{DateTime, Datelike, Local, TimeZone};
use gpui::{
    Bounds, Context, Entity, FontWeight, Hsla, IntoElement, ParentElement, PathBuilder, Pixels,
    Render, SharedString, Styled, Window, canvas, div, hsla, point, prelude::*, px, uniform_list,
};
use gpui_component::{h_flex, v_flex};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::commit::{self, Repo};
use crate::diff::{ShowDiff, Spec};
use crate::palette::{self, Palette};
use crate::workspace::{Event, State, Workspace};

/// A branch, tag or HEAD naming a commit: kind is head (the branch
/// checked out, or a detached HEAD), branch, remote or tag.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Ref {
    pub name: String,
    pub kind: String,
}

/// A commit as git.log lists it.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Commit {
    pub hash: String,
    pub parents: Vec<String>,
    pub author: String,
    #[serde(default)]
    pub email: String,
    pub time: f64,
    pub subject: String,
    #[serde(default)]
    pub refs: Vec<Ref>,
}

#[derive(Debug, Deserialize)]
struct Log {
    commits: Vec<Commit>,
    more: bool,
}

/// A file a commit changed, and how: A, M, D, R or C.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Named {
    pub path: String,
    pub status: String,
    #[serde(default)]
    pub from: Option<String>,
}

/// A commit in full, as git.commitInfo gives it.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Info {
    pub hash: String,
    pub parents: Vec<String>,
    pub author: String,
    pub email: String,
    pub time: f64,
    pub committer: String,
    #[serde(rename = "committerTime")]
    pub committer_time: f64,
    pub message: String,
    pub files: Vec<Named>,
}

/// A line within a graph row: In runs from a lane at the row's top to the
/// commit, Through passes the row in its lane, and Out runs from the commit
/// to a lane at the row's bottom, towards a parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    In,
    Through,
    Out,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge {
    pub kind: EdgeKind,
    pub from: usize,
    pub to: usize,
    pub color: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphRow {
    pub col: usize,
    pub color: usize,
    pub edges: Vec<Edge>,
}

/// The commit graph's layout, as git log --graph and IntelliJ draw it (the
/// web UI's graph.ts): each commit on its own row, in a lane, with lines
/// down to its parents. Lanes are the commits still to come that rows above
/// point at; a commit takes the lane that points at it (the leftmost,
/// others joining it), its first parent carries the lane on, and each
/// other parent opens a lane of its own unless one already leads there.
/// Freed lanes are reused, so a lane keeps its column, and its colour, all
/// the way down. Returns the rows and how many lanes the widest needs.
pub fn layout(commits: &[Commit]) -> (Vec<GraphRow>, usize) {
    struct Lane {
        hash: String,
        color: usize,
    }
    let mut lanes: Vec<Option<Lane>> = Vec::new();
    let mut colors = 0;
    let mut widest = 0;
    let free = |lanes: &Vec<Option<Lane>>, not: Option<usize>| {
        lanes
            .iter()
            .enumerate()
            .position(|(j, l)| l.is_none() && Some(j) != not)
            .unwrap_or_else(|| lanes.len().max(not.map_or(0, |n| n + 1)))
    };
    let put = |lanes: &mut Vec<Option<Lane>>, j: usize, lane: Lane| {
        if lanes.len() <= j {
            lanes.resize_with(j + 1, || None);
        }
        lanes[j] = Some(lane);
    };
    let rows = commits
        .iter()
        .map(|c| {
            let mut edges = Vec::new();
            let found = lanes
                .iter()
                .position(|l| l.as_ref().is_some_and(|l| l.hash == c.hash));
            // A commit no row above points at begins a lane: a branch's tip.
            let (col, color) = match found {
                Some(i) => (i, lanes[i].as_ref().unwrap().color),
                None => {
                    colors += 1;
                    (free(&lanes, None), colors - 1)
                }
            };
            for (i, l) in lanes.iter_mut().enumerate() {
                let Some(lane) = l else { continue };
                if lane.hash == c.hash {
                    edges.push(Edge {
                        kind: EdgeKind::In,
                        from: i,
                        to: col,
                        color: lane.color,
                    });
                    *l = None;
                } else {
                    edges.push(Edge {
                        kind: EdgeKind::Through,
                        from: i,
                        to: i,
                        color: lane.color,
                    });
                }
            }
            for (k, p) in c.parents.iter().enumerate() {
                // The first parent always carries the commit's lane on,
                // even when another lane leads to it too: the lanes meet at
                // the parent, where the branches forked. A merged parent
                // joins a lane that leads there.
                let leads = lanes
                    .iter()
                    .position(|l| l.as_ref().is_some_and(|l| &l.hash == p));
                let j = match (k, leads) {
                    (0, _) => {
                        put(
                            &mut lanes,
                            col,
                            Lane {
                                hash: p.clone(),
                                color,
                            },
                        );
                        col
                    }
                    (_, Some(j)) => j,
                    (_, None) => {
                        let j = free(&lanes, Some(col));
                        colors += 1;
                        put(
                            &mut lanes,
                            j,
                            Lane {
                                hash: p.clone(),
                                color: colors - 1,
                            },
                        );
                        j
                    }
                };
                edges.push(Edge {
                    kind: EdgeKind::Out,
                    from: col,
                    to: j,
                    color: lanes[j].as_ref().unwrap().color,
                });
            }
            while matches!(lanes.last(), Some(None)) {
                lanes.pop();
            }
            widest = edges
                .iter()
                .map(|e| e.from.max(e.to) + 1)
                .chain([col + 1, lanes.len(), widest])
                .max()
                .unwrap_or(widest);
            GraphRow { col, color, edges }
        })
        .collect();
    (rows, widest)
}

/// How a time is shown in the list: today's as the time, this year's as
/// the day, older ones with the year.
pub fn when(secs: f64, now: DateTime<Local>) -> String {
    let Some(t) = Local.timestamp_opt(secs as i64, 0).single() else {
        return String::new();
    };
    if t.date_naive() == now.date_naive() {
        t.format("%H:%M").to_string()
    } else if t.year() == now.year() {
        t.format("%-d %b").to_string()
    } else {
        t.format("%-d %b %Y").to_string()
    }
}

/// The commits git.log is asked for at a time.
const PAGE: usize = 300;
const ROW: f32 = 22.;
const LANE: f32 = 12.;

/// The repository shown: its project's root and its path there.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Chosen {
    root: String,
    repo: String,
}

pub struct GitView {
    workspace: Entity<Workspace>,
    status: HashMap<String, Vec<Repo>>,
    chosen: Option<Chosen>,
    /// The commits of every branch, or of HEAD's alone.
    current_only: bool,
    commits: Vec<Commit>,
    graph: Vec<GraphRow>,
    lanes: usize,
    more: bool,
    loading: bool,
    error: Option<String>,
    selected: Option<String>,
    info: Option<Result<Info, String>>,
    _sub: gpui::Subscription,
}

impl gpui::EventEmitter<ShowDiff> for GitView {}

impl GitView {
    pub fn new(workspace: Entity<Workspace>, cx: &mut Context<Self>) -> Self {
        let sub = cx.subscribe(&workspace, |v, _, e: &Event, cx| match e {
            Event::Projects => v.read_repos(cx),
            Event::Heard { root, event, .. } if event == "git.changed" => {
                v.read_status(root.clone(), cx);
                if v.chosen.as_ref().is_some_and(|c| &c.root == root) {
                    v.load(false, cx);
                }
            }
            _ => {}
        });
        let mut v = GitView {
            workspace,
            status: HashMap::new(),
            chosen: None,
            current_only: false,
            commits: vec![],
            graph: vec![],
            lanes: 0,
            more: false,
            loading: false,
            error: None,
            selected: None,
            info: None,
            _sub: sub,
        };
        v.read_repos(cx);
        v
    }

    pub fn commits(&self) -> &[Commit] {
        &self.commits
    }

    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    fn read_repos(&mut self, cx: &mut Context<Self>) {
        let roots: Vec<String> = self
            .workspace
            .read(cx)
            .projects()
            .iter()
            .map(|f| f.root.clone())
            .collect();
        self.status.retain(|r, _| roots.contains(r));
        for root in roots {
            self.read_status(root, cx);
        }
    }

    fn read_status(&mut self, root: String, cx: &mut Context<Self>) {
        let Some(code) = self.workspace.read(cx).code(&root) else {
            return;
        };
        let call = code.call("git.status", json!({}));
        cx.spawn(async move |this, cx| {
            let Ok(s) = call.await else { return };
            let _ = this.update(cx, |v, cx| {
                v.status.insert(root, commit::repos(&s));
                // The first repository, until one listed is chosen: a
                // repository is listed under the first project holding
                // it, which may be read after another.
                let repos: Vec<Chosen> = v
                    .repos(cx)
                    .into_iter()
                    .map(|(root, r)| Chosen { root, repo: r.path })
                    .collect();
                let listed = v.chosen.as_ref().is_some_and(|c| repos.contains(c));
                if !listed && let Some(first) = repos.into_iter().next() {
                    v.choose(first, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Every repository, each once, in the workspace's order.
    fn repos(&self, cx: &gpui::App) -> Vec<(String, Repo)> {
        let projects: Vec<(String, Vec<Repo>)> = self
            .workspace
            .read(cx)
            .projects()
            .iter()
            .filter_map(|f| Some((f.root.clone(), self.status.get(&f.root)?.clone())))
            .collect();
        commit::unique(&projects)
            .into_iter()
            .filter(|(_, r)| !r.uninitialised && r.error.is_none())
            .map(|(root, r)| (root.to_string(), r.clone()))
            .collect()
    }

    fn choose(&mut self, c: Chosen, cx: &mut Context<Self>) {
        if self.chosen.as_ref() == Some(&c) {
            return;
        }
        self.chosen = Some(c);
        self.commits.clear();
        self.selected = None;
        self.info = None;
        self.load(false, cx);
    }

    fn set_current_only(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.current_only != on {
            self.current_only = on;
            self.load(false, cx);
        }
    }

    /// Reads the history from its start, as much of it as is shown, or
    /// the next page of it.
    fn load(&mut self, next: bool, cx: &mut Context<Self>) {
        let Some(c) = self.chosen.clone() else {
            return;
        };
        let Some(code) = self.workspace.read(cx).code(&c.root) else {
            return;
        };
        if next && self.loading {
            return;
        }
        let skip = if next { self.commits.len() } else { 0 };
        let max = if next {
            PAGE
        } else {
            PAGE.max(self.commits.len())
        };
        let mut params = json!({ "skip": skip, "max": max, "repo": c.repo });
        if self.current_only {
            params["ref"] = Value::String("HEAD".into());
        }
        let call = code.call("git.log", params);
        let current_only = self.current_only;
        self.loading = true;
        cx.spawn(async move |this, cx| {
            let result = call
                .await
                .and_then(|v| serde_json::from_value::<Log>(v).map_err(|e| e.to_string()));
            let _ = this.update(cx, |v, cx| {
                v.loading = false;
                if v.chosen.as_ref() != Some(&c) || v.current_only != current_only {
                    return;
                }
                match result {
                    Ok(log) => {
                        if next {
                            v.commits.extend(log.commits);
                        } else {
                            v.commits = log.commits;
                        }
                        v.more = log.more;
                        v.error = None;
                        (v.graph, v.lanes) = layout(&v.commits);
                        if v.selected.is_none()
                            && let Some(first) = v.commits.first()
                        {
                            let hash = first.hash.clone();
                            v.select(hash, cx);
                        }
                    }
                    Err(e) => v.error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn select(&mut self, hash: String, cx: &mut Context<Self>) {
        let Some(c) = self.chosen.clone() else {
            return;
        };
        let Some(code) = self.workspace.read(cx).code(&c.root) else {
            return;
        };
        self.selected = Some(hash.clone());
        let call = code.call("git.commitInfo", json!({ "hash": hash, "repo": c.repo }));
        cx.spawn(async move |this, cx| {
            let result = call
                .await
                .and_then(|v| serde_json::from_value::<Info>(v).map_err(|e| e.to_string()));
            let _ = this.update(cx, |v, cx| {
                if v.selected.as_deref() == Some(hash.as_str()) {
                    v.info = Some(result);
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Shows the diff of the chosen commit's file at index i.
    pub fn diff_file(&self, i: usize, cx: &mut Context<Self>) {
        if let Some(Ok(info)) = &self.info
            && let Some(f) = info.files.get(i)
        {
            self.diff(&f.clone(), cx);
        }
    }

    /// Shows a file's change in the commit chosen.
    fn diff(&self, f: &Named, cx: &mut Context<Self>) {
        let (Some(c), Some(Ok(info))) = (&self.chosen, &self.info) else {
            return;
        };
        let against = info
            .parents
            .first()
            .cloned()
            .unwrap_or_else(|| format!("{}^", info.hash));
        cx.emit(ShowDiff(Spec {
            root: c.root.clone(),
            path: f.path.clone(),
            against,
            from: f.from.clone(),
            to: Some(info.hash.clone()),
        }));
    }

    fn row(&self, i: usize, p: &Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        let c = &self.commits[i];
        let selected = self.selected.as_deref() == Some(c.hash.as_str());
        let row = self.graph.get(i).cloned();
        let merge = c.parents.len() > 1;
        let lanes = self.lanes.min(16);
        let width = lanes as f32 * LANE + 8.;
        let bg = p.bg;
        let graph = canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                if let Some(row) = row {
                    paint_row(&row, merge, bounds, bg, window);
                }
            },
        )
        .w(px(width))
        .h(px(ROW))
        .flex_none();
        let hash = c.hash.clone();
        let mut subject = h_flex().flex_1().min_w_0().gap(px(4.)).overflow_hidden();
        for r in &c.refs {
            subject = subject.child(badge(r, p));
        }
        subject = subject.child(
            div()
                .min_w_0()
                .truncate()
                .text_color(p.text)
                .child(c.subject.clone()),
        );
        h_flex()
            .id(("commit", i))
            .w_full()
            .h(px(ROW))
            .pr(px(10.))
            .gap(px(8.))
            .cursor_pointer()
            .when(selected, |d| d.bg(p.sidebar_active))
            .when(!selected, |d| d.hover(|s| s.bg(p.surface_2)))
            .child(graph)
            .child(subject)
            .child(
                div()
                    .w(px(140.))
                    .flex_none()
                    .truncate()
                    .text_color(p.muted)
                    .child(c.author.clone()),
            )
            .child(
                div()
                    .w(px(90.))
                    .flex_none()
                    .text_color(p.muted)
                    .child(when(c.time, Local::now())),
            )
            .on_click(cx.listener(move |v, _, _, cx| v.select(hash.clone(), cx)))
            .into_any_element()
    }

    fn details(&self, p: &Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        let info = match &self.info {
            None => return note("Choose a commit to see its files and message.", p),
            Some(Err(e)) => return note(e.clone(), p),
            Some(Ok(info)) => info,
        };
        let mut files = v_flex().child(
            h_flex()
                .gap(px(6.))
                .h(px(20.))
                .px(px(4.))
                .text_size(px(11.))
                .text_color(p.muted)
                .child("Files")
                .child(info.files.len().to_string()),
        );
        if info.files.is_empty() {
            files = files.child(note("No files changed in this folder.", p));
        }
        for (i, f) in info.files.iter().enumerate() {
            let (name, dir) = match f.path.rsplit_once('/') {
                Some((d, n)) => (n.to_string(), d.to_string()),
                None => (f.path.clone(), String::new()),
            };
            let colour = status_colour(&f.status, p);
            let file = f.clone();
            files = files.child(
                h_flex()
                    .id(("commit-file", i))
                    .h(px(20.))
                    .gap(px(6.))
                    .px(px(6.))
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
                    .child(div().flex_none().text_color(colour).child(name))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.))
                            .text_color(p.muted)
                            .child(dir),
                    )
                    .on_click(cx.listener(move |v, _, _, cx| v.diff(&file, cx))),
            );
        }
        let (subject, body) = match info.message.split_once('\n') {
            Some((s, b)) => (s.to_string(), b.trim().to_string()),
            None => (info.message.clone(), String::new()),
        };
        let at = |t: f64| {
            Local
                .timestamp_opt(t as i64, 0)
                .single()
                .map(|t| t.format("%-d %b %Y, %H:%M").to_string())
                .unwrap_or_default()
        };
        let mut meta = v_flex()
            .gap(px(2.))
            .text_size(px(11.))
            .text_color(p.muted)
            .child(format!(
                "{} by {} <{}>, {}",
                &info.hash[..info.hash.len().min(8)],
                info.author,
                info.email,
                at(info.time)
            ));
        if info.committer != info.author {
            meta = meta.child(format!(
                "committed by {}, {}",
                info.committer,
                at(info.committer_time)
            ));
        }
        if !info.parents.is_empty() {
            let mut parents = h_flex().gap(px(4.)).child(if info.parents.len() > 1 {
                "Parents"
            } else {
                "Parent"
            });
            for (i, h) in info.parents.iter().enumerate() {
                let hash = h.clone();
                parents = parents.child(
                    div()
                        .id(("parent", i))
                        .text_color(p.accent)
                        .cursor_pointer()
                        .hover(|s| s.underline())
                        .child(h[..h.len().min(8)].to_string())
                        .on_click(cx.listener(move |v, _, _, cx| v.select(hash.clone(), cx))),
                );
            }
            meta = meta.child(parents);
        }
        let message = v_flex()
            .gap(px(6.))
            .p(px(8.))
            .border_t_1()
            .border_color(p.border)
            .child(div().font_weight(FontWeight::SEMIBOLD).child(subject))
            .when(!body.is_empty(), |d| {
                d.child(div().text_color(p.text).whitespace_normal().child(body))
            })
            .child(meta);
        v_flex()
            .id("commit-details")
            .size_full()
            .overflow_y_scroll()
            .child(div().p(px(4.)).child(files))
            .child(message)
            .into_any_element()
    }
}

/// Lane colours, which read on light and dark alike.
fn lane(color: usize) -> Hsla {
    const HUES: [f32; 8] = [174., 212., 268., 28., 330., 140., 0., 48.];
    hsla(HUES[color % 8] / 360., 0.6, 0.48, 1.)
}

fn x(lane: usize, origin: Pixels) -> Pixels {
    origin + px(6. + lane as f32 * LANE + LANE / 2. - 2.)
}

/// Draws one row of the graph: the lanes passing, the lines into its
/// commit and out of it to its parents, and the commit itself.
fn paint_row(row: &GraphRow, merge: bool, b: Bounds<Pixels>, bg: Hsla, window: &mut Window) {
    let (top, h) = (b.origin.y, b.size.height);
    let m = top + h / 2.;
    let bottom = top + h;
    for e in &row.edges {
        let (a, c) = (x(e.from, b.origin.x), x(e.to, b.origin.x));
        let mut path = PathBuilder::stroke(px(1.5));
        match e.kind {
            EdgeKind::Through => {
                path.move_to(point(a, top));
                path.line_to(point(a, bottom));
            }
            EdgeKind::In => {
                path.move_to(point(a, top));
                if a == c {
                    path.line_to(point(a, m));
                } else {
                    path.cubic_bezier_to(point(c, m), point(a, m), point(c, top));
                }
            }
            EdgeKind::Out => {
                path.move_to(point(a, m));
                if a == c {
                    path.line_to(point(a, bottom));
                } else {
                    path.cubic_bezier_to(point(c, bottom), point(a, bottom), point(c, m));
                }
            }
        }
        if let Ok(path) = path.build() {
            window.paint_path(path, lane(e.color));
        }
    }
    let r = px(if merge { 3. } else { 4. });
    let centre = point(x(row.col, b.origin.x), m);
    let dot = Bounds::new(
        point(centre.x - r, centre.y - r),
        gpui::size(r * 2., r * 2.),
    );
    let colour = lane(row.color);
    let mut q = gpui::fill(dot, if merge { bg } else { colour }).corner_radii(r);
    if merge {
        q = q.border_widths(px(1.5)).border_color(colour);
    }
    window.paint_quad(q);
}

fn badge(r: &Ref, p: &Palette) -> impl IntoElement {
    let (fg, bg) = match r.kind.as_str() {
        "head" => (p.accent, p.accent_soft),
        "tag" => (p.wait, p.wait.opacity(0.14)),
        "remote" => (p.muted, p.surface_2),
        _ => (p.good, p.good.opacity(0.14)),
    };
    div()
        .flex_none()
        .px(px(5.))
        .rounded(px(3.))
        .text_size(px(10.5))
        .text_color(fg)
        .bg(bg)
        .child(if r.kind == "tag" {
            format!("◇ {}", r.name)
        } else {
            r.name.clone()
        })
}

/// A file's colour by how it changed: A, M, D, R or C.
pub(crate) fn status_colour(s: &str, p: &Palette) -> Hsla {
    match s {
        "A" | "C" => p.good,
        "D" => p.bad,
        _ => p.busy,
    }
}

fn note(text: impl Into<SharedString>, p: &Palette) -> gpui::AnyElement {
    div()
        .p(px(8.))
        .text_size(px(11.5))
        .text_color(p.muted)
        .child(text.into())
        .into_any_element()
}

impl Render for GitView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let state = self.workspace.read(cx).state().clone();
        let repos = self.repos(cx);

        let mut bar = h_flex().gap(px(4.)).px(px(8.)).h(px(28.)).flex_none();
        if repos.len() > 1 {
            for (i, (root, r)) in repos.iter().enumerate() {
                let c = Chosen {
                    root: root.clone(),
                    repo: r.path.clone(),
                };
                let on = self.chosen.as_ref() == Some(&c);
                bar = bar.child(
                    chip(("repo", i), r.name.clone(), on, &p)
                        .on_click(cx.listener(move |v, _, _, cx| v.choose(c.clone(), cx))),
                );
            }
            bar = bar.child(div().w(px(1.)).h(px(14.)).mx(px(4.)).bg(p.border));
        }
        let current = self.current_only;
        let branch = self
            .chosen
            .as_ref()
            .and_then(|c| repos.iter().find(|(r, x)| r == &c.root && x.path == c.repo))
            .and_then(|(_, r)| r.branch.clone())
            .unwrap_or_else(|| "HEAD".into());
        bar = bar
            .child(
                chip("all-branches", "All branches".to_string(), !current, &p)
                    .on_click(cx.listener(|v, _, _, cx| v.set_current_only(false, cx))),
            )
            .child(
                chip("current-branch", format!("⑂ {branch}"), current, &p)
                    .on_click(cx.listener(|v, _, _, cx| v.set_current_only(true, cx))),
            );

        let log: gpui::AnyElement = match (&state, &self.error) {
            (State::Connecting, _) => note("Connecting…", &p),
            (State::Reconnecting(why), _) => note(State::reconnecting(why), &p),
            (State::Failed(e), _) => note(format!("Could not reach the Code service: {e}"), &p),
            (_, Some(e)) => note(e.clone(), &p),
            _ if self.chosen.is_none() => note(
                if self.status.is_empty() {
                    "Reading…"
                } else {
                    "No git repository in the environment's projects."
                },
                &p,
            ),
            _ => {
                let more = self.more;
                let n = self.commits.len() + usize::from(more);
                uniform_list(
                    "git-log",
                    n,
                    cx.processor(move |v, range: Range<usize>, _, cx| {
                        let p = palette::of(cx);
                        range
                            .map(|i| {
                                if i < v.commits.len() {
                                    v.row(i, &p, cx)
                                } else {
                                    div()
                                        .id("git-more")
                                        .h(px(ROW))
                                        .px(px(10.))
                                        .text_color(p.accent)
                                        .cursor_pointer()
                                        .child(if v.loading { "Reading…" } else { "Show more" })
                                        .on_click(cx.listener(|v, _, _, cx| v.load(true, cx)))
                                        .into_any_element()
                                }
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .flex_1()
                .into_any_element()
            }
        };
        h_flex()
            .size_full()
            .min_h_0()
            .text_size(px(12.))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(bar)
                    .child(v_flex().flex_1().min_h_0().child(log)),
            )
            .child(
                div()
                    .w(px(380.))
                    .flex_none()
                    .h_full()
                    .border_l_1()
                    .border_color(p.border)
                    .child(self.details(&p, cx)),
            )
    }
}

/// A toggle among a few: on, or muted until hovered.
pub(crate) fn chip(
    id: impl Into<gpui::ElementId>,
    label: String,
    on: bool,
    p: &Palette,
) -> gpui::Stateful<gpui::Div> {
    let hover = p.surface_2;
    div()
        .id(id)
        .px(px(7.))
        .py(px(1.))
        .rounded(px(4.))
        .text_size(px(11.))
        .cursor_pointer()
        .when(on, |d| d.bg(p.accent_soft).text_color(p.accent))
        .when(!on, move |d| {
            d.text_color(p.muted).hover(move |s| s.bg(hover))
        })
        .child(label)
}
