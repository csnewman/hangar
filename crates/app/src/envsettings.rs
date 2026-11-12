//! Environment Settings (⌘;), as IntelliJ's Project Structure is for a
//! project: one window over the app for one environment, its pages in a
//! searchable tree -- General, Resources, Files, Access, Ports and the
//! Danger zone -- with Cancel, Apply and OK. What is changed is kept until
//! Apply or OK. Its image's upgrade and rollback, resetting it to its
//! template and deleting it are actions, asked first and done at once.

use std::collections::HashMap;
use std::time::Duration;

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement,
    ParentElement, Render, SharedString, Styled, Window, div, prelude::*, px, relative,
};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::slider::{Slider, SliderEvent, SliderState};
use gpui_component::{Disableable as _, Sizable, h_flex, v_flex};

use crate::api::{Client, Environment, EnvironmentPack, MachineSettings, Resources, Stats};
use crate::model::Action;
use crate::palette::{self, Palette};
use crate::place::Update;
use crate::places::Places;
use crate::ui::{button, disabled, primary};

/// A page of an environment's settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    General,
    Resources,
    Files,
    Access,
    Ports,
    Danger,
}

impl Page {
    pub const ALL: [Page; 6] = [
        Page::General,
        Page::Resources,
        Page::Files,
        Page::Access,
        Page::Ports,
        Page::Danger,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Page::General => "General",
            Page::Resources => "Resources",
            Page::Files => "Files",
            Page::Access => "Access",
            Page::Ports => "Ports",
            Page::Danger => "Danger zone",
        }
    }

    /// What a search finds the page by, beside its title.
    fn words(self) -> &'static str {
        match self {
            Page::General => "name task template image upgrade rollback",
            Page::Resources => "memory ram vcpus cpus swap display desktop gpu dax placement",
            Page::Files => "packs copies shared files profile",
            Page::Access => "sensitive credentials ssh keys registry trust",
            Page::Ports => "public private web servers",
            Page::Danger => "reset template delete",
        }
    }

    pub fn matches(self, q: &str) -> bool {
        let q = q.trim().to_lowercase();
        q.is_empty()
            || self.title().to_lowercase().contains(&q)
            || self.words().split(' ').any(|w| w.starts_with(&q))
    }
}

/// The room a running environment's memory keeps above what its programs
/// use when it shrinks, as the server holds it to.
const SHRINK_HEADROOM_MIB: i64 = 512;

/// Whether an environment's machine can be changed: one stopped, or one
/// not yet placed. Its memory alone changes as it runs (`live_memory`).
pub fn changeable(env: &Environment) -> bool {
    if env.phase == "suspended" || env.desired == "suspended" || env.phase == "stopping" {
        return false;
    }
    env.desired == "stopped" || (env.worker.is_none() && env.phase == "pending")
}

/// What a running environment's memory can be resized between as it runs,
/// or None when it cannot be: one stopped, or one whose machine started with
/// no room to grow. It cannot shrink below what its programs use plus
/// 512 MiB.
pub fn live_memory(env: &Environment, stats: Option<&Stats>) -> Option<(i64, i64)> {
    let s = stats?;
    if env.desired != "running" || env.phase != "running" || env.worker_online != Some(true) {
        return None;
    }
    let max = s.machine_memory_max_mib?;
    let memory = env.memory_mib.unwrap_or(max);
    let used = (s.memory_used_mib + SHRINK_HEADROOM_MIB).min(memory);
    Some((s.machine_memory_min_mib.unwrap_or(0).max(used), max))
}

/// An environment's machine as its settings say it is.
pub fn saved(env: &Environment) -> MachineSettings {
    MachineSettings {
        cpus: env.cpus.unwrap_or(1),
        memory_mib: env.memory_mib.unwrap_or(1024),
        display: env.display.clone().unwrap_or_else(|| "none".into()),
        gpu: env.details.gpu.clone().unwrap_or_else(|| "none".into()),
        dax: env.details.dax,
        swap_mib: env.details.swap_mib,
    }
}

/// What differs between a machine and how it is to be, a phrase each, as
/// the web's Apply bar says it ("memory 2 GB → 4 GB").
pub fn changes(from: &MachineSettings, to: &MachineSettings) -> Vec<String> {
    let mut out = vec![];
    if from.cpus != to.cpus {
        out.push(format!("vCPUs {} → {}", from.cpus, to.cpus));
    }
    if from.memory_mib != to.memory_mib {
        out.push(format!(
            "memory {} → {}",
            memory(from.memory_mib),
            memory(to.memory_mib)
        ));
    }
    if from.display != to.display {
        out.push(format!(
            "display {} → {}",
            display(&from.display),
            display(&to.display)
        ));
    }
    if from.gpu != to.gpu {
        out.push(format!("GPU {} → {}", from.gpu, to.gpu));
    }
    if from.dax != to.dax {
        let way = |d| if d { "mapped" } else { "read in" };
        out.push(format!("image files {} → {}", way(from.dax), way(to.dax)));
    }
    if from.swap_mib != to.swap_mib {
        out.push(format!(
            "swap {} → {}",
            swap(from.swap_mib),
            swap(to.swap_mib)
        ));
    }
    out
}

fn display(d: &str) -> &'static str {
    if d == "desktop" {
        "desktop"
    } else {
        "headless"
    }
}

fn swap(s: Option<i64>) -> String {
    match s {
        None => "the worker's default".into(),
        Some(0) => "none".into(),
        Some(m) => memory(m),
    }
}

/// MiB as the web shows memory: GB from 1024 on, to a tenth.
pub fn memory(mib: i64) -> String {
    if mib >= 1024 {
        let g = (mib as f64 / 1024. * 10.).round() / 10.;
        if g.fract() == 0. {
            format!("{g:.0} GB")
        } else {
            format!("{g:.1} GB")
        }
    } else {
        format!("{mib} MB")
    }
}

