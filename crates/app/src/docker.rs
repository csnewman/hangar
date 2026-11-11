//! The Docker tool window, as IntelliJ's Services window: the environment's
//! containers by Compose project, then its images, on the left, kept
//! current as Docker reports changes; on the right, the chosen container's
//! log, followed, or its details, with what can be done to it and a shell
//! in it as a terminal tab. A Compose project and an image have their
//! details and actions likewise, and every row a menu.

use std::collections::HashSet;
use std::time::Duration;

use chrono::Local;
use gpui::{
    App, Context, Entity, FocusHandle, Focusable, FontWeight, IntoElement, KeyDownEvent,
    ParentElement, Render, SharedString, Styled, WeakEntity, Window, div, prelude::*, px, svg,
};
use gpui_component::menu::{ContextMenuExt, PopupMenu, PopupMenuItem};
use gpui_component::{h_flex, v_flex};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::assets;
use crate::gitlog::{chip, when};
use crate::logview::LogPane;
use crate::model::Tone;
use crate::palette::{self, Palette};
use crate::project::OpenFile;
use crate::ui::dot;
use crate::workspace::{Event, PRIMARY, State, Workspace};

/// A container, as the Code service's docker.overview gives it.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
    /// running, exited, paused, created, restarting, removing or dead.
    pub state: String,
    /// Docker's own words: "Up 3 minutes", "Exited (1) 2 hours ago".
    pub status: String,
    /// Its ports: "8080→80/tcp" when published, else "6379/tcp".
    pub ports: Vec<String>,
    pub created: f64,
    pub command: String,
    pub project: Option<String>,
    pub service: Option<String>,
    #[serde(rename = "workingDir")]
    pub working_dir: Option<String>,
    #[serde(rename = "configFiles")]
    pub config_files: Option<Vec<String>>,
}

impl Container {
    pub fn running(&self) -> bool {
        self.state == "running"
    }

    /// Running or paused: what Stop applies to.
    pub fn up(&self) -> bool {
        self.running() || self.state == "paused"
    }

    /// Its name in a list: under its project, its service's.
    pub fn label(&self, nested: bool) -> String {
        match &self.service {
            Some(s) if nested => s.clone(),
            _ => self.name.clone(),
        }
    }

    /// The code an exited container's status gives.
    pub fn exit_code(&self) -> Option<i64> {
        let rest = self.status.strip_prefix("Exited (")?;
        rest.split(')').next()?.parse().ok()
    }

    /// Its dot: running, paused, coming up, stopped, or stopped by a fault.
    pub fn tone(&self) -> Tone {
        match self.state.as_str() {
            "running" => Tone::Good,
            "paused" => Tone::Wait,
            "restarting" | "removing" => Tone::Busy,
            "dead" => Tone::Bad,
            "exited" if self.exit_code().is_some_and(|c| c != 0) => Tone::Bad,
            _ => Tone::Idle,
        }
    }

    /// What a row says beside its name: a running container's published
    /// ports, else its image; else how it stopped.
    pub fn meta(&self) -> String {
        if self.running() {
            let published: Vec<String> = self
                .ports
                .iter()
                .filter_map(|p| p.split_once('→').map(|(public, _)| format!(":{public}")))
                .collect();
            return if published.is_empty() {
                self.image.clone()
            } else {
                published.join(", ")
            };
        }
        match self.exit_code() {
            Some(code) => format!("exited ({code})"),
            None => self.state.clone(),
        }
    }
}

/// An image.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct Image {
    pub id: String,
    pub tags: Vec<String>,
    pub size: f64,
    pub created: f64,
    /// How many containers use it.
    pub containers: f64,
}

impl Image {
    /// Its first tag, else the start of its ID.
    pub fn name(&self) -> String {
        self.tags.first().cloned().unwrap_or_else(|| {
            let id = self.id.strip_prefix("sha256:").unwrap_or(&self.id);
            id.chars().take(12).collect()
        })
    }

    pub fn in_use(&self) -> bool {
        self.containers > 0.
    }
}

/// The environment's Docker: whether it can be reached, why not, and what
/// it holds.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct Overview {
    pub available: bool,
    pub why: Option<String>,
    pub containers: Vec<Container>,
    pub images: Vec<Image>,
}

impl Overview {
    /// How many containers run, of how many.
    pub fn summary(&self) -> String {
        let up = self.containers.iter().filter(|c| c.running()).count();
        format!("{up} of {} running", self.containers.len())
    }

    /// A Compose project's containers.
    pub fn project(&self, name: &str) -> Vec<&Container> {
        self.containers
            .iter()
            .filter(|c| c.project.as_deref() == Some(name))
            .collect()
    }

    pub fn container(&self, id: &str) -> Option<&Container> {
        self.containers.iter().find(|c| c.id == id)
    }

    pub fn image(&self, id: &str) -> Option<&Image> {
        self.images.iter().find(|i| i.id == id)
    }
}

/// A row of the list.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    /// A Compose project: how many of its containers run, and whether its
    /// rows are shown.
    Project {
        name: String,
        up: usize,
        count: usize,
        open: bool,
    },
    /// A container, under its project or alone.
    Container {
        id: String,
        nested: bool,
    },
    /// The images' heading: how many, their size, and whether they are
    /// shown.
    Images {
        count: usize,
        size: f64,
        open: bool,
    },
    Image {
        id: String,
    },
}

