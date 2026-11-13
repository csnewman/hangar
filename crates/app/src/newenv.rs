//! New environment (⌘N, or + New environment), as the design's sheet: a
//! template, the place it runs in, its name and branch, and its machine if
//! changed from the template's; then Create and Open, which opens its tab.

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement,
    ParentElement, Render, SharedString, Styled, Window, div, prelude::*, px, relative,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Sizable, h_flex, v_flex};

use crate::api::{Environment, MachineSettings, NewEnvironment, Template};
use crate::envsettings::{memory, parse_gib};
use crate::palette::{self, Palette};
use crate::place::{Connection, Update};
use crate::places::Places;
use crate::ui::{button, disabled, primary};

/// What is wrong with a name an environment cannot have, or None. It
/// becomes its hostname, so it is a DNS label: letters, digits and hyphens,
/// at most 63, neither starting nor ending with a hyphen. A template's own
/// pattern the server checks.
pub fn name_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return None;
    }
    if name.len() > 63 {
        return Some("At most 63 characters");
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Some("Letters, digits and hyphens only");
    }
    if name.starts_with('-') || name.ends_with('-') {
        return Some("Not starting or ending with a hyphen");
    }
    None
}

/// A template's text with the variables known before the environment is
/// made filled in: {name}, {owner} and {template}, each also :lower and
/// :upper. Others, such as {short_id}, are left for the server.
pub fn fill(text: &str, name: &str, owner: &str, template: &str) -> String {
    let mut out = text.to_string();
    for (var, value) in [("name", name), ("owner", owner), ("template", template)] {
        out = out
            .replace(&format!("{{{var}}}"), value)
            .replace(&format!("{{{var}:lower}}"), &value.to_lowercase())
            .replace(&format!("{{{var}:upper}}"), &value.to_uppercase());
    }
    out
}

/// A template's machine in a line, as the web lists it.
pub fn machine_line(t: &Template) -> String {
    let mut out = format!(
        "{} vCPU{} · {}",
        t.cpus,
        if t.cpus == 1 { "" } else { "s" },
        memory(t.memory_mib)
    );
    if t.display == "desktop" {
        out.push_str(" · desktop");
    }
    if t.gpu != "none" && !t.gpu.is_empty() {
        out.push_str(&format!(" · GPU {}", t.gpu));
    }
    out
}

pub enum NewEnvEvent {
    /// Cancel or Escape.
    Close,
    /// It was made: its tab is to open.
    Made { place: String, env: String },
}

gpui::actions!(new_environment, [Cancel]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([gpui::KeyBinding::new(
        "escape",
        Cancel,
        Some("NewEnvironment"),
    )]);
}

/// The machine, changed from the template's.
struct Machine {
    cpus: Entity<InputState>,
    memory: Entity<InputState>,
    display: String,
    gpu: String,
}

pub struct NewEnvView {
    places: Entity<Places>,
    /// Where it is to run, and that place's templates once read.
    place: Option<String>,
    templates: Option<Result<Vec<Template>, String>>,
    template: Option<String>,
    name: Entity<InputState>,
    branch: Entity<InputState>,
    /// The branch as last filled in from the name; typed over, it is the
    /// person's.
    filled: String,
    machine: Option<Machine>,
    busy: bool,
    error: Option<String>,
    focus: FocusHandle,
    _subs: Vec<gpui::Subscription>,
}

impl EventEmitter<NewEnvEvent> for NewEnvView {}

impl Focusable for NewEnvView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.name.focus_handle(cx)
    }
}

