//! Search Everywhere (⌘K): one box for environments in every place, the
//! open environment's files (search.files of each of its projects), and
//! what can be done: its tool windows, its actions, and the app's own.
//! What is typed is matched as IntelliJ's is (fuzzy.rs); the arrows choose,
//! Enter goes, Escape leaves.

use std::collections::HashSet;

use gpui::{
    Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, HighlightStyle, IntoElement,
    KeyDownEvent, ParentElement, Render, SharedString, Styled, StyledText, Window, div, prelude::*,
    px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{h_flex, v_flex};
use serde_json::json;

use crate::envtab::Tool;
use crate::fuzzy;
use crate::model::{self, Action, Card, Rank, Tone};
use crate::palette::{self, Palette};
use crate::places::Places;
use crate::ui;
use crate::workspace::Workspace;

/// What a result leads to.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Env {
        place: String,
        env: String,
    },
    File {
        root: String,
        path: String,
    },
    Tool(Tool),
    Act {
        place: String,
        env: String,
        action: Action,
    },
    SuspendIdle,
    AddPlace,
}

/// Asks the shell to go to a result, or (None) to close.
#[derive(Debug, Clone, PartialEq)]
pub struct Chose(pub Option<Target>);

impl EventEmitter<Chose> for SearchView {}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub group: &'static str,
    pub label: String,
    pub detail: String,
    pub tone: Option<Tone>,
    /// The byte offsets of label matched, to show.
    pub matched: Vec<usize>,
    pub target: Target,
}

/// A candidate before matching.
struct Candidate {
    group: &'static str,
    label: String,
    /// What else it is found by: an environment's name and place.
    also: String,
    detail: String,
    tone: Option<Tone>,
    target: Target,
}

/// How many of each group are shown.
const SHOWN: usize = 8;