/// The rows: each Compose project, by name, with its containers by
/// service, those of none after them, then the images, tagged ones first,
/// closed projects and images showing their heading alone.
pub fn rows(o: &Overview, closed: &HashSet<String>, images_open: bool) -> Vec<Row> {
    let mut out = Vec::new();
    if !o.available {
        return out;
    }
    let mut projects: Vec<&str> = o
        .containers
        .iter()
        .filter_map(|c| c.project.as_deref())
        .collect();
    projects.sort_unstable();
    projects.dedup();
    for name in projects {
        let mut list = o.project(name);
        list.sort_by_key(|c| c.label(true));
        let open = !closed.contains(name);
        out.push(Row::Project {
            name: name.to_string(),
            up: list.iter().filter(|c| c.running()).count(),
            count: list.len(),
            open,
        });
        if open {
            out.extend(list.into_iter().map(|c| Row::Container {
                id: c.id.clone(),
                nested: true,
            }));
        }
    }
    let mut alone: Vec<&Container> = o
        .containers
        .iter()
        .filter(|c| c.project.is_none())
        .collect();
    alone.sort_by_key(|c| c.name.clone());
    out.extend(alone.into_iter().map(|c| Row::Container {
        id: c.id.clone(),
        nested: false,
    }));
    out.push(Row::Images {
        count: o.images.len(),
        size: o.images.iter().fold(0., |n, i| n + i.size),
        open: images_open,
    });
    if images_open {
        let mut images: Vec<&Image> = o.images.iter().collect();
        images.sort_by_key(|i| (i.tags.is_empty(), i.name()));
        out.extend(images.into_iter().map(|i| Row::Image { id: i.id.clone() }));
    }
    out
}

/// A size in bytes, in the largest unit that keeps it above one.
pub fn bytes(n: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let (mut v, mut i) = (n, 0);
    while v >= 1000. && i < UNITS.len() - 1 {
        v /= 1000.;
        i += 1;
    }
    if v >= 100. || i == 0 {
        format!("{} {}", v.round(), UNITS[i])
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// The command a shell in a container runs: bash, where its image has it.
pub fn shell_command(name: &str) -> String {
    format!("docker exec -it {name} sh -c 'command -v bash >/dev/null && exec bash || exec sh'")
}

/// What the right side shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Chosen {
    Container(String),
    Project(String),
    Image(String),
}

/// A container's two views.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Log,
    Inspect,
}

/// Asks for a terminal tab running a command: a shell in a container.
#[derive(Debug, Clone, PartialEq)]
pub struct RunInTerminal(pub String);

impl gpui::EventEmitter<RunInTerminal> for DockerView {}
impl gpui::EventEmitter<OpenFile> for DockerView {}

/// The container whose log is followed.
struct Follow {
    id: String,
    /// The connection it is followed on, which stops it.
    code: crate::code::Code,
    /// The stream docker.logs gave, once it has answered.
    stream: Option<u64>,
    /// Why it stopped following.
    ended: Option<String>,
}

/// What a container's log said before docker.logs answered with its
/// stream's number.
enum Early {
    Text(u64, String),
    End(u64, String),
}

/// What a log said when the connection carrying it was lost.
const LOST: &str = "the connection was lost";

pub struct DockerView {
    workspace: Entity<Workspace>,
    overview: Option<Overview>,
    error: Option<String>,
    /// What is being done to: containers by ID, projects as project:NAME,
    /// images as image:ID.
    busy: HashSet<String>,
    closed: HashSet<String>,
    images_open: bool,
    chosen: Option<Chosen>,
    pane: Pane,
    log: Entity<LogPane>,
    follow: Option<Follow>,
    /// Each docker.logs asked counts; an answer to an older one is
    /// stopped, not shown.
    asked: u64,
    early: Vec<Early>,
    loading: bool,
    again: bool,
    focus: FocusHandle,
    _subs: Vec<gpui::Subscription>,
}

/// How often statuses ("Up 3 minutes") are read again, which move on
/// without Docker saying so.
const TICK: Duration = Duration::from_secs(30);