impl NewEnvView {
    /// The sheet, for a place, else the first one online.
    pub fn new(
        places: Entity<Places>,
        place: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("rate-limits"));
        let branch = cx.new(|cx| InputState::new(window, cx));
        let subs = vec![
            cx.subscribe_in(&name, window, |this, _, e: &InputEvent, window, cx| {
                if matches!(e, InputEvent::Change) {
                    this.error = None;
                    this.fill_branch(window, cx);
                    cx.notify();
                }
            }),
            cx.subscribe(&branch, |_, _, e: &InputEvent, cx| {
                if matches!(e, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.observe(&places, |_, _, cx| cx.notify()),
        ];
        let online = |id: &str, cx: &App| {
            places
                .read(cx)
                .get(id)
                .is_some_and(|(_, s)| s.connection == Connection::Online)
        };
        let place = place.filter(|p| online(p, cx)).or_else(|| {
            places
                .read(cx)
                .list()
                .iter()
                .find(|(_, s)| s.connection == Connection::Online)
                .map(|(c, _)| c.id.clone())
        });
        let mut view = NewEnvView {
            places,
            place: None,
            templates: None,
            template: None,
            name,
            branch,
            filled: String::new(),
            machine: None,
            busy: false,
            error: None,
            focus: cx.focus_handle(),
            _subs: subs,
        };
        if let Some(p) = place {
            view.choose_place(p, window, cx);
        }
        view
    }

    /// Where it is to run: reads that place's templates.
    pub fn choose_place(&mut self, place: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.place.as_deref() == Some(&place) {
            return;
        }
        let Some(client) = self.places.read(cx).client(&place) else {
            return;
        };
        self.place = Some(place);
        self.templates = None;
        self.template = None;
        self.machine = None;
        self.error = None;
        let task = cx.background_spawn(async move { client.templates() });
        cx.spawn_in(window, async move |this, cx| {
            let got = task.await;
            let _ = this.update_in(cx, |v, window, cx| {
                v.templates = Some(got.map_err(|e| e.to_string()));
                if let Some(t) = v.default_template(cx) {
                    v.choose_template(t, window, cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// The template of the user's newest environment at the place, else
    /// the first.
    fn default_template(&self, cx: &App) -> Option<String> {
        let list = self.templates.as_ref()?.as_ref().ok()?;
        let place = self.place.as_ref()?;
        let (_, state) = self.places.read(cx).get(place)?;
        let me = state.me.as_ref().map(|m| m.username.clone());
        let newest = state
            .environments
            .iter()
            .filter(|e| Some(&e.owner) == me.as_ref())
            .filter_map(|e| e.details.template_id.clone().map(|t| (e.created_at, t)))
            .max_by_key(|(at, _)| *at)
            .map(|(_, t)| t);
        newest
            .filter(|t| list.iter().any(|x| &x.id == t))
            .or_else(|| sorted(list, me.as_deref()).first().map(|t| t.id.clone()))
    }

    pub fn choose_template(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.template = Some(id);
        self.machine = None;
        self.filled = String::new();
        self.branch
            .update(cx, |s, cx| s.set_value(String::new(), window, cx));
        self.fill_branch(window, cx);
        cx.notify();
    }

    fn chosen(&self) -> Option<&Template> {
        let list = self.templates.as_ref()?.as_ref().ok()?;
        let id = self.template.as_ref()?;
        list.iter().find(|t| &t.id == id)
    }

    fn owner(&self, cx: &App) -> String {
        self.place
            .as_ref()
            .and_then(|p| self.places.read(cx).get(p))
            .and_then(|(_, s)| s.me.as_ref())
            .map(|m| m.username.clone())
            .unwrap_or_default()
    }

    /// Fills the branch in from the name, unless it has been typed over.
    fn fill_branch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.chosen() else {
            return;
        };
        let Some(pattern) = t.repos.first().and_then(|r| r.branch.clone()) else {
            return;
        };
        let typed = self.branch.read(cx).value().to_string();
        if !typed.is_empty() && typed != self.filled {
            return;
        }
        let name = self.name.read(cx).value().to_string();
        let filled = fill(&pattern, &name, &self.owner(cx), &t.name.clone());
        self.filled = filled.clone();
        self.branch
            .update(cx, |s, cx| s.set_value(filled, window, cx));
    }

    /// Changes the machine from the template's, or back to it.
    fn change_machine(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.machine = None;
        if on && let Some(t) = self.chosen().cloned() {
            let input = |value: String, window: &mut Window, cx: &mut Context<Self>| {
                cx.new(|cx| {
                    let mut s = InputState::new(window, cx);
                    s.set_value(value, window, cx);
                    s
                })
            };
            let gib = t.memory_mib as f64 / 1024.;
            let cpus = input(t.cpus.to_string(), window, cx);
            let memory = input(
                if gib.fract() == 0. {
                    format!("{gib:.0}")
                } else {
                    format!("{gib:.1}")
                },
                window,
                cx,
            );
            for i in [&cpus, &memory] {
                self._subs.push(cx.subscribe(i, |_, _, e: &InputEvent, cx| {
                    if matches!(e, InputEvent::Change) {
                        cx.notify();
                    }
                }));
            }
            self.machine = Some(Machine {
                cpus,
                memory,
                display: t.display.clone(),
                gpu: t.gpu.clone(),
            });
        }
        cx.notify();
    }

    /// The environment to make, or why it cannot be made yet.
    pub fn request(&self, cx: &App) -> Result<NewEnvironment, String> {
        let t = self.chosen().ok_or("Choose a template")?;
        let name = self.name.read(cx).value().trim().to_string();
        if name.is_empty() {
            return Err("Give it a name".into());
        }
        if let Some(p) = name_problem(&name) {
            return Err(p.into());
        }
        let settings = match &self.machine {
            None => None,
            Some(m) => Some(MachineSettings {
                cpus: m
                    .cpus
                    .read(cx)
                    .value()
                    .trim()
                    .parse()
                    .ok()
                    .filter(|n: &i64| *n >= 1)
                    .ok_or("vCPUs is a whole number, 1 or more")?,
                memory_mib: parse_gib(&m.memory.read(cx).value())
                    .ok_or("Memory is a number of GB, such as 4 or 1.5")?,
                display: m.display.clone(),
                gpu: m.gpu.clone(),
                dax: t.dax,
                swap_mib: t.swap_mib,
                memory_reclaim: t.memory_reclaim,
            }),
        };
        // A branch typed over is sent with the template's repositories; one
        // filled in from the name the server fills in itself.
        let typed = self.branch.read(cx).value().trim().to_string();
        let repos = (!t.repos.is_empty() && typed != self.filled.trim()).then(|| {
            let mut repos = t.repos.clone();
            repos[0].branch = Some(typed).filter(|b| !b.is_empty());
            repos
        });
        Ok(NewEnvironment {
            template_id: t.id.clone(),
            name,
            settings,
            repos,
        })
    }

    /// Makes it, then has its tab opened.
    pub fn create(&mut self, cx: &mut Context<Self>) {
        let request = match self.request(cx) {
            Ok(r) => r,
            Err(e) => {
                self.error = Some(e);
                cx.notify();
                return;
            }
        };
        let Some(place) = self.place.clone() else {
            return;
        };
        let Some(client) = self.places.read(cx).client(&place) else {
            return;
        };
        self.busy = true;
        self.error = None;
        cx.notify();
        let task = cx.background_spawn(async move { client.create_environment(&request) });
        cx.spawn(async move |this, cx| {
            let got: Result<Environment, _> = task.await;
            let _ = this.update(cx, |v, cx| {
                v.busy = false;
                match got {
                    Ok(e) => {
                        let id = e.id.clone();
                        v.places.update(cx, |p, cx| {
                            p.apply(&place, Update::Environment(Box::new(e)), cx)
                        });
                        cx.emit(NewEnvEvent::Made { place, env: id });
                    }
                    Err(e) => v.error = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn place(&self) -> Option<&str> {
        self.place.as_deref()
    }

    pub fn template(&self) -> Option<&str> {
        self.template.as_deref()
    }
}

/// The templates as listed: the user's own first, then their teams', then
/// others', each by name.
fn sorted<'a>(list: &'a [Template], me: Option<&str>) -> Vec<&'a Template> {
    let mut out: Vec<&Template> = list.iter().collect();
    out.sort_by_key(|t| {
        let rank = if t.team.is_none() && Some(t.owner.as_str()) == me {
            0
        } else if t.team.is_some() {
            1
        } else {
            2
        };
        (rank, t.name.to_lowercase())
    });
    out
}

impl Render for NewEnvView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let ready = self.request(cx).is_ok() && !self.busy;
        let name = self.name.read(cx).value().to_string();
        let problem = name_problem(name.trim());
        let hint = self.chosen().and_then(|t| t.name_hint.clone());
        let body = v_flex()
            .gap(px(10.))
            .child(field("Where", &p).child(self.places_list(&p, cx)))
            .child(field("Template", &p).child(self.templates_list(&p, window, cx)))
            .child(
                field("Name", &p).child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(px(3.))
                        .child(Input::new(&self.name).small())
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(if problem.is_some() { p.bad } else { p.muted })
                                .child(problem.map(str::to_string).or(hint).unwrap_or_else(|| {
                                    "It becomes its hostname, in lower case.".into()
                                })),
                        ),
                ),
            )
            .when_some(self.chosen().cloned(), |d, t| {
                let d = match t.repos.first() {
                    Some(r) if r.branch.is_some() => d.child(
                        field("Branch", &p).child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap(px(3.))
                                .child(Input::new(&self.branch).small())
                                .child(div().text_size(px(11.)).text_color(p.muted).child(
                                    format!(
                                        "Made in {} from {}",
                                        repo_name(&r.url),
                                        r.r#ref
                                            .clone()
                                            .unwrap_or_else(|| "its default branch".into())
                                    ),
                                )),
                        ),
                    ),
                    Some(r) => d.child(field("Repository", &p).child(
                        div().text_color(p.muted).child(format!(
                            "{} into {}",
                            repo_name(&r.url),
                            r.path
                        )),
                    )),
                    None => d,
                };
                d.child(field("Machine", &p).child(self.machine_part(&t, &p, cx)))
                    .child(
                        field("Image", &p).child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(p.muted)
                                .font_family(crate::diff::mono())
                                .text_size(px(11.))
                                .child(t.image.clone()),
                        ),
                    )
            });
        v_flex()
            .id("new-env-sheet")
            .debug_selector(|| "new-env-sheet".into())
            .track_focus(&self.focus)
            .key_context("NewEnvironment")
            // Its clicks are its own: the backdrop behind it, which closes
            // it, takes only those outside it.
            .occlude()
            .on_action(cx.listener(|_, _: &Cancel, _, cx| cx.emit(NewEnvEvent::Close)))
            .w(px(520.))
            .max_h(window.viewport_size().height - px(80.))
            .bg(p.surface)
            .border_1()
            .border_color(p.border_strong)
            .rounded(px(10.))
            .shadow_lg()
            .overflow_hidden()
            .text_size(px(12.5))
            .line_height(relative(1.45))
            .child(
                div()
                    .px(px(16.))
                    .py(px(12.))
                    .border_b_1()
                    .border_color(p.border)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("New environment"),
            )
            .child(
                div()
                    .id("new-env-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(16.))
                    .py(px(12.))
                    .child(body),
            )
            .child(
                h_flex()
                    .px(px(16.))
                    .py(px(10.))
                    .gap(px(8.))
                    .border_t_1()
                    .border_color(p.border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(11.5))
                            .text_color(p.bad)
                            .when_some(self.error.clone(), |d, e| d.child(e)),
                    )
                    .child(
                        button("new-env-cancel", "Cancel", &p)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(NewEnvEvent::Close))),
                    )
                    .child(if ready {
                        primary("new-env-create", "Create and Open", &p)
                            .debug_selector(|| "new-env-create".into())
                            .on_click(cx.listener(|this, _, _, cx| this.create(cx)))
                    } else {
                        disabled(
                            "new-env-create",
                            if self.busy {
                                "Creating…"
                            } else {
                                "Create and Open"
                            },
                            &p,
                        )
                    }),
            )
    }
}

impl NewEnvView {
    fn places_list(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let list: Vec<(String, String, Connection, usize)> = self
            .places
            .read(cx)
            .list()
            .iter()
            .map(|(c, s)| {
                (
                    c.id.clone(),
                    c.name.clone(),
                    s.connection.clone(),
                    s.environments.len(),
                )
            })
            .collect();
        let mut opts = v_flex().flex_1().min_w_0().gap(px(4.));
        for (i, (id, name, connection, count)) in list.into_iter().enumerate() {
            let online = connection == Connection::Online;
            let on = self.place.as_deref() == Some(&id);
            let (tone, note) = match &connection {
                Connection::Online => (
                    p.good,
                    format!(
                        "server · {count} environment{}",
                        if count == 1 { "" } else { "s" }
                    ),
                ),
                Connection::SignIn => (p.busy, "sign in first".into()),
                Connection::Connecting => (p.idle, "connecting…".into()),
                _ => (p.bad, "can't be reached".into()),
            };
            opts = opts.child(
                opt(
                    SharedString::from(format!("new-env-place-{i}")),
                    on,
                    !online,
                    p,
                )
                .child(div().size(px(8.)).rounded_full().bg(tone))
                .child(div().child(name))
                .child(div().flex_1())
                .child(div().text_size(px(11.)).text_color(p.muted).child(note))
                .when(online, |d| {
                    d.on_click(cx.listener(move |this, _, window, cx| {
                        this.choose_place(id.clone(), window, cx)
                    }))
                }),
            );
        }
        opts
    }

    fn templates_list(
        &self,
        p: &Palette,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let list = match &self.templates {
            None if self.place.is_none() => {
                return note("No place is online to make it in.", p).into_any_element();
            }
            None => return note("Reading its templates…", p).into_any_element(),
            Some(Err(e)) => return div().text_color(p.bad).child(e.clone()).into_any_element(),
            Some(Ok(list)) if list.is_empty() => {
                return note(
                    "There are no templates here yet: make one in the browser first.",
                    p,
                )
                .into_any_element();
            }
            Some(Ok(list)) => list.clone(),
        };
        let me = self.owner(cx);
        let mut opts = v_flex().gap(px(4.));
        for (i, t) in sorted(&list, Some(&me)).into_iter().enumerate() {
            let on = self.template.as_deref() == Some(&t.id);
            let whose = match (&t.team, t.owner == me) {
                (Some(team), _) => team.clone(),
                (None, true) => "yours".into(),
                (None, false) => t.owner.clone(),
            };
            let id = t.id.clone();
            opts = opts.child(
                opt(
                    SharedString::from(format!("new-env-template-{i}")),
                    on,
                    false,
                    p,
                )
                .child(
                    v_flex()
                        .min_w_0()
                        .flex_1()
                        .child(div().truncate().child(t.name.clone()))
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(p.muted)
                                .truncate()
                                .child(machine_line(t)),
                        ),
                )
                .child(div().text_size(px(11.)).text_color(p.muted).child(whose))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.choose_template(id.clone(), window, cx)
                })),
            );
        }
        div()
            .id("new-env-templates")
            .flex_1()
            .min_w_0()
            .max_h(px(220.))
            .overflow_y_scroll()
            .child(opts)
            .into_any_element()
    }