/// A size typed in GiB ("4", "1.5"), in MiB.
pub fn parse_gib(text: &str) -> Option<i64> {
    let t = text.trim().trim_end_matches(['G', 'g', 'B', 'b', 'i', ' ']);
    let g: f64 = t.trim().parse().ok()?;
    (g > 0. && g.is_finite()).then(|| (g * 1024.).round() as i64)
}

/// How its swap is to be: its worker's default, none, or a size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SwapWay {
    Default,
    None,
    Size,
}

pub enum EnvSettingsEvent {
    /// The window is done with: OK, Cancel or Escape.
    Close,
}

gpui::actions!(environment_settings, [Cancel]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([gpui::KeyBinding::new(
        "escape",
        Cancel,
        Some("EnvironmentSettings"),
    )]);
}

pub struct EnvSettingsView {
    places: Entity<Places>,
    place: String,
    env: String,
    page: Page,
    search: Entity<InputState>,
    focus: FocusHandle,
    /// The machine as it is to be.
    cpus: Entity<InputState>,
    memory: Entity<InputState>,
    live: Entity<SliderState>,
    display: String,
    gpu: String,
    dax: bool,
    swap: SwapWay,
    swap_size: Entity<InputState>,
    task: Entity<InputState>,
    ports_public: bool,
    /// The copy chosen of each pack, by pack: None undoes a choice.
    copies: HashMap<String, Option<String>>,
    /// What was read of the server: its packs, the workers' sizes, what
    /// it uses as it runs.
    packs: Option<Result<Vec<EnvironmentPack>, String>>,
    sizes: Vec<Resources>,
    stats: Option<Stats>,
    /// What the memory slider runs between, while it runs.
    bounds: Option<(i64, i64)>,
    /// The machine the window started from, to compare the draft with.
    base: MachineSettings,
    busy: bool,
    error: Option<String>,
    _subs: Vec<gpui::Subscription>,
}

impl EventEmitter<EnvSettingsEvent> for EnvSettingsView {}