/// The results for a query: each group's best, in the groups' order, an
/// empty query showing environments and actions as they are.
fn rank(query: &str, candidates: &[Candidate], groups: &[&'static str]) -> Vec<Item> {
    let mut out = vec![];
    for g in groups {
        let mut hits: Vec<(i64, usize, Vec<usize>)> = candidates
            .iter()
            .enumerate()
            .filter(|(_, c)| c.group == *g)
            .filter_map(|(i, c)| {
                let on_label = fuzzy::score(query, &c.label);
                let on_also = fuzzy::score(query, &c.also).map(|(s, _)| (s - 50, vec![]));
                let (s, at) = match (on_label, on_also) {
                    (Some(a), Some(b)) => {
                        if a.0 >= b.0 {
                            a
                        } else {
                            b
                        }
                    }
                    (a, b) => a.or(b)?,
                };
                Some((s, i, at))
            })
            .collect();
        if !query.trim().is_empty() {
            hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        }
        for (_, i, at) in hits.into_iter().take(SHOWN) {
            let c = &candidates[i];
            out.push(Item {
                group: c.group,
                label: c.label.clone(),
                detail: c.detail.clone(),
                tone: c.tone,
                matched: at,
                target: c.target.clone(),
            });
        }
    }
    out
}

pub struct SearchView {
    places: Entity<Places>,
    /// The environment shown, if one is, and its projects.
    active: Option<(String, String)>,
    workspace: Option<Entity<Workspace>>,
    query: Entity<InputState>,
    /// The open environment's files, as each project lists them: its root,
    /// its name and the file's path in it.
    files: Vec<(String, String, String)>,
    items: Vec<Item>,
    selected: usize,
    _subs: Vec<gpui::Subscription>,
}

impl SearchView {
    pub fn new(
        places: Entity<Places>,
        active: Option<(String, String)>,
        workspace: Option<Entity<Workspace>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search Everywhere"));
        let subs = vec![cx.subscribe(&query, |v, _, e: &InputEvent, cx| match e {
            InputEvent::Change => {
                v.selected = 0;
                v.refresh(cx);
            }
            InputEvent::PressEnter { .. } => v.choose(v.selected, cx),
            _ => {}
        })];
        let mut v = SearchView {
            places,
            active,
            workspace,
            query,
            files: vec![],
            items: vec![],
            selected: 0,
            _subs: subs,
        };
        v.list_files(cx);
        v.refresh(cx);
        v
    }

    pub fn query(&self) -> &Entity<InputState> {
        &self.query
    }

    /// Puts text in the box, as if typed: setting the value is not told
    /// as a change.
    pub fn set_query(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.query
            .update(cx, |q, cx| q.set_value(text.to_string(), window, cx));
        self.selected = 0;
        self.refresh(cx);
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// Reads each project's files, once.
    fn list_files(&mut self, cx: &mut Context<Self>) {
        let Some(ws) = &self.workspace else { return };
        let projects: Vec<(String, String, crate::code::Code)> = ws
            .read(cx)
            .projects()
            .iter()
            .map(|f| (f.root.clone(), f.name.clone(), f.code.clone()))
            .collect();
        for (root, name, code) in projects {
            let call = code.call("search.files", json!({}));
            cx.spawn(async move |this, cx| {
                let Ok(list) = call.await else { return };
                let paths: Vec<String> = list["files"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|f| f.as_str().map(str::to_string))
                    .collect();
                let _ = this.update(cx, |v, cx| {
                    // A file a project nearer the start holds (a clone in
                    // the other's folder) is listed under it alone.
                    let mut seen: HashSet<String> = v
                        .files
                        .iter()
                        .map(|(r, _, p)| format!("{}/{p}", r.trim_end_matches('/')))
                        .collect();
                    for p in paths {
                        if seen.insert(format!("{}/{p}", root.trim_end_matches('/'))) {
                            v.files.push((root.clone(), name.clone(), p));
                        }
                    }
                    v.refresh(cx);
                });
            })
            .detach();
        }
    }

    fn candidates(&self, cx: &gpui::App) -> (Vec<Candidate>, Vec<&'static str>) {
        let cards: Vec<Card> = model::home(self.places.read(cx).list(), chrono::Utc::now());
        let mut out = vec![];
        for c in &cards {
            out.push(Candidate {
                group: "Environments",
                label: c.title.clone(),
                also: format!("{} {}", c.name, c.place_name),
                detail: format!(
                    "{} · {} · {}",
                    c.name,
                    c.place_name,
                    c.status.to_lowercase()
                ),
                tone: Some(c.tone),
                target: Target::Env {
                    place: c.place.clone(),
                    env: c.env.clone(),
                },
            });
        }
        let env_name = self.active.as_ref().and_then(|(p, e)| {
            cards
                .iter()
                .find(|c| &c.place == p && &c.env == e)
                .map(|c| c.name.clone())
        });
        let files_group = "Files";
        for (root, project, path) in &self.files {
            out.push(Candidate {
                group: files_group,
                label: path.clone(),
                also: String::new(),
                detail: project.clone(),
                tone: None,
                target: Target::File {
                    root: root.clone(),
                    path: path.clone(),
                },
            });
        }
        if let Some((place, env)) = &self.active {
            let name = env_name.clone().unwrap_or_default();
            for (tool, label) in [
                (Tool::Project, "Project"),
                (Tool::Terminal, "Terminal"),
                (Tool::Commit, "Commit"),
                (Tool::Git, "Git"),
                (Tool::Find, "Find in Files"),
                (Tool::Problems, "Problems"),
                (Tool::Agents, "Agents"),
            ] {
                out.push(Candidate {
                    group: "Actions",
                    label: label.into(),
                    also: String::new(),
                    detail: "tool window".into(),
                    tone: None,
                    target: Target::Tool(tool),
                });
            }
            let rank = cards
                .iter()
                .find(|c| &c.place == place && &c.env == env)
                .map(|c| c.rank);
            let action = match rank {
                Some(Rank::Suspended) => Some(Action::Resume),
                Some(Rank::Stopped | Rank::Failed) => Some(Action::Start),
                Some(Rank::Starting) | None => None,
                Some(_) => Some(Action::Suspend),
            };
            if let Some(action) = action {
                out.push(Candidate {
                    group: "Actions",
                    label: format!("{} {name}", action.label()),
                    also: String::new(),
                    detail: "this environment".into(),
                    tone: None,
                    target: Target::Act {
                        place: place.clone(),
                        env: env.clone(),
                        action,
                    },
                });
            }
        }
        let idle = model::idle(&cards).len();
        if idle > 0 {
            out.push(Candidate {
                group: "Actions",
                label: "Suspend all idle environments".into(),
                also: String::new(),
                detail: idle.to_string(),
                tone: None,
                target: Target::SuspendIdle,
            });
        }
        out.push(Candidate {
            group: "Actions",
            label: "Add a place".into(),
            also: "server".into(),
            detail: String::new(),
            tone: None,
            target: Target::AddPlace,
        });
        (out, vec!["Environments", files_group, "Actions"])
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).value().to_string();
        let (candidates, groups) = self.candidates(cx);
        // Files are many: shown once something is typed.
        let candidates: Vec<Candidate> = candidates
            .into_iter()
            .filter(|c| c.group != "Files" || !query.trim().is_empty())
            .collect();
        self.items = rank(&query, &candidates, &groups);
        self.selected = self.selected.min(self.items.len().saturating_sub(1));
        cx.notify();
    }

    pub fn choose(&mut self, i: usize, cx: &mut Context<Self>) {
        if let Some(item) = self.items.get(i) {
            cx.emit(Chose(Some(item.target.clone())));
        }
    }

    fn key(&mut self, e: &KeyDownEvent, cx: &mut Context<Self>) {
        match e.keystroke.key.as_str() {
            "down" => {
                if !self.items.is_empty() {
                    self.selected = (self.selected + 1) % self.items.len();
                }
            }
            "up" => {
                if !self.items.is_empty() {
                    self.selected = (self.selected + self.items.len() - 1) % self.items.len();
                }
            }
            "escape" => cx.emit(Chose(None)),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }
}

impl Focusable for SearchView {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        self.query.read(cx).focus_handle(cx)
    }
}

impl Render for SearchView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let mut list = v_flex().px(px(4.)).pb(px(6.));
        let mut group = "";
        for (i, item) in self.items.iter().enumerate() {
            if item.group != group {
                group = item.group;
                let heading = if group == "Files" {
                    let name = self
                        .active
                        .as_ref()
                        .and_then(|(pl, e)| {
                            model::home(self.places.read(cx).list(), chrono::Utc::now())
                                .into_iter()
                                .find(|c| &c.place == pl && &c.env == e)
                                .map(|c| c.name)
                        })
                        .unwrap_or_default();
                    format!("FILES IN {}", name.to_uppercase())
                } else {
                    group.to_uppercase()
                };
                list = list.child(
                    div()
                        .px(px(10.))
                        .pt(px(6.))
                        .pb(px(2.))
                        .text_size(px(10.5))
                        .text_color(p.muted)
                        .child(heading),
                );
            }
            list = list.child(self.row(i, item, &p, cx));
        }
        if self.items.is_empty() {
            list = list.child(
                div()
                    .px(px(10.))
                    .py(px(8.))
                    .text_color(p.muted)
                    .child("Nothing matches."),
            );
        }
        v_flex()
            .w(px(460.))
            .max_h(px(480.))
            .rounded(px(10.))
            .border_1()
            .border_color(p.border_strong)
            .bg(p.surface)
            .shadow_lg()
            .overflow_hidden()
            .text_size(px(12.5))
            .capture_key_down(cx.listener(|v, e: &KeyDownEvent, _, cx| v.key(e, cx)))
            .child(
                div()
                    .px(px(8.))
                    .py(px(6.))
                    .border_b_1()
                    .border_color(p.border)
                    .child(Input::new(&self.query).appearance(false)),
            )
            .child(
                div()
                    .id("search-results")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(list),
            )
    }
}