    fn machine_part(&self, t: &Template, p: &Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(m) = &self.machine else {
            return h_flex()
                .gap(px(8.))
                .child(machine_line(t))
                .child(
                    div()
                        .id("new-env-machine-change")
                        .debug_selector(|| "new-env-machine-change".into())
                        .text_color(p.accent)
                        .cursor_pointer()
                        .hover(|s| s.underline())
                        .child("Change…")
                        .on_click(
                            cx.listener(|this, _, window, cx| {
                                this.change_machine(true, window, cx)
                            }),
                        ),
                )
                .into_any_element();
        };
        let seg = |id: &'static str, label: &'static str, on: bool| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .px(px(10.))
                .py(px(2.))
                .cursor_pointer()
                .when(on, |d| d.bg(p.accent).text_color(p.on_accent))
                .when(!on, |d| d.hover(|s| s.bg(p.surface_2)))
                .child(label)
        };
        let group = || {
            h_flex()
                .rounded(px(6.))
                .border_1()
                .border_color(p.border_strong)
                .overflow_hidden()
        };
        let display = m.display.clone();
        let gpu = m.gpu.clone();
        let passthrough = t.gpu == "passthrough";
        v_flex()
            .gap(px(6.))
            .child(
                h_flex()
                    .gap(px(6.))
                    .child(div().w(px(46.)).child(Input::new(&m.cpus).small()))
                    .child(div().text_color(p.muted).child("vCPUs"))
                    .child(div().w(px(10.)))
                    .child(div().w(px(56.)).child(Input::new(&m.memory).small()))
                    .child(div().text_color(p.muted).child("GB")),
            )
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(
                        group()
                            .child(
                                seg("new-env-desktop", "Desktop", display == "desktop").on_click(
                                    cx.listener(|this, _, _, cx| {
                                        if let Some(m) = &mut this.machine {
                                            m.display = "desktop".into();
                                        }
                                        cx.notify();
                                    }),
                                ),
                            )
                            .child(
                                seg("new-env-headless", "Headless", display == "none").on_click(
                                    cx.listener(|this, _, _, cx| {
                                        if let Some(m) = &mut this.machine {
                                            m.display = "none".into();
                                        }
                                        cx.notify();
                                    }),
                                ),
                            ),
                    )
                    .child(
                        group()
                            .child(seg("new-env-gpu-none", "No GPU", gpu == "none").on_click(
                                cx.listener(|this, _, _, cx| {
                                    if let Some(m) = &mut this.machine {
                                        m.gpu = "none".into();
                                    }
                                    cx.notify();
                                }),
                            ))
                            .child(
                                seg("new-env-gpu-virtual", "Virtual GPU", gpu == "virtual")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if let Some(m) = &mut this.machine {
                                            m.gpu = "virtual".into();
                                        }
                                        cx.notify();
                                    })),
                            )
                            .when(passthrough, |d| {
                                d.child(
                                    seg("new-env-gpu-pass", "Passthrough", gpu == "passthrough")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            if let Some(m) = &mut this.machine {
                                                m.gpu = "passthrough".into();
                                            }
                                            cx.notify();
                                        })),
                                )
                            }),
                    ),
            )
            .child(
                div()
                    .id("new-env-machine-reset")
                    .text_color(p.accent)
                    .cursor_pointer()
                    .hover(|s| s.underline())
                    .child("Use the template's")
                    .on_click(
                        cx.listener(|this, _, window, cx| this.change_machine(false, window, cx)),
                    ),
            )
            .into_any_element()
    }
}