impl Focusable for EnvSettingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EnvSettingsView {
    pub fn new(
        places: Entity<Places>,
        place: &str,
        env: &str,
        page: Page,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let e = places
            .read(cx)
            .get(place)
            .and_then(|(_, s)| s.environment(env).cloned());
        let base = e.as_ref().map(saved).unwrap_or(MachineSettings {
            cpus: 1,
            memory_mib: 1024,
            display: "none".into(),
            gpu: "none".into(),
            dax: false,
            swap_mib: None,
        });
        let input = |value: String, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut s = InputState::new(window, cx);
                s.set_value(value, window, cx);
                s
            })
        };
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search settings"));
        let cpus = input(base.cpus.to_string(), window, cx);
        let gib = |mib: i64| {
            let g = mib as f64 / 1024.;
            if g.fract() == 0. {
                format!("{g:.0}")
            } else {
                format!("{g:.2}")
                    .trim_end_matches('0')
                    .trim_end_matches('.')
                    .to_string()
            }
        };
        let memory = input(gib(base.memory_mib), window, cx);
        let swap_size = input(
            gib(base.swap_mib.filter(|s| *s > 0).unwrap_or(base.memory_mib)),
            window,
            cx,
        );
        let task_text = e
            .as_ref()
            .and_then(|e| e.task.as_ref())
            .map(|t| t.text.clone())
            .unwrap_or_default();
        let task = cx.new(|cx| {
            let mut s = InputState::new(window, cx).placeholder("What it is for");
            s.set_value(task_text, window, cx);
            s
        });
        let live = cx.new(|_| {
            SliderState::new()
                .max(base.memory_mib as f32)
                .min(0.)
                .step(256.)
                .default_value(base.memory_mib as f32)
        });
        let mut subs = vec![];
        for i in [&search, &cpus, &memory, &swap_size, &task] {
            subs.push(cx.subscribe(i, |_, _, e: &InputEvent, cx| {
                if matches!(e, InputEvent::Change) {
                    cx.notify();
                }
            }));
        }
        subs.push(cx.subscribe(&live, |_, _, _: &SliderEvent, cx| cx.notify()));
        // Settings changed elsewhere, by the web or an agent, come in by the
        // event stream; the window shows them where nothing is drafted.
        subs.push(cx.observe(&places, |_, _, cx| cx.notify()));
        let mut view = EnvSettingsView {
            places,
            place: place.to_string(),
            env: env.to_string(),
            page,
            search,
            focus: cx.focus_handle(),
            cpus,
            memory,
            live,
            display: base.display.clone(),
            gpu: base.gpu.clone(),
            dax: base.dax,
            swap: match base.swap_mib {
                None => SwapWay::Default,
                Some(0) => SwapWay::None,
                Some(_) => SwapWay::Size,
            },
            swap_size,
            task,
            ports_public: e.as_ref().is_some_and(|e| e.ports_public),
            copies: HashMap::new(),
            packs: None,
            sizes: vec![],
            stats: None,
            bounds: None,
            base,
            busy: false,
            error: None,
            _subs: subs,
        };
        view.read(cx);
        view.watch(cx);
        view
    }

    fn client(&self, cx: &App) -> Option<Client> {
        self.places.read(cx).client(&self.place)
    }

    /// The environment, as its place last said.
    pub fn environment(&self, cx: &App) -> Option<Environment> {
        self.places
            .read(cx)
            .get(&self.place)
            .and_then(|(_, s)| s.environment(&self.env).cloned())
    }

    pub fn page(&self) -> Page {
        self.page
    }

    /// Whether these are the settings of that environment.
    pub fn is_for(&self, place: &str, env: &str) -> bool {
        self.place == place && self.env == env
    }

    pub fn show(&mut self, page: Page, cx: &mut Context<Self>) {
        self.page = page;
        cx.notify();
    }

    /// Reads what the window shows beyond the environment: its packs, the
    /// workers' sizes and what it uses as it runs.
    fn read(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client(cx) else {
            return;
        };
        let env = self.env.clone();
        let task = cx.background_spawn(async move {
            (
                client.environment_packs(&env),
                client.worker_sizes(),
                client.stats(&env),
            )
        });
        cx.spawn(async move |this, cx| {
            let (packs, sizes, stats) = task.await;
            let _ = this.update(cx, |v, cx| {
                v.packs = Some(packs.map_err(|e| e.to_string()));
                v.sizes = sizes.unwrap_or_default();
                v.take_stats(stats.ok().flatten(), cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Reads what it uses every few seconds while it runs, for the bounds
    /// its memory can be resized between.
    fn watch(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(5)).await;
                let Ok(Some((client, env))) = this.update(cx, |v, cx| {
                    let running = v.environment(cx).is_some_and(|e| e.phase == "running");
                    running
                        .then(|| v.client(cx).map(|c| (c, v.env.clone())))
                        .flatten()
                }) else {
                    if this.upgrade().is_none() {
                        return;
                    }
                    continue;
                };
                let stats = cx.background_spawn(async move { client.stats(&env) }).await;
                if this
                    .update(cx, |v, cx| v.take_stats(stats.ok().flatten(), cx))
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
    }

    /// Takes what it uses, as last read: while it runs, the bounds its
    /// memory can be resized between.
    pub fn take_stats(&mut self, stats: Option<Stats>, cx: &mut Context<Self>) {
        self.stats = stats;
        let bounds = self
            .environment(cx)
            .and_then(|e| live_memory(&e, self.stats.as_ref()));
        if bounds != self.bounds {
            // The slider starts at the memory it has, and keeps where it
            // was moved to as its bounds move. Its top is set before its
            // bottom: each is checked against the other.
            let first = self.bounds.is_none();
            self.bounds = bounds;
            if let Some((min, max)) = bounds {
                let base = self.base.memory_mib as f32;
                self.live.update(cx, |s, cx| {
                    let v = if first { base } else { s.value().end() };
                    *s = SliderState::new()
                        .max(max as f32)
                        .min(min as f32)
                        .step(256.)
                        .default_value(v.clamp(min as f32, max as f32));
                    cx.notify();
                });
            }
        }
        cx.notify();
    }

    /// The machine as the window has it, or why it cannot be.
    pub fn draft(&self, cx: &App) -> Result<MachineSettings, String> {
        let env = self.environment(cx);
        let live = env
            .as_ref()
            .and_then(|e| live_memory(e, self.stats.as_ref()));
        let memory_mib = if live.is_some() && self.bounds.is_some() {
            self.live.read(cx).value().end().round() as i64
        } else {
            parse_gib(&self.memory.read(cx).value())
                .ok_or("Memory is a number of GB, such as 4 or 1.5")?
        };
        let cpus: i64 = self
            .cpus
            .read(cx)
            .value()
            .trim()
            .parse()
            .ok()
            .filter(|n| *n >= 1)
            .ok_or("vCPUs is a whole number, 1 or more")?;
        let swap_mib = match self.swap {
            SwapWay::Default => None,
            SwapWay::None => Some(0),
            SwapWay::Size => Some(
                parse_gib(&self.swap_size.read(cx).value())
                    .ok_or("Swap is a number of GB, such as 4 or 1.5")?,
            ),
        };
        Ok(MachineSettings {
            cpus,
            memory_mib,
            display: self.display.clone(),
            gpu: self.gpu.clone(),
            dax: self.dax,
            swap_mib,
        })
    }

    /// Everything changed and not yet applied, a phrase each.
    pub fn pending(&self, cx: &App) -> Vec<String> {
        let Some(env) = self.environment(cx) else {
            return vec![];
        };
        let mut out = match self.draft(cx) {
            Ok(d) => changes(&saved(&env), &d),
            Err(_) => vec!["the machine".into()],
        };
        if self.ports_public != env.ports_public {
            out.push(format!(
                "ports {}",
                if self.ports_public {
                    "public"
                } else {
                    "private"
                }
            ));
        }
        let task = env.task.as_ref().map(|t| t.text.as_str()).unwrap_or("");
        if self.task.read(cx).value().trim() != task.trim() {
            out.push("its task".into());
        }
        for pack in self.copies.keys() {
            let name = self
                .packs
                .as_ref()
                .and_then(|p| p.as_ref().ok())
                .and_then(|ps| ps.iter().find(|p| &p.id == pack))
                .map(|p| p.name.clone())
                .unwrap_or_else(|| "a pack".into());
            out.push(format!("{name}'s copy"));
        }
        out
    }

    /// Asks the server for what is drafted: the machine, who reaches its
    /// ports, its task and its packs' copies, one after the other; then
    /// done (closing on OK) or the first refusal said.
    pub fn apply(&mut self, close: bool, cx: &mut Context<Self>) {
        let Some(env) = self.environment(cx) else {
            return;
        };
        let Some(client) = self.client(cx) else {
            return;
        };
        let machine = match self.draft(cx) {
            Ok(d) => d,
            Err(e) => {
                self.error = Some(e);
                cx.notify();
                return;
            }
        };
        let machine = (machine != saved(&env)).then_some(machine);
        let ports = (self.ports_public != env.ports_public).then_some(self.ports_public);
        let text = self.task.read(cx).value().trim().to_string();
        let had = env
            .task
            .as_ref()
            .map(|t| t.text.trim().to_string())
            .unwrap_or_default();
        let task = (text != had).then_some(text);
        let copies: Vec<(String, Option<String>)> = self.copies.drain().collect();
        if machine.is_none() && ports.is_none() && task.is_none() && copies.is_empty() {
            if close {
                cx.emit(EnvSettingsEvent::Close);
            }
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        let id = self.env.clone();
        let call = cx.background_spawn(async move {
            let mut latest = None;
            if let Some(m) = &machine {
                latest = Some(client.update_settings(&id, m).map_err(|e| e.to_string())?);
            }
            if let Some(p) = ports {
                latest = Some(client.set_ports_public(&id, p).map_err(|e| e.to_string())?);
            }
            if let Some(t) = &task {
                if t.is_empty() {
                    client.end_task(&id).map_err(|e| e.to_string())?;
                } else {
                    client.set_task(&id, t).map_err(|e| e.to_string())?;
                }
            }
            for (pack, copy) in &copies {
                client
                    .choose_copy(&id, pack, copy.as_deref())
                    .map_err(|e| e.to_string())?;
            }
            let packs = if copies.is_empty() {
                None
            } else {
                Some(client.environment_packs(&id).map_err(|e| e.to_string()))
            };
            if task.is_some() || latest.is_none() {
                latest = client.environment(&id).ok();
            }
            Ok::<_, String>((latest, packs))
        });
        cx.spawn(async move |this, cx| {
            let got = call.await;
            let _ = this.update(cx, |v, cx| {
                v.busy = false;
                match got {
                    Ok((latest, packs)) => {
                        if let Some(e) = latest {
                            v.base = saved(&e);
                            let place = v.place.clone();
                            v.places.update(cx, |p, cx| {
                                p.apply(&place, Update::Environment(Box::new(e)), cx)
                            });
                        }
                        if let Some(p) = packs {
                            v.packs = Some(p);
                        }
                        if close {
                            cx.emit(EnvSettingsEvent::Close);
                        }
                    }
                    Err(e) => v.error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Changes its image at once: upgrade, rollback, keep or cancel.
    fn image(&mut self, change: &'static str, force: bool, cx: &mut Context<Self>) {
        let Some(client) = self.client(cx) else {
            return;
        };
        let id = self.env.clone();
        self.busy = true;
        self.error = None;
        cx.notify();
        let call = cx.background_spawn(async move { client.change_image(&id, change, force) });
        cx.spawn(async move |this, cx| {
            let got = call.await;
            let _ = this.update(cx, |v, cx| {
                v.busy = false;
                match got {
                    Ok(e) => {
                        let place = v.place.clone();
                        v.places.update(cx, |p, cx| {
                            p.apply(&place, Update::Environment(Box::new(e)), cx)
                        });
                    }
                    Err(e) => v.error = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Resets it to its template, or deletes it, once asked.
    fn act(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.environment(cx).map(|e| e.name).unwrap_or_default();
        let (title, body, ok) = match action {
            Action::Reset => (
                format!("Reset {name} to its template?"),
                "It takes its template's current settings; its disks and everything on them are kept.",
                "Reset",
            ),
            _ => (
                format!("Delete {name}?"),
                "Its disks and everything on them go with it. This cannot be undone.",
                "Delete",
            ),
        };
        let (places, place, env) = (self.places.clone(), self.place.clone(), self.env.clone());
        let me = cx.entity().downgrade();
        crate::treeops::confirm(&title, body, ok, window, cx, move |_, cx| {
            places.update(cx, |p, cx| p.act(&place, &env, action, cx));
            if action == Action::Delete {
                let _ = me.update(cx, |_, cx| cx.emit(EnvSettingsEvent::Close));
            }
        });
    }
}

impl Render for EnvSettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let size = window.viewport_size();
        let w = (size.width - px(40.)).min(px(860.));
        let h = (size.height - px(70.)).min(px(600.));
        let env = self.environment(cx);
        let name = env.as_ref().map(|e| e.name.clone()).unwrap_or_default();
        let pending = self.pending(cx);
        let body = match &env {
            Some(env) => self.page_body(env, &p, window, cx).into_any_element(),
            None => note("The environment is gone.", &p).into_any_element(),
        };
        let summary = if let Some(e) = &self.error {
            div().text_color(p.bad).child(e.clone())
        } else if self.busy {
            div().text_color(p.muted).child("Applying…")
        } else if pending.is_empty() {
            div()
        } else {
            let running = env.as_ref().is_some_and(|e| e.phase == "running");
            let tail = if running {
                " Memory changes as it runs."
            } else {
                ""
            };
            div()
                .text_color(p.muted)
                .truncate()
                .child(format!("Changed: {}.{tail}", pending.join(", ")))
        };
        v_flex()
            .id("env-settings-window")
            .debug_selector(|| "env-settings-window".into())
            .track_focus(&self.focus)
            .key_context("EnvironmentSettings")
            .on_action(cx.listener(|_, _: &Cancel, _, cx| cx.emit(EnvSettingsEvent::Close)))
            .w(w)
            .h(h)
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
                    .px(px(14.))
                    .py(px(9.))
                    .border_b_1()
                    .border_color(p.border)
                    .bg(p.sidebar)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!("Environment Settings — {name}")),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .child(self.tree(&name, &p, cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .px(px(18.))
                                    .pt(px(12.))
                                    .pb(px(6.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(format!("{name} › {}", self.page.title())),
                            )
                            .child(
                                div()
                                    .id("env-settings-page")
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .px(px(18.))
                                    .pb(px(16.))
                                    .child(body),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .px(px(14.))
                    .py(px(10.))
                    .gap(px(8.))
                    .border_t_1()
                    .border_color(p.border)
                    .child(div().flex_1().min_w_0().text_size(px(11.5)).child(summary))
                    .child(
                        button("env-settings-cancel", "Cancel", &p)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(EnvSettingsEvent::Close))),
                    )
                    .child(if !pending.is_empty() && !self.busy {
                        button("env-settings-apply", "Apply", &p)
                            .debug_selector(|| "env-settings-apply".into())
                            .on_click(cx.listener(|this, _, _, cx| this.apply(false, cx)))
                    } else {
                        disabled("env-settings-apply", "Apply", &p)
                    })
                    .child(
                        primary("env-settings-ok", "OK", &p)
                            .debug_selector(|| "env-settings-ok".into())
                            .on_click(cx.listener(|this, _, _, cx| this.apply(true, cx))),
                    ),
            )
    }
}

impl EnvSettingsView {
    fn tree(&self, name: &str, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let q = self.search.read(cx).value().to_string();
        let mut list = v_flex().gap(px(1.)).child(
            div()
                .px(px(6.))
                .py(px(2.))
                .mt(px(4.))
                .text_color(p.muted)
                .font_weight(FontWeight::SEMIBOLD)
                .truncate()
                .child(format!("▾ {name}")),
        );
        for page in Page::ALL.into_iter().filter(|pg| pg.matches(&q)) {
            let on = page == self.page;
            list = list.child(
                div()
                    .id(SharedString::from(format!(
                        "env-settings-node-{}",
                        page.title().to_lowercase().replace(' ', "-")
                    )))
                    .debug_selector(move || {
                        format!(
                            "env-settings-node-{}",
                            page.title().to_lowercase().replace(' ', "-")
                        )
                    })
                    .pl(px(22.))
                    .pr(px(6.))
                    .py(px(2.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .when(on, |d| d.bg(p.selection))
                    .when(!on, |d| d.hover(|s| s.bg(p.surface_2)))
                    .child(page.title())
                    .on_click(cx.listener(move |this, _, _, cx| this.show(page, cx))),
            );
        }
        v_flex()
            .w(px(200.))
            .flex_none()
            .p(px(8.))
            .gap(px(6.))
            .bg(p.sidebar)
            .border_r_1()
            .border_color(p.border)
            .child(Input::new(&self.search).small())
            .child(list)
    }

    fn page_body(
        &mut self,
        env: &Environment,
        p: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        match self.page {
            Page::General => self.general(env, p, window, cx).into_any_element(),
            Page::Resources => self.resources(env, p, cx).into_any_element(),
            Page::Files => self.files(p, cx).into_any_element(),
            Page::Access => access(env, p).into_any_element(),
            Page::Ports => self.ports(env, p, cx).into_any_element(),
            Page::Danger => self.danger(env, p, window, cx).into_any_element(),
        }
    }

    fn general(
        &self,
        env: &Environment,
        p: &Palette,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let d = &env.details;
        let digest = |s: &str| {
            s.strip_prefix("sha256:")
                .unwrap_or(s)
                .chars()
                .take(12)
                .collect::<String>()
        };
        let mut image = v_flex().gap(px(6.)).child(
            field("Image", p).child(
                div().min_w_0().truncate().child(
                    d.image
                        .clone()
                        .unwrap_or_else(|| "As its template names it".into()),
                ),
            ),
        );
        if let Some(dg) = &d.image_digest {
            image =
                image.child(field("Copy", p).child(div().text_color(p.muted).child(digest(dg))));
        }
        // What needs deciding about its image, as its tab's banners say it.
        let busy = self.busy;
        for (i, b) in crate::banners::banners(env)
            .into_iter()
            .filter(|b| b.kind == crate::banners::Kind::Image)
            .enumerate()
        {
            let mut acts = h_flex().gap(px(8.));
            for (j, a) in b.acts.iter().enumerate() {
                let id = SharedString::from(format!("env-image-{i}-{j}"));
                acts = acts.child(match a.does {
                    crate::banners::Does::Image { change, force } if a.ready && !busy => button(
                        id, a.label, p,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.image(change, force, cx))),
                    _ => disabled(id, a.label, p),
                });
            }
            image = image
                .child(note(&b.text, p))
                .when(!b.files.is_empty(), |d| {
                    d.child(
                        div()
                            .max_w(px(560.))
                            .text_size(px(11.))
                            .font_family(crate::diff::mono())
                            .text_color(p.muted)
                            .children(b.files.iter().map(|f| div().child(f.clone()))),
                    )
                })
                .child(acts);
        }
        let changed = &d.template_changes;
        v_flex()
            .gap(px(10.))
            .child(section("Environment", p))
            .child(field("Name", p).child(env.name.clone()))
            .child(field("Owner", p).child(env.owner.clone()))
            .child(field("Task", p).child(div().w(px(360.)).child(Input::new(&self.task).small())))
            .child(note("Empty marks its task done.", p))
            .child(section("Template", p))
            .child(field("Template", p).child(env.template.clone()))
            .when(!changed.is_empty(), |d| {
                d.child(note(
                    &format!(
                        "Changed from its template: {}. Reset to Template, in the Danger zone, gives it the template's.",
                        changed.join(", ")
                    ),
                    p,
                ))
            })
            .child(section("Image", p))
            .child(image)
    }

    fn resources(
        &self,
        env: &Environment,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let live = live_memory(env, self.stats.as_ref());
        let fixed = !changeable(env);
        let place_name = self
            .places
            .read(cx)
            .get(&self.place)
            .map(|(c, _)| c.name.clone())
            .unwrap_or_default();
        let most = self.sizes.first().copied();
        let mut memory_part = v_flex().gap(px(4.));
        if let Some((min, max)) = live {
            let now = self.live.read(cx).value().end().round() as i64;
            memory_part = memory_part
                .child(div().px(px(4.)).py(px(6.)).child(Slider::new(&self.live)))
                .child(
                    h_flex()
                        .text_size(px(11.))
                        .text_color(p.muted)
                        .child(div().flex_1().child(format!("{} · the least", memory(min))))
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .justify_center()
                                .text_color(p.text)
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(memory(now)),
                        )
                        .child(div().flex_1().flex().justify_end().child(format!(
                            "{} · the most {place_name} has room for",
                            memory(max)
                        ))),
                );
            if let Some(s) = &self.stats {
                let mut used = format!(
                    "In use {} · on the host {}",
                    memory(s.memory_used_mib),
                    memory(s.host_memory_mib)
                );
                if s.swap_used_mib > 0 {
                    used.push_str(&format!(" · swapped {}", memory(s.swap_used_mib)));
                }
                used.push_str(". It can't go below what its programs use plus 512 MB.");
                memory_part = memory_part.child(note(&used, p));
            }
        } else {
            memory_part = memory_part.child(
                field("Memory", p).child(
                    h_flex()
                        .gap(px(6.))
                        .child(
                            div()
                                .w(px(80.))
                                .child(Input::new(&self.memory).small().disabled(fixed)),
                        )
                        .child("GB")
                        .when_some(most, |d, m| {
                            d.child(div().text_color(p.muted).child(format!(
                                "up to {} on the largest worker",
                                memory(m.memory_mib)
                            )))
                        }),
                ),
            );
        }
        let swap_seg =
            |id: &'static str, label: &'static str, way: SwapWay, cx: &mut Context<Self>| {
                seg(id, label, self.swap == way, fixed, p).on_click(cx.listener(
                    move |this, _, _, cx| {
                        if !fixed {
                            this.swap = way;
                            cx.notify();
                        }
                    },
                ))
            };
        let display_seg =
            |id: &'static str, label: &'static str, value: &'static str, cx: &mut Context<Self>| {
                seg(id, label, self.display == value, fixed, p).on_click(cx.listener(
                    move |this, _, _, cx| {
                        if !fixed {
                            this.display = value.into();
                            cx.notify();
                        }
                    },
                ))
            };
        let gpu_seg =
            |id: &'static str, label: &'static str, value: &'static str, cx: &mut Context<Self>| {
                seg(id, label, self.gpu == value, fixed, p).on_click(cx.listener(
                    move |this, _, _, cx| {
                        if !fixed {
                            this.gpu = value.into();
                            cx.notify();
                        }
                    },
                ))
            };
        let passthrough = env.details.gpu.as_deref() == Some("passthrough");
        let hint = if changeable(env) {
            "They apply when it next starts."
        } else if live.is_some() {
            "Its memory changes as it runs. Stop it to change the rest; they apply when it next starts."
        } else {
            "Stop it to change these. They apply when it next starts."
        };
        let placement: Vec<String> = env
            .details
            .placement
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        v_flex()
            .gap(px(10.))
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(section("Memory", p))
                    .when(live.is_some(), |d| {
                        d.child(
                            div()
                                .pt(px(4.))
                                .text_size(px(11.))
                                .text_color(p.muted)
                                .child("changes while it runs"),
                        )
                    }),
            )
            .child(memory_part)
            .child(section("vCPUs", p))
            .child(
                field("vCPUs", p).child(
                    h_flex()
                        .gap(px(6.))
                        .child(
                            div()
                                .w(px(60.))
                                .child(Input::new(&self.cpus).small().disabled(fixed)),
                        )
                        .when_some(most, |d, m| {
                            d.child(
                                div()
                                    .text_color(p.muted)
                                    .child(format!("of {} on the largest worker", m.cpus)),
                            )
                        }),
                ),
            )
            .child(section("Swap", p))
            .child(
                field("Swap", p).child(
                    h_flex()
                        .gap(px(8.))
                        .child(
                            h_flex()
                                .rounded(px(6.))
                                .border_1()
                                .border_color(p.border_strong)
                                .overflow_hidden()
                                .child(swap_seg(
                                    "swap-default",
                                    "Worker's Default",
                                    SwapWay::Default,
                                    cx,
                                ))
                                .child(swap_seg("swap-none", "None", SwapWay::None, cx))
                                .child(swap_seg("swap-size", "Size", SwapWay::Size, cx)),
                        )
                        .when(self.swap == SwapWay::Size, |d| {
                            d.child(
                                div()
                                    .w(px(70.))
                                    .child(Input::new(&self.swap_size).small().disabled(fixed)),
                            )
                            .child("GB")
                        }),
                ),
            )
            .child(note(
                "Behind zswap; made empty at each cold start, kept across a suspend.",
                p,
            ))
            .child(section("Machine", p))
            .child(
                field("Display", p).child(
                    h_flex()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(p.border_strong)
                        .overflow_hidden()
                        .child(display_seg("display-desktop", "Desktop", "desktop", cx))
                        .child(display_seg("display-none", "Headless", "none", cx)),
                ),
            )
            .child(
                field("GPU", p).child(
                    h_flex()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(p.border_strong)
                        .overflow_hidden()
                        .child(gpu_seg("gpu-none", "None", "none", cx))
                        .child(gpu_seg("gpu-virtual", "Virtual", "virtual", cx))
                        .when(passthrough, |d| {
                            d.child(gpu_seg("gpu-passthrough", "Passthrough", "passthrough", cx))
                        }),
                ),
            )
            .child(
                field("Image files", p).child(
                    Checkbox::new("dax")
                        .label("Map them from the worker's memory (DAX)")
                        .checked(self.dax)
                        .disabled(fixed)
                        .on_click(cx.listener(|this, b: &bool, _, cx| {
                            this.dax = *b;
                            cx.notify();
                        })),
                ),
            )
            .when(!placement.is_empty(), |d| {
                d.child(
                    field("Placement", p)
                        .child(
                            div()
                                .font_family(crate::diff::mono())
                                .child(placement.join(", ")),
                        )
                        .child(div().text_color(p.muted).child("from its template")),
                )
            })
            .child(note(hint, p))
    }

    fn files(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let mine = self
            .places
            .read(cx)
            .get(&self.place)
            .and_then(|(_, s)| s.me.as_ref())
            .map(|m| m.username.clone())
            .unwrap_or_default();
        let body = match &self.packs {
            None => note("Reading its packs…", p).into_any_element(),
            Some(Err(e)) => div().text_color(p.bad).child(e.clone()).into_any_element(),
            Some(Ok(list)) if list.is_empty() => note("It has no packs.", p).into_any_element(),
            Some(Ok(list)) => {
                let mut rows = v_flex().gap(px(6.));
                for (i, pk) in list.iter().enumerate() {
                    let drafted = self.copies.get(&pk.id);
                    let chosen = match drafted {
                        Some(Some(c)) => pk.copies.iter().find(|x| &x.id == c).cloned(),
                        Some(None) => None,
                        None => Some(pk.copy.clone()),
                    };
                    let label = match (&chosen, drafted) {
                        (Some(c), _) => copy_name(c, &mine),
                        (None, _) => "As it was given".into(),
                    };
                    let from = match (drafted, pk.from.as_str()) {
                        (Some(Some(_)), _) | (None, "environment") => "chosen here",
                        (Some(None), _) => "back to how it was given",
                        (None, "template") => "the template's",
                        (None, "kept") => "the one it was given",
                        (None, "default") => "your default",
                        _ => "",
                    };
                    let options: Vec<(String, String)> = pk
                        .copies
                        .iter()
                        .map(|c| (c.id.clone(), copy_name(c, &mine)))
                        .collect();
                    let me = cx.entity().downgrade();
                    let pack = pk.id.clone();
                    let can_undo = drafted.is_some_and(|d| d.is_some())
                        || (drafted.is_none() && pk.from == "environment");
                    let whose = match (&pk.owner, &pk.team, pk.builtin) {
                        (_, _, true) => "Hangar's".to_string(),
                        (_, Some(t), _) => format!("team {t}"),
                        (Some(o), _, _) => o.clone(),
                        _ => String::new(),
                    };
                    rows = rows.child(
                        v_flex()
                            .gap(px(4.))
                            .px(px(10.))
                            .py(px(7.))
                            .border_1()
                            .border_color(p.border)
                            .rounded(px(6.))
                            .child(
                                h_flex()
                                    .gap(px(8.))
                                    .child(
                                        div()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(pk.name.clone()),
                                    )
                                    .child(
                                        div().text_size(px(11.)).text_color(p.muted).child(whose),
                                    )
                                    .child(div().flex_1())
                                    .when(!pk.listed, |d| {
                                        d.child(
                                            div()
                                                .text_size(px(11.))
                                                .text_color(p.muted)
                                                .child("attaches itself"),
                                        )
                                    }),
                            )
                            .when(!pk.description.is_empty(), |d| {
                                d.child(note(&pk.description, p))
                            })
                            .child(
                                h_flex()
                                    .gap(px(8.))
                                    .child(div().text_color(p.muted).child("Copy"))
                                    .child(
                                        gpui_component::button::Button::new(SharedString::from(
                                            format!("pack-copy-{i}"),
                                        ))
                                        .small()
                                        .outline()
                                        .label(format!("{label} ▾"))
                                        .dropdown_menu(
                                            move |mut menu, _, cx| {
                                                crate::ui::modal(cx);
                                                for (id, name) in options.clone() {
                                                    let (me, pack) = (me.clone(), pack.clone());
                                                    menu = menu.item(
                                                        PopupMenuItem::new(name).on_click(
                                                            move |_, _, cx| {
                                                                let _ = me.update(cx, |v, cx| {
                                                                    v.choose(
                                                                        &pack,
                                                                        Some(id.clone()),
                                                                        cx,
                                                                    )
                                                                });
                                                            },
                                                        ),
                                                    );
                                                }
                                                menu
                                            },
                                        ),
                                    )
                                    .child(div().text_size(px(11.)).text_color(p.muted).child(from))
                                    .when(can_undo, |d| {
                                        let pack = pk.id.clone();
                                        d.child(
                                            button(
                                                SharedString::from(format!("pack-reset-{i}")),
                                                "Reset",
                                                p,
                                            )
                                            .on_click(
                                                cx.listener(move |this, _, _, cx| {
                                                    this.choose(&pack, None, cx)
                                                }),
                                            ),
                                        )
                                    }),
                            ),
                    );
                }
                rows.into_any_element()
            }
        };
        v_flex()
            .gap(px(10.))
            .child(section("Packs", p))
            .child(note(
                "The shared files it has, and which copy of each: a copy is one set of a pack's files, yours or shared. A running environment changes over as it runs.",
                p,
            ))
            .child(body)
    }

    /// Drafts the copy of a pack it is to use; None returns it to how it
    /// was given.
    fn choose(&mut self, pack: &str, copy: Option<String>, cx: &mut Context<Self>) {
        let current = self
            .packs
            .as_ref()
            .and_then(|p| p.as_ref().ok())
            .and_then(|ps| ps.iter().find(|p| p.id == pack))
            .map(|p| (p.copy.id.clone(), p.from.clone()));
        let unchanged = match (&copy, &current) {
            (Some(c), Some((id, from))) => c == id && from == "environment",
            (None, Some((_, from))) => from != "environment",
            _ => false,
        };
        if unchanged {
            self.copies.remove(pack);
        } else {
            self.copies.insert(pack.to_string(), copy);
        }
        cx.notify();
    }

    fn ports(&self, env: &Environment, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let public = self.ports_public;
        let port_seg =
            |id: &'static str, label: &'static str, value: bool, cx: &mut Context<Self>| {
                seg(id, label, public == value, false, p).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.ports_public = value;
                        cx.notify();
                    },
                ))
            };
        v_flex()
            .gap(px(10.))
            .child(section("Its web servers", p))
            .child(
                field("Reached by", p).child(
                    h_flex()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(p.border_strong)
                        .overflow_hidden()
                        .child(port_seg("ports-private", "Private", false, cx))
                        .child(port_seg("ports-public", "Public", true, cx)),
                ),
            )
            .child(note(
                if public {
                    "Anyone who can reach Hangar, without signing in. A port reached by its number stays behind sign-in."
                } else {
                    "People who may use this environment, signed in to Hangar."
                },
                p,
            ))
            .when(!env.web_names.is_empty(), |d| {
                d.child(field("Its names", p).child(env.web_names.join(", ")))
            })
            .child(note("The Ports tool window lists what it listens on, each opened in the browser.", p))
    }

    fn danger(
        &self,
        env: &Environment,
        p: &Palette,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let stopped = matches!(env.phase.as_str(), "stopped" | "failed");
        let changed = &env.details.template_changes;
        let reset_note = if changed.is_empty() {
            "It takes its template's current settings; its disks and everything on them are kept."
                .to_string()
        } else {
            format!(
                "It takes its template's current settings ({}); its disks and everything on them are kept.",
                changed.join(", ")
            )
        };
        let row = |title: &'static str, about: String, control: gpui::AnyElement| {
            h_flex()
                .gap(px(14.))
                .px(px(12.))
                .py(px(9.))
                .border_1()
                .border_color(p.border)
                .rounded(px(6.))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(px(2.))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                        .child(div().text_size(px(11.5)).text_color(p.muted).child(about)),
                )
                .child(control)
        };
        let reset = if stopped {
            button("env-reset", "Reset to Template…", p)
                .on_click(cx.listener(|this, _, window, cx| this.act(Action::Reset, window, cx)))
                .into_any_element()
        } else {
            disabled("env-reset", "Reset to Template…", p).into_any_element()
        };
        let delete = div()
            .id("env-delete")
            .px(px(10.))
            .py(px(3.))
            .rounded(px(6.))
            .border_1()
            .border_color(p.bad)
            .text_color(p.bad)
            .cursor_pointer()
            .hover(|s| s.bg(p.bad.opacity(0.08)))
            .child("Delete…")
            .on_click(cx.listener(|this, _, window, cx| this.act(Action::Delete, window, cx)))
            .into_any_element();
        v_flex()
            .gap(px(10.))
            .child(row(
                "Reset to template",
                if stopped {
                    reset_note
                } else {
                    format!("{reset_note} Stop it first.")
                },
                reset,
            ))
            .child(row(
                "Delete",
                "Its disks and everything on them go with it. This cannot be undone.".into(),
                delete,
            ))
    }
}

/// The Access page: what its template keeps from it, each said either way.
fn access(env: &Environment, p: &Palette) -> impl IntoElement {
    let a = env.details.access;
    let rows = [
        (
            "Packs that attach themselves",
            !a.no_self_attached,
            "the profile among them",
        ),
        (
            "Sensitive files",
            !a.no_sensitive_files,
            "credentials in its packs, such as Claude's sign-in",
        ),
        (
            "Signing with your SSH keys",
            !a.no_ssh_keys,
            "through its SSH agent; the keys never enter it",
        ),
        (
            "Hangar's registry",
            !a.no_registry,
            "a credential for pulling and pushing images",
        ),
        (
            "Running code in its folders",
            !a.no_editor_trust,
            "language servers, without asking first",
        ),
    ];
    let mut list = v_flex()
        .border_1()
        .border_color(p.border)
        .rounded(px(6.))
        .overflow_hidden();
    for (i, (what, given, about)) in rows.into_iter().enumerate() {
        list = list.child(
            h_flex()
                .gap(px(10.))
                .px(px(10.))
                .py(px(6.))
                .when(i > 0, |d| d.border_t_1().border_color(p.border))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(what)
                        .child(div().text_size(px(11.)).text_color(p.muted).child(about)),
                )
                .child(
                    div()
                        .text_size(px(11.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(if given { p.good } else { p.muted })
                        .child(if given { "Given" } else { "Kept from it" }),
                ),
        );
    }
    v_flex()
        .gap(px(10.))
        .child(section("What it is given", p))
        .child(list)
        .child(note(
            "Its template says these, for code you do not trust; change the template to change them.",
            p,
        ))
}

/// A copy as the web names it: yours, someone's own, or a shared one by
/// its name.
fn copy_name(c: &crate::api::PackCopy, me: &str) -> String {
    if c.personal {
        match &c.owner {
            Some(o) if o == me => "Yours".into(),
            Some(o) => format!("{o}'s own"),
            None => "Personal".into(),
        }
    } else {
        match &c.team {
            Some(t) => format!("{} ({t})", c.name),
            None => c.name.clone(),
        }
    }
}

/// One choice of a segmented control.
fn seg(
    id: &'static str,
    label: &'static str,
    on: bool,
    off: bool,
    p: &Palette,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .px(px(12.))
        .py(px(3.))
        .when(!off, |d| d.cursor_pointer())
        .when(on, |d| d.bg(p.accent).text_color(p.on_accent))
        .when(!on && !off, |d| d.hover(|s| s.bg(p.surface_2)))
        .when(off && !on, |d| d.text_color(p.muted))
        .when(off && on, |d| d.opacity(0.6))
        .child(label)
}

/// A heading over a page's settings.
fn section(title: &str, p: &Palette) -> impl IntoElement {
    div()
        .pt(px(4.))
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(p.muted)
        .child(title.to_uppercase())
}

/// A setting's label, beside what sets it.
fn field(label: &str, p: &Palette) -> gpui::Div {
    h_flex().gap(px(12.)).child(
        div()
            .w(px(90.))
            .flex_none()
            .text_color(p.muted)
            .child(label.to_string()),
    )
}

/// A line explaining a page or a setting.
fn note(text: &str, p: &Palette) -> gpui::Div {
    div()
        .max_w(px(560.))
        .text_size(px(11.5))
        .text_color(p.muted)
        .child(text.to_string())
}