impl SearchView {
    fn row(&self, i: usize, item: &Item, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let on = i == self.selected;
        let highlight = HighlightStyle {
            font_weight: Some(FontWeight::BOLD),
            color: Some(p.accent),
            ..Default::default()
        };
        let label = StyledText::new(SharedString::from(item.label.clone())).with_highlights(
            item.matched.iter().filter_map(|&b| {
                let len = item.label[b..].chars().next()?.len_utf8();
                Some((b..b + len, highlight))
            }),
        );
        h_flex()
            .id(("search-row", i))
            .gap(px(8.))
            .px(px(10.))
            .py(px(5.))
            .rounded(px(6.))
            .cursor_pointer()
            .when(on, |d| d.bg(p.accent_soft))
            .when(!on, |d| d.hover(|s| s.bg(p.surface_2)))
            .when_some(item.tone, |d, t| d.child(ui::dot(t, p)))
            .child(div().min_w_0().truncate().child(label))
            .child(div().flex_1())
            .child(
                div()
                    .flex_none()
                    .max_w(px(220.))
                    .truncate()
                    .text_size(px(11.))
                    .text_color(p.muted)
                    .child(item.detail.clone()),
            )
            .on_click(cx.listener(move |v, _, _, cx| v.choose(i, cx)))
    }
}