/// A repository's name from its address: owner/name, without .git.
fn repo_name(url: &str) -> String {
    let path = url
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .rsplit(['/', ':'])
        .take(2)
        .collect::<Vec<_>>();
    path.into_iter().rev().collect::<Vec<_>>().join("/")
}

/// A choice in one of the sheet's lists.
fn opt(id: SharedString, on: bool, off: bool, p: &Palette) -> gpui::Stateful<gpui::Div> {
    let selector = id.to_string();
    h_flex()
        .id(id)
        .debug_selector(move || selector.clone())
        .gap(px(8.))
        .px(px(9.))
        .py(px(5.))
        .rounded(px(6.))
        .border_1()
        .border_color(if on { p.accent } else { p.border })
        .when(on, |d| d.bg(p.accent.opacity(0.12)))
        .when(!off && !on, |d| {
            d.cursor_pointer().hover(|s| s.bg(p.surface_2))
        })
        .when(off, |d| d.opacity(0.6))
}

/// A field's label, beside what sets it.
fn field(label: &str, p: &Palette) -> gpui::Div {
    h_flex().items_start().gap(px(10.)).child(
        div()
            .w(px(82.))
            .flex_none()
            .pt(px(4.))
            .text_color(p.muted)
            .child(label.to_string()),
    )
}

fn note(text: &str, p: &Palette) -> gpui::Div {
    div()
        .text_size(px(11.5))
        .text_color(p.muted)
        .child(text.to_string())
}