impl DockerView {
    pub fn new(workspace: Entity<Workspace>, cx: &mut Context<Self>) -> Self {
        let subs = vec![cx.subscribe(&workspace, |v, _, e: &Event, cx| match e {
            Event::Projects => v.load(cx),
            Event::Heard {
                root,
                event,
                params,
            } if root == PRIMARY => match event.as_str() {
                "docker.changed" => v.load(cx),
                "docker.log" => v.heard_log(params, cx),
                "docker.logEnd" => v.heard_end(params, cx),
                _ => {}
            },
            Event::Lost { root } if root == PRIMARY => {
                if let Some(f) = &mut v.follow {
                    f.stream = None;
                    f.ended = Some(LOST.into());
                }
                cx.notify();
            }
            _ => {}
        })];
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                if this.update(cx, |v, cx| v.load(cx)).is_err() {
                    return;
                }
            }
        })
        .detach();
        let mut v = DockerView {
            workspace,
            overview: None,
            error: None,
            busy: HashSet::new(),
            closed: HashSet::new(),
            images_open: false,
            chosen: None,
            pane: Pane::Log,
            log: cx.new(|cx| LogPane::new("docker-log", "Waiting for output…", cx)),
            follow: None,
            asked: 0,
            early: Vec::new(),
            loading: false,
            again: false,
            focus: cx.focus_handle(),
            _subs: subs,
        };
        v.load(cx);
        v
    }

    pub fn overview(&self) -> Option<&Overview> {
        self.overview.as_ref()
    }

    pub fn chosen(&self) -> Option<&Chosen> {
        self.chosen.as_ref()
    }

    pub fn log(&self) -> &Entity<LogPane> {
        &self.log
    }

    /// What the window's title says beside its name.
    pub fn summary(&self) -> Option<String> {
        self.overview
            .as_ref()
            .filter(|o| o.available)
            .map(Overview::summary)
    }

    fn code(&self, cx: &App) -> Option<crate::code::Code> {
        self.workspace.read(cx).primary()
    }

    /// Reads what Docker holds, asking again once an answer comes when more
    /// changed meanwhile. Asking also has the connection told of changes.
    pub fn load(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            self.again = true;
            return;
        }
        let Some(code) = self.code(cx) else { return };
        self.loading = true;
        let call = code.call("docker.overview", json!({}));
        cx.spawn(async move |this, cx| {
            let got = call
                .await
                .and_then(|v| serde_json::from_value::<Overview>(v).map_err(|e| e.to_string()));
            let _ = this.update(cx, |v, cx| {
                v.loading = false;
                match got {
                    Ok(o) => {
                        v.overview = Some(o);
                        v.error = None;
                        v.loaded(cx);
                    }
                    Err(e) => v.error = Some(e),
                }
                if std::mem::take(&mut v.again) {
                    v.load(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// What is chosen is kept while it is there; a log that ended because
    /// its container stopped, or the connection was lost, is followed
    /// again once it runs.
    fn loaded(&mut self, cx: &mut Context<Self>) {
        let Some(o) = &self.overview else { return };
        let gone = match &self.chosen {
            Some(Chosen::Container(id)) => o.container(id).is_none(),
            Some(Chosen::Project(name)) => o.project(name).is_empty(),
            Some(Chosen::Image(id)) => o.image(id).is_none(),
            None => false,
        };
        if gone {
            self.chosen = None;
            self.stop_following(cx);
            return;
        }
        let again = self.follow.as_ref().and_then(|f| {
            let c = o.container(&f.id)?;
            (f.ended.is_some() && c.running()).then(|| f.id.clone())
        });
        if let Some(id) = again
            && self.pane == Pane::Log
        {
            self.start_following(id, cx);
        }
    }

    /// Chooses a row's subject; a container shows its log, followed.
    pub fn choose(&mut self, chosen: Chosen, cx: &mut Context<Self>) {
        if self.chosen.as_ref() == Some(&chosen) {
            return;
        }
        self.chosen = Some(chosen.clone());
        match chosen {
            Chosen::Container(id) if self.pane == Pane::Log => self.start_following(id, cx),
            _ => self.stop_following(cx),
        }
        cx.notify();
    }

    pub fn show_pane(&mut self, pane: Pane, cx: &mut Context<Self>) {
        self.pane = pane;
        match (&self.chosen, pane) {
            (Some(Chosen::Container(id)), Pane::Log)
                if self
                    .follow
                    .as_ref()
                    .is_none_or(|f| &f.id != id || f.ended.is_some()) =>
            {
                let id = id.clone();
                self.start_following(id, cx)
            }
            (_, Pane::Inspect) => self.stop_following(cx),
            _ => {}
        }
        cx.notify();
    }

    /// Follows a container's output: its last thousand lines, then what it
    /// writes.
    fn start_following(&mut self, id: String, cx: &mut Context<Self>) {
        self.stop_following(cx);
        let Some(code) = self.code(cx) else { return };
        self.asked += 1;
        let asked = self.asked;
        self.early.clear();
        self.log.update(cx, |l, cx| {
            l.clear(cx);
            l.set_empty("Waiting for output…", cx);
        });
        self.follow = Some(Follow {
            id: id.clone(),
            code: code.clone(),
            stream: None,
            ended: None,
        });
        let call = code.call("docker.logs", json!({ "id": id, "tail": 1000 }));
        cx.spawn(async move |this, cx| {
            let got = call.await;
            let _ = this.update(cx, |v, cx| {
                let stream = got.as_ref().ok().and_then(|r| r["stream"].as_u64());
                if v.asked != asked {
                    if let (Some(s), Some(code)) = (stream, v.code(cx)) {
                        drop(code.call("docker.logsStop", json!({ "stream": s })));
                    }
                    return;
                }
                let Some(f) = &mut v.follow else { return };
                match (got, stream) {
                    (Ok(_), Some(s)) => {
                        f.stream = Some(s);
                        for e in std::mem::take(&mut v.early) {
                            match e {
                                Early::Text(n, text) if n == s => {
                                    v.log.update(cx, |l, cx| l.push(&text, cx))
                                }
                                Early::End(n, why) if n == s => v.ended(why, cx),
                                _ => {}
                            }
                        }
                    }
                    (Err(e), _) => v.ended(e, cx),
                    (Ok(_), None) => v.ended("Docker gave no log".into(), cx),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn stop_following(&mut self, _: &mut Context<Self>) {
        self.asked += 1;
        if let Some(Follow {
            stream: Some(s),
            code,
            ..
        }) = self.follow.take()
        {
            drop(code.call("docker.logsStop", json!({ "stream": s })));
        }
    }

    fn ended(&mut self, why: String, cx: &mut Context<Self>) {
        if let Some(f) = &mut self.follow {
            f.stream = None;
            f.ended = Some(why.clone());
        }
        if self.log.read(cx).lines().is_empty() {
            self.log
                .update(cx, |l, cx| l.set_empty(format!("No output: {why}."), cx));
        }
        cx.notify();
    }

    fn heard_log(&mut self, params: &Value, cx: &mut Context<Self>) {
        let (Some(n), Some(text)) = (params["stream"].as_u64(), params["text"].as_str()) else {
            return;
        };
        match &self.follow {
            Some(f) if f.stream == Some(n) => self.log.update(cx, |l, cx| l.push(text, cx)),
            Some(f) if f.stream.is_none() && f.ended.is_none() => {
                self.early.push(Early::Text(n, text.to_string()))
            }
            _ => {}
        }
    }

    fn heard_end(&mut self, params: &Value, cx: &mut Context<Self>) {
        let Some(n) = params["stream"].as_u64() else {
            return;
        };
        let why = params["why"].as_str().unwrap_or("it ended").to_string();
        match &self.follow {
            Some(f) if f.stream == Some(n) => self.ended(why, cx),
            Some(f) if f.stream.is_none() && f.ended.is_none() => {
                self.early.push(Early::End(n, why))
            }
            _ => {}
        }
    }

    /// Why the log shown is not being followed.
    pub fn not_following(&self) -> Option<&str> {
        self.follow.as_ref().and_then(|f| f.ended.as_deref())
    }

    /// Does something to a container: start, stop, restart, pause,
    /// unpause or remove.
    pub fn act(&mut self, id: String, action: &'static str, cx: &mut Context<Self>) {
        let name = self
            .overview
            .as_ref()
            .and_then(|o| o.container(&id))
            .map(|c| c.name.clone())
            .unwrap_or_else(|| id.clone());
        let params = json!({ "id": id, "action": action });
        self.call(id, name, "docker.act", params, cx);
    }

    /// Does something to every container of a Compose project, or takes it
    /// down.
    pub fn project_act(&mut self, name: String, action: &'static str, cx: &mut Context<Self>) {
        let params = json!({ "project": name, "action": action });
        self.call(
            format!("project:{name}"),
            name,
            "docker.project",
            params,
            cx,
        );
    }

    pub fn remove_image(&mut self, id: String, cx: &mut Context<Self>) {
        let name = self
            .overview
            .as_ref()
            .and_then(|o| o.image(&id))
            .map(Image::name)
            .unwrap_or_else(|| id.clone());
        let params = json!({ "id": id });
        self.call(
            format!("image:{id}"),
            name,
            "docker.removeImage",
            params,
            cx,
        );
    }

    fn call(
        &mut self,
        key: String,
        name: String,
        method: &str,
        params: Value,
        cx: &mut Context<Self>,
    ) {
        let Some(code) = self.code(cx) else { return };
        self.busy.insert(key.clone());
        let call = code.call(method, params);
        cx.spawn(async move |this, cx| {
            let got = call.await;
            let _ = this.update(cx, |v, cx| {
                v.busy.remove(&key);
                if let Err(e) = got {
                    v.error = Some(format!("{name}: {e}"));
                }
                v.load(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn open_path(&mut self, path: String, cx: &mut Context<Self>) {
        let root = self
            .workspace
            .read(cx)
            .project_of(&path)
            .map(|f| f.root.clone())
            .unwrap_or_else(|| "/".into());
        cx.emit(OpenFile { root, path });
    }

    fn shell(&mut self, name: String, cx: &mut Context<Self>) {
        cx.emit(RunInTerminal(shell_command(&name)));
    }

    fn toggle_project(&mut self, name: &str, cx: &mut Context<Self>) {
        if !self.closed.remove(name) {
            self.closed.insert(name.to_string());
        }
        cx.notify();
    }

    fn rows(&self) -> Vec<Row> {
        self.overview
            .as_ref()
            .map(|o| rows(o, &self.closed, self.images_open))
            .unwrap_or_default()
    }

    /// The row a choice is on.
    fn chosen_row(&self, rows: &[Row]) -> Option<usize> {
        rows.iter().position(|r| match (r, &self.chosen) {
            (Row::Project { name, .. }, Some(Chosen::Project(n))) => name == n,
            (Row::Container { id, .. }, Some(Chosen::Container(c))) => id == c,
            (Row::Image { id }, Some(Chosen::Image(i))) => id == i,
            _ => false,
        })
    }

    fn choose_row(&mut self, row: &Row, cx: &mut Context<Self>) {
        match row {
            Row::Project { name, .. } => self.choose(Chosen::Project(name.clone()), cx),
            Row::Container { id, .. } => self.choose(Chosen::Container(id.clone()), cx),
            Row::Image { id } => self.choose(Chosen::Image(id.clone()), cx),
            Row::Images { .. } => {}
        }
    }

    /// The list's keys: up and down move, left and right close and open a
    /// project or the images.
    fn key(&mut self, e: &KeyDownEvent, cx: &mut Context<Self>) {
        let rows: Vec<Row> = self
            .rows()
            .into_iter()
            .filter(|r| !matches!(r, Row::Images { .. }))
            .collect();
        let at = self.chosen_row(&rows);
        match e.keystroke.key.as_str() {
            "down" => {
                let next = at.map_or(0, |i| (i + 1).min(rows.len().saturating_sub(1)));
                if let Some(r) = rows.get(next) {
                    self.choose_row(&r.clone(), cx);
                }
            }
            "up" => {
                let prev = at.map_or(0, |i| i.saturating_sub(1));
                if let Some(r) = rows.get(prev) {
                    self.choose_row(&r.clone(), cx);
                }
            }
            "left" | "right" => {
                let open = e.keystroke.key == "right";
                if let Some(Chosen::Project(name)) = self.chosen.clone()
                    && self.closed.contains(&name) == open
                {
                    self.toggle_project(&name, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    fn list(&self, p: &Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        let state = self.workspace.read(cx).state().clone();
        let Some(o) = &self.overview else {
            let text = match &state {
                State::Failed(e) => format!("Could not reach the Code service: {e}"),
                State::Reconnecting(why) => State::reconnecting(why),
                _ => match &self.error {
                    Some(e) => e.clone(),
                    None => "Reading…".into(),
                },
            };
            return note(text, p);
        };
        let mut list = v_flex().gap(px(1.)).p(px(4.));
        if let Some(e) = &self.error {
            list = list.child(
                div()
                    .px(px(6.))
                    .py(px(3.))
                    .text_size(px(11.))
                    .text_color(p.bad)
                    .child(e.clone()),
            );
        }
        if !o.available {
            return list
                .child(note(
                    o.why
                        .clone()
                        .unwrap_or_else(|| "Docker is not running.".into()),
                    p,
                ))
                .into_any_element();
        }
        if o.containers.is_empty() {
            list = list.child(note(
                "No containers. Start one with docker run or docker compose up.",
                p,
            ));
        }
        for (i, row) in self.rows().into_iter().enumerate() {
            list = list.child(self.row(i, row, o, p, cx));
        }
        list.into_any_element()
    }

    fn row(
        &self,
        i: usize,
        row: Row,
        o: &Overview,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let me = cx.entity().downgrade();
        let selected = match (&row, &self.chosen) {
            (Row::Project { name, .. }, Some(Chosen::Project(n))) => name == n,
            (Row::Container { id, .. }, Some(Chosen::Container(c))) => id == c,
            (Row::Image { id }, Some(Chosen::Image(c))) => id == c,
            _ => false,
        };
        let hover = p.surface_2;
        let base = h_flex()
            .id(("docker-row", i))
            .h(px(22.))
            .px(px(6.))
            .gap(px(6.))
            .rounded(px(4.))
            .cursor_pointer()
            .when(selected, |d| d.bg(p.selection))
            .when(!selected, move |d| d.hover(move |s| s.bg(hover)));
        let meta = |text: String| {
            div()
                .min_w_0()
                .truncate()
                .text_size(px(10.5))
                .text_color(p.muted)
                .child(text)
        };
        let icon = |name: &str| {
            svg()
                .path(assets::icon(name))
                .size(px(12.))
                .flex_none()
                .text_color(p.muted)
        };
        match row {
            Row::Project {
                name,
                up,
                count,
                open,
            } => {
                let at = format!("docker-project-{name}");
                let (toggle, choose, right) = (name.clone(), name.clone(), name.clone());
                let list: Vec<Container> = o.project(&name).into_iter().cloned().collect();
                base.debug_selector(move || at)
                    .child(icon(if open {
                        "chevron-down"
                    } else {
                        "chevron-right"
                    }))
                    .child(icon("layers"))
                    .child(
                        div()
                            .flex_none()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(name.clone()),
                    )
                    .child(meta(format!("{up} of {count} running")))
                    .on_click(cx.listener(move |v, e: &gpui::ClickEvent, window, cx| {
                        window.focus(&v.focus, cx);
                        if e.click_count() >= 2 {
                            v.toggle_project(&toggle, cx);
                        } else {
                            v.choose(Chosen::Project(choose.clone()), cx);
                        }
                    }))
                    .on_mouse_down(
                        gpui::MouseButton::Right,
                        cx.listener(move |v, _, _, cx| {
                            v.choose(Chosen::Project(right.clone()), cx)
                        }),
                    )
                    .context_menu(move |menu, _, cx| {
                        crate::ui::modal(cx);
                        project_menu(menu, me.clone(), name.clone(), &list)
                    })
                    .into_any_element()
            }
            Row::Container { id, nested } => {
                let Some(c) = o.container(&id).cloned() else {
                    return div().into_any_element();
                };
                let at = format!("docker-row-{}", c.name);
                let (choose, right) = (id.clone(), id.clone());
                base.debug_selector(move || at)
                    .when(nested, |d| d.pl(px(24.)))
                    .tooltip({
                        let tip = format!("{}\n{}\n{}", c.name, c.image, c.status);
                        move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                        }
                    })
                    .child(dot(c.tone(), p))
                    .child(
                        div()
                            .flex_none()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(c.label(nested)),
                    )
                    .child(meta(c.meta()))
                    .on_click(cx.listener(move |v, e: &gpui::ClickEvent, window, cx| {
                        window.focus(&v.focus, cx);
                        v.choose(Chosen::Container(choose.clone()), cx);
                        if e.click_count() >= 2 {
                            v.show_pane(Pane::Log, cx);
                        }
                    }))
                    .on_mouse_down(
                        gpui::MouseButton::Right,
                        cx.listener(move |v, _, _, cx| {
                            v.choose(Chosen::Container(right.clone()), cx)
                        }),
                    )
                    .context_menu(move |menu, _, cx| {
                        crate::ui::modal(cx);
                        container_menu(menu, me.clone(), &c)
                    })
                    .into_any_element()
            }
            Row::Images { count, size, open } => base
                .debug_selector(|| "docker-images".into())
                .child(icon(if open {
                    "chevron-down"
                } else {
                    "chevron-right"
                }))
                .child(dot(Tone::Idle, p))
                .child(div().flex_none().child("Images"))
                .child(meta(format!("{count} · {}", bytes(size))))
                .on_click(cx.listener(|v, _, _, cx| {
                    v.images_open = !v.images_open;
                    cx.notify();
                }))
                .into_any_element(),
            Row::Image { id } => {
                let Some(img) = o.image(&id).cloned() else {
                    return div().into_any_element();
                };
                let at = format!("docker-image-{}", img.name());
                let (choose, right) = (id.clone(), id.clone());
                base.debug_selector(move || at)
                    .pl(px(24.))
                    .child(div().min_w_0().truncate().child(img.name()))
                    .child(meta(bytes(img.size)))
                    .on_click(cx.listener(move |v, _, window, cx| {
                        window.focus(&v.focus, cx);
                        v.choose(Chosen::Image(choose.clone()), cx)
                    }))
                    .on_mouse_down(
                        gpui::MouseButton::Right,
                        cx.listener(move |v, _, _, cx| v.choose(Chosen::Image(right.clone()), cx)),
                    )
                    .context_menu(move |menu, _, cx| {
                        crate::ui::modal(cx);
                        image_menu(menu, me.clone(), &img)
                    })
                    .into_any_element()
            }
        }
    }

    fn details(&self, p: &Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(o) = self.overview.as_ref().filter(|o| o.available) else {
            return div().into_any_element();
        };
        match self.chosen.clone() {
            None => note("Choose a container to see its log.", p),
            Some(Chosen::Container(id)) => match o.container(&id).cloned() {
                Some(c) => self.container_details(c, p, cx),
                None => div().into_any_element(),
            },
            Some(Chosen::Project(name)) => {
                let list: Vec<Container> = o.project(&name).into_iter().cloned().collect();
                self.project_details(name, list, p, cx)
            }
            Some(Chosen::Image(id)) => match o.image(&id).cloned() {
                Some(img) => self.image_details(img, p, cx),
                None => div().into_any_element(),
            },
        }
    }

    fn container_details(
        &self,
        c: Container,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let busy = self.busy.contains(&c.id);
        let (id, name) = (c.id.clone(), c.name.clone());
        let mut bar = h_flex()
            .h(px(26.))
            .flex_none()
            .px(px(8.))
            .gap(px(4.))
            .border_b_1()
            .border_color(p.border)
            .child(
                chip("docker-log-tab", "Log".into(), self.pane == Pane::Log, p)
                    .debug_selector(|| "docker-log-tab".into())
                    .on_click(cx.listener(|v, _, _, cx| v.show_pane(Pane::Log, cx))),
            )
            .child(
                chip(
                    "docker-inspect-tab",
                    "Inspect".into(),
                    self.pane == Pane::Inspect,
                    p,
                )
                .debug_selector(|| "docker-inspect-tab".into())
                .on_click(cx.listener(|v, _, _, cx| v.show_pane(Pane::Inspect, cx))),
            );
        if self.pane == Pane::Log
            && let Some(why) = self.not_following()
        {
            let again = id.clone();
            bar = bar
                .child(
                    div()
                        .ml(px(6.))
                        .text_size(px(11.))
                        .text_color(p.muted)
                        .truncate()
                        .child(format!("Not following: {why}")),
                )
                .child(
                    div()
                        .id("docker-follow-again")
                        .debug_selector(|| "docker-follow-again".into())
                        .text_size(px(11.))
                        .text_color(p.accent)
                        .cursor_pointer()
                        .child("Follow Again")
                        .on_click(
                            cx.listener(move |v, _, _, cx| v.start_following(again.clone(), cx)),
                        ),
                );
        }
        bar = bar.child(div().flex_1());
        bar = if c.up() {
            let id = id.clone();
            bar.child(tool(
                "docker-stop",
                "stop",
                "Stop",
                !busy,
                p,
                cx.listener(move |v, _, _, cx| v.act(id.clone(), "stop", cx)),
            ))
        } else {
            let id = id.clone();
            bar.child(tool(
                "docker-start",
                "play",
                "Start",
                !busy,
                p,
                cx.listener(move |v, _, _, cx| v.act(id.clone(), "start", cx)),
            ))
        };
        let restart = id.clone();
        let shell = name.clone();
        bar = bar
            .child(tool(
                "docker-restart",
                "refresh",
                "Restart",
                !busy,
                p,
                cx.listener(move |v, _, _, cx| v.act(restart.clone(), "restart", cx)),
            ))
            .child(tool(
                "docker-shell",
                "terminal",
                "Shell",
                c.running(),
                p,
                cx.listener(move |v, _, _, cx| v.shell(shell.clone(), cx)),
            ));
        let body = match self.pane {
            Pane::Log => div()
                .flex_1()
                .min_h_0()
                .child(self.log.clone())
                .into_any_element(),
            Pane::Inspect => {
                let mut grid = grid(p)
                    .child(detail("Status", c.status.clone(), p))
                    .child(detail("Image", c.image.clone(), p))
                    .child(
                        detail_row("ID", p).child(
                            div()
                                .id("docker-copy-id")
                                .debug_selector(|| "docker-copy-id".into())
                                .text_color(p.accent)
                                .cursor_pointer()
                                .child(c.id.chars().take(12).collect::<String>())
                                .tooltip(|window, cx| {
                                    gpui_component::tooltip::Tooltip::new("Copy It")
                                        .build(window, cx)
                                })
                                .on_click(move |_, _, cx| crate::treeops::copy(id.clone(), cx)),
                        ),
                    );
                if !c.command.is_empty() {
                    grid = grid.child(detail("Command", c.command.clone(), p));
                }
                grid = grid
                    .child(detail(
                        "Ports",
                        if c.ports.is_empty() {
                            "none published".into()
                        } else {
                            c.ports.join(", ")
                        },
                        p,
                    ))
                    .child(detail("Created", when(c.created, Local::now()), p));
                if let Some(project) = &c.project {
                    let service = c
                        .service
                        .as_ref()
                        .map(|s| format!(" / {s}"))
                        .unwrap_or_default();
                    grid = grid.child(detail("Compose", format!("{project}{service}"), p));
                }
                grid = grid.when_some(c.config_files.clone(), |g, files| {
                    g.child(self.compose_files(files, p, cx))
                });
                div()
                    .id("docker-inspect")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(grid)
                    .into_any_element()
            }
        };
        v_flex()
            .size_full()
            .min_h_0()
            .child(bar)
            .child(body)
            .into_any_element()
    }

    fn compose_files(&self, files: Vec<String>, p: &Palette, cx: &mut Context<Self>) -> gpui::Div {
        let mut links = v_flex().gap(px(2.));
        for (i, f) in files.iter().enumerate() {
            let path = f.clone();
            links = links.child(
                h_flex()
                    .id(("docker-compose-file", i))
                    .gap(px(4.))
                    .text_color(p.accent)
                    .cursor_pointer()
                    .child(
                        svg()
                            .path(assets::icon("file"))
                            .size(px(12.))
                            .text_color(p.accent),
                    )
                    .child(f.clone())
                    .on_click(cx.listener(move |v, _, _, cx| v.open_path(path.clone(), cx))),
            );
        }
        let label = if files.len() == 1 {
            "Compose file"
        } else {
            "Compose files"
        };
        detail_row(label, p).child(links)
    }

    fn project_details(
        &self,
        name: String,
        list: Vec<Container>,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let busy = self.busy.contains(&format!("project:{name}"));
        let up = list.iter().filter(|c| c.running()).count();
        let dir = list.iter().find_map(|c| c.working_dir.clone());
        let files = list.iter().find_map(|c| c.config_files.clone());
        let mut grid = grid(p);
        if let Some(dir) = dir {
            grid = grid.child(detail("Directory", dir, p));
        }
        if let Some(files) = files {
            grid = grid.child(self.compose_files(files, p, cx));
        }
        grid = grid.child(detail(
            "Services",
            list.iter()
                .map(|c| format!("{} ({})", c.label(true), c.state))
                .collect::<Vec<_>>()
                .join(", "),
            p,
        ));
        let on = |action: &'static str| {
            let name = name.clone();
            cx.listener(move |v: &mut Self, _: &gpui::ClickEvent, _, cx| {
                v.project_act(name.clone(), action, cx)
            })
        };
        let me = cx.entity().downgrade();
        let down = name.clone();
        let actions = h_flex()
            .gap(px(4.))
            .px(px(8.))
            .py(px(6.))
            .child(tool(
                "docker-start-all",
                "play",
                "Start All",
                !busy && up < list.len(),
                p,
                on("start"),
            ))
            .child(tool(
                "docker-stop-all",
                "stop",
                "Stop All",
                !busy && list.iter().any(Container::up),
                p,
                on("stop"),
            ))
            .child(tool(
                "docker-restart-all",
                "refresh",
                "Restart All",
                !busy,
                p,
                on("restart"),
            ))
            .child(tool(
                "docker-down",
                "trash",
                "Down…",
                !busy,
                p,
                move |_, window, cx| confirm_down(me.clone(), down.clone(), window, cx),
            ));
        v_flex()
            .size_full()
            .min_h_0()
            .child(header(
                "layers",
                name.clone(),
                format!("Compose project, {up} of {} running", list.len()),
                p,
            ))
            .child(actions)
            .child(grid)
            .into_any_element()
    }

    fn image_details(&self, img: Image, p: &Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        let busy = self.busy.contains(&format!("image:{}", img.id));
        let me = cx.entity().downgrade();
        let (id, name) = (img.id.clone(), img.name());
        let grid = grid(p)
            .child(detail("ID", img.id.clone(), p))
            .child(detail(
                "Tags",
                if img.tags.is_empty() {
                    "none".into()
                } else {
                    img.tags.join(", ")
                },
                p,
            ))
            .child(detail("Size", bytes(img.size), p))
            .child(detail("Created", when(img.created, Local::now()), p))
            .child(detail(
                "Used by",
                match img.containers as usize {
                    0 => "no container".to_string(),
                    1 => "1 container".to_string(),
                    n => format!("{n} containers"),
                },
                p,
            ));
        let in_use = img.in_use();
        v_flex()
            .size_full()
            .min_h_0()
            .child(header("docker", name.clone(), bytes(img.size), p))
            .child(h_flex().px(px(8.)).py(px(6.)).child(tool(
                "docker-remove-image",
                "trash",
                "Remove…",
                !busy && !in_use,
                p,
                move |_, window, cx| {
                    confirm_remove_image(me.clone(), id.clone(), name.clone(), window, cx)
                },
            )))
            .child(grid)
            .into_any_element()
    }
}

impl Drop for DockerView {
    fn drop(&mut self) {
        // The request goes as it is made; its answer is not waited for.
        if let Some(Follow {
            stream: Some(s),
            code,
            ..
        }) = &self.follow
        {
            drop(code.call("docker.logsStop", json!({ "stream": s })));
        }
    }
}

impl Focusable for DockerView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for DockerView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        h_flex()
            .size_full()
            .min_h_0()
            .text_size(px(12.))
            .child(
                div()
                    .id("docker-tree")
                    .track_focus(&self.focus)
                    .on_key_down(cx.listener(|v, e: &KeyDownEvent, _, cx| v.key(e, cx)))
                    .w(px(240.))
                    .flex_none()
                    .h_full()
                    .overflow_y_scroll()
                    .border_r_1()
                    .border_color(p.border)
                    .child(self.list(&p, cx)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.details(&p, cx)),
            )
    }
}

/// What a menu item does to the view.
type Act = Box<dyn Fn(&mut DockerView, &mut Window, &mut Context<DockerView>)>;

fn container_menu(menu: PopupMenu, view: WeakEntity<DockerView>, c: &Container) -> PopupMenu {
    let on = |f: Act| {
        let view = view.clone();
        move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
            let _ = view.update(cx, |v, cx| f(v, window, cx));
        }
    };
    let act = |action: &'static str| {
        let id = c.id.clone();
        on(Box::new(move |v, _, cx| v.act(id.clone(), action, cx)))
    };
    let (id, name) = (c.id.clone(), c.name.clone());
    let logs = id.clone();
    let shell = name.clone();
    let (copy_name, copy_id) = (name.clone(), id.clone());
    let remove = {
        let view = view.clone();
        let (id, name, running) = (id.clone(), name.clone(), c.running());
        move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
            confirm_remove(view.clone(), id.clone(), name.clone(), running, window, cx)
        }
    };
    let menu = menu
        .item(
            PopupMenuItem::new("Show Log").on_click(on(Box::new(move |v, _, cx| {
                v.choose(Chosen::Container(logs.clone()), cx);
                v.show_pane(Pane::Log, cx);
            }))),
        )
        .item(
            PopupMenuItem::new("Open Shell")
                .disabled(!c.running())
                .on_click(on(Box::new(move |v, _, cx| v.shell(shell.clone(), cx)))),
        )
        .separator();
    let menu = if c.up() {
        menu.item(PopupMenuItem::new("Stop").on_click(act("stop")))
    } else {
        menu.item(PopupMenuItem::new("Start").on_click(act("start")))
    };
    let menu = menu.item(PopupMenuItem::new("Restart").on_click(act("restart")));
    let menu = if c.state == "paused" {
        menu.item(PopupMenuItem::new("Unpause").on_click(act("unpause")))
    } else {
        menu.item(
            PopupMenuItem::new("Pause")
                .disabled(!c.running())
                .on_click(act("pause")),
        )
    };
    menu.separator()
        .item(
            PopupMenuItem::new("Copy Name")
                .on_click(move |_, _, cx| crate::treeops::copy(copy_name.clone(), cx)),
        )
        .item(
            PopupMenuItem::new("Copy ID")
                .on_click(move |_, _, cx| crate::treeops::copy(copy_id.clone(), cx)),
        )
        .separator()
        .item(
            PopupMenuItem::new(if c.running() {
                "Stop and Remove…"
            } else {
                "Remove…"
            })
            .on_click(remove),
        )
}

fn project_menu(
    menu: PopupMenu,
    view: WeakEntity<DockerView>,
    name: String,
    list: &[Container],
) -> PopupMenu {
    let act = |action: &'static str| {
        let (view, name) = (view.clone(), name.clone());
        move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
            let _ = view.update(cx, |v, cx| v.project_act(name.clone(), action, cx));
        }
    };
    let files = list
        .iter()
        .find_map(|c| c.config_files.clone())
        .unwrap_or_default();
    let dir = list.iter().find_map(|c| c.working_dir.clone());
    let mut menu = menu
        .item(
            PopupMenuItem::new("Start All")
                .disabled(list.iter().all(Container::running))
                .on_click(act("start")),
        )
        .item(
            PopupMenuItem::new("Stop All")
                .disabled(!list.iter().any(Container::up))
                .on_click(act("stop")),
        )
        .item(PopupMenuItem::new("Restart All").on_click(act("restart")))
        .separator();
    for f in files {
        let file = f.rsplit('/').next().unwrap_or(&f).to_string();
        let view = view.clone();
        menu = menu.item(
            PopupMenuItem::new(format!("Open {file}")).on_click(move |_, _, cx| {
                let _ = view.update(cx, |v, cx| v.open_path(f.clone(), cx));
            }),
        );
    }
    let down = {
        let (view, name) = (view.clone(), name.clone());
        move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
            confirm_down(view.clone(), name.clone(), window, cx)
        }
    };
    menu.item(
        PopupMenuItem::new("Copy Working Directory")
            .disabled(dir.is_none())
            .on_click(move |_, _, cx| {
                if let Some(d) = &dir {
                    crate::treeops::copy(d.clone(), cx)
                }
            }),
    )
    .separator()
    .item(PopupMenuItem::new("Down…").on_click(down))
}

fn image_menu(menu: PopupMenu, view: WeakEntity<DockerView>, img: &Image) -> PopupMenu {
    let copy = img.id.clone();
    let (id, name) = (img.id.clone(), img.name());
    menu.item(
        PopupMenuItem::new("Copy ID")
            .on_click(move |_, _, cx| crate::treeops::copy(copy.clone(), cx)),
    )
    .separator()
    .item(
        PopupMenuItem::new("Remove…")
            .disabled(img.in_use())
            .on_click(move |_, window, cx| {
                confirm_remove_image(view.clone(), id.clone(), name.clone(), window, cx)
            }),
    )
}

fn confirm_remove(
    view: WeakEntity<DockerView>,
    id: String,
    name: String,
    running: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let (title, ok) = if running {
        (format!("Stop and remove {name}?"), "Stop and Remove")
    } else {
        (format!("Remove {name}?"), "Remove")
    };
    crate::treeops::confirm(
        &title,
        "The container is deleted, with what it wrote outside its volumes.",
        ok,
        window,
        cx,
        move |_, cx| {
            let _ = view.update(cx, |v, cx| v.act(id.clone(), "remove", cx));
        },
    );
}

fn confirm_down(view: WeakEntity<DockerView>, name: String, window: &mut Window, cx: &mut App) {
    crate::treeops::confirm(
        &format!("Take {name} down?"),
        "Its containers and networks are removed, as docker compose down does. Its volumes are kept.",
        "Down",
        window,
        cx,
        move |_, cx| {
            let _ = view.update(cx, |v, cx| v.project_act(name.clone(), "down", cx));
        },
    );
}

fn confirm_remove_image(
    view: WeakEntity<DockerView>,
    id: String,
    name: String,
    window: &mut Window,
    cx: &mut App,
) {
    crate::treeops::confirm(
        &format!("Remove {name}?"),
        "The image is deleted; pulling or building it again brings it back.",
        "Remove",
        window,
        cx,
        move |_, cx| {
            let _ = view.update(cx, |v, cx| v.remove_image(id.clone(), cx));
        },
    );
}

/// A button of the details' bar: an icon and a word, greyed when it cannot
/// be used.
fn tool(
    id: &'static str,
    icon: &str,
    label: &'static str,
    enabled: bool,
    p: &Palette,
    on: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    let hover = p.surface_2;
    let colour = if enabled { p.text } else { p.muted };
    h_flex()
        .id(id)
        .debug_selector(move || id.to_string())
        .h(px(20.))
        .px(px(6.))
        .gap(px(4.))
        .rounded(px(4.))
        .text_size(px(11.))
        .text_color(colour)
        .when(!enabled, |d| d.opacity(0.5))
        .when(enabled, move |d| {
            d.cursor_pointer().hover(move |s| s.bg(hover)).on_click(on)
        })
        .child(
            svg()
                .path(assets::icon(icon))
                .size(px(12.))
                .text_color(colour),
        )
        .child(label)
}

fn header(icon: &str, name: String, about: String, p: &Palette) -> gpui::Div {
    h_flex()
        .h(px(26.))
        .flex_none()
        .px(px(8.))
        .gap(px(6.))
        .border_b_1()
        .border_color(p.border)
        .child(
            svg()
                .path(assets::icon(icon))
                .size(px(13.))
                .text_color(p.muted),
        )
        .child(div().font_weight(FontWeight::SEMIBOLD).child(name))
        .child(div().text_size(px(11.)).text_color(p.muted).child(about))
}

fn grid(_: &Palette) -> gpui::Div {
    v_flex().gap(px(4.)).px(px(10.)).py(px(8.))
}

fn detail_row(label: &str, p: &Palette) -> gpui::Div {
    h_flex().items_start().gap(px(10.)).child(
        div()
            .w(px(110.))
            .flex_none()
            .text_color(p.muted)
            .child(label.to_string()),
    )
}

fn detail(label: &str, value: String, p: &Palette) -> gpui::Div {
    detail_row(label, p).child(div().flex_1().min_w_0().child(value))
}

fn note(text: impl Into<SharedString>, p: &Palette) -> gpui::AnyElement {
    div()
        .p(px(8.))
        .text_size(px(11.5))
        .text_color(p.muted)
        .child(text.into())
        .into_any_element()
}
