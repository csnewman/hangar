//! Settings › Sync › Linked Places: each of the person's
//! places, where it stands among their linked places and which it reaches,
//! and the Link Places sheet that links them. What is decided is
//! `linking`'s; this reads the places and makes its calls.

use std::collections::HashSet;
use std::time::Duration;

use chrono::Utc;
use gpui::{
    Context, Entity, EventEmitter, FontWeight, IntoElement, ParentElement, Render, SharedString,
    Styled, Task, Window, div, prelude::*, px,
};
use gpui_component::{h_flex, v_flex};

use crate::api::{Client, Error};
use crate::linking::{self, Known, State, Step};
use crate::model::Tone;
use crate::palette::{self, Palette};
use crate::places::Places;
use crate::ui::{button, disabled, dot, primary};

/// How often the page reads the places again while it is open.
const EVERY: Duration = Duration::from_secs(15);

/// A place as the app has it, to read.
struct Source {
    id: String,
    name: String,
    kind: &'static str,
    url: String,
    client: Option<Client>,
}

fn sources(places: &Entity<Places>, cx: &gpui::App) -> Vec<Source> {
    let places = places.read(cx);
    places
        .list()
        .iter()
        .map(|(c, _)| Source {
            id: c.id.clone(),
            name: c.name.clone(),
            kind: if c.proxmox.is_some() {
                "proxmox"
            } else {
                "server"
            },
            url: c.url.clone(),
            client: places.client(&c.id),
        })
        .collect()
}

/// What each place says of its links, read all at once.
fn read(sources: Vec<Source>) -> Vec<Known> {
    std::thread::scope(|s| {
        let reads: Vec<_> = sources
            .into_iter()
            .map(|src| {
                s.spawn(move || {
                    let (links, username) = match &src.client {
                        None => (Err("Sign in to read its links.".to_string()), None),
                        Some(c) => (
                            c.links().map_err(|e| match e {
                                Error::Status(404, _) => {
                                    "Its server is too old to link places.".to_string()
                                }
                                Error::Transport(_) => "Offline".to_string(),
                                e => e.to_string(),
                            }),
                            c.me().ok().map(|m| m.username),
                        ),
                    };
                    Known {
                        id: src.id,
                        name: src.name,
                        kind: src.kind,
                        url: src.url,
                        username,
                        links,
                    }
                })
            })
            .collect();
        reads.into_iter().filter_map(|r| r.join().ok()).collect()
    })
}

/// Makes the calls, in order, at the places the app knows them by; gives
/// what failed, in words.
fn run(steps: Vec<Step>, sources: Vec<Source>) -> Vec<String> {
    let mut failed = vec![];
    for step in steps {
        let at = match &step {
            Step::Put { at, .. } | Step::Delete { at, .. } => at.clone(),
        };
        let Some(src) = sources.iter().find(|s| s.id == at) else {
            continue;
        };
        let Some(client) = &src.client else {
            failed.push(format!("{}: not signed in", src.name));
            continue;
        };
        let r = match &step {
            Step::Put {
                other,
                name,
                public_key,
                url,
                person,
                ..
            } => client
                .put_link(other, name, public_key, url, person)
                .map(|_| ()),
            Step::Delete { other, .. } => match client.delete_link(other) {
                Err(Error::Status(404, _)) => Ok(()),
                r => r,
            },
        };
        if let Err(e) = r {
            failed.push(format!("{}: {e}", src.name));
        }
    }
    failed
}

pub struct LinkedView {
    places: Entity<Places>,
    known: Vec<Known>,
    loaded: bool,
    /// The place whose links are shown below its row.
    selected: Option<String>,
    busy: bool,
    error: Option<String>,
    sheet: Option<Entity<LinkSheet>>,
    _poll: Task<()>,
    _subs: Vec<gpui::Subscription>,
}

impl LinkedView {
    pub fn new(places: Entity<Places>, cx: &mut Context<Self>) -> Self {
        let poll = cx.spawn(async move |this, cx| {
            loop {
                if this.update(cx, |v, cx| v.load(cx)).is_err() {
                    return;
                }
                cx.background_executor().timer(EVERY).await;
            }
        });
        let subs = vec![cx.observe(&places, |v, _, cx| v.load(cx))];
        LinkedView {
            places,
            known: vec![],
            loaded: false,
            selected: None,
            busy: false,
            error: None,
            sheet: None,
            _poll: poll,
            _subs: subs,
        }
    }

    /// The Link Places sheet, while it is open.
    pub fn sheet(&self) -> Option<Entity<LinkSheet>> {
        self.sheet.clone()
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let sources = sources(&self.places, cx);
        let task = cx.background_spawn(async move { read(sources) });
        cx.spawn(async move |this, cx| {
            let known = task.await;
            let _ = this.update(cx, |v, cx| {
                v.known = known;
                v.loaded = true;
                cx.notify();
            });
        })
        .detach();
    }

    fn open_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let places = self.places.clone();
        let known = self.known.clone();
        let sheet = cx.new(|cx| LinkSheet::new(places, known, window, cx));
        self._subs
            .push(cx.subscribe(&sheet, |v, _, e: &SheetEvent, cx| match e {
                SheetEvent::Close { linked } => {
                    v.sheet = None;
                    if *linked {
                        v.load(cx);
                    }
                    cx.notify();
                }
            }));
        self.sheet = Some(sheet);
        cx.notify();
    }

    fn unlink(&mut self, place: &str, cx: &mut Context<Self>) {
        let Some(target) = self.known.iter().find(|k| k.id == place).cloned() else {
            return;
        };
        let steps = linking::unlink_steps(&target, &self.known);
        let sources = sources(&self.places, cx);
        self.busy = true;
        self.error = None;
        cx.notify();
        let task = cx.background_spawn(async move { run(steps, sources) });
        cx.spawn(async move |this, cx| {
            let failed = task.await;
            let _ = this.update(cx, |v, cx| {
                v.busy = false;
                v.error = (!failed.is_empty()).then(|| failed.join("\n"));
                v.load(cx);
            });
        })
        .detach();
    }
}

/// When something happened, in a few words: "just now", "4 min ago".
fn ago(at: chrono::DateTime<Utc>) -> String {
    let s = (Utc::now() - at).num_seconds().max(0);
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86399 => format!("{} h ago", s / 3600),
        _ => format!("{} days ago", s / 86400),
    }
}

fn state_cell(state: &State, p: &Palette) -> gpui::Div {
    let (tone, text) = match state {
        State::Linked => (Some(Tone::Good), "Linked".to_string()),
        State::UpToDate => (Some(Tone::Good), "Up to date".to_string()),
        State::Behind(_) => (Some(Tone::Busy), "Not up to date".to_string()),
        State::Waiting(_) => (Some(Tone::Busy), "Waiting".to_string()),
        State::NotLinked => (None, "Not linked".to_string()),
        State::NotAllowed => (None, "Not linked".to_string()),
        State::Unknown(e) => (Some(Tone::Idle), e.clone()),
    };
    h_flex()
        .gap(px(6.))
        .min_w_0()
        .when_some(tone, |d, t| d.child(dot(t, p)))
        .child(div().min_w_0().truncate().child(text))
}

fn reach_cell(row: &linking::Row) -> String {
    match &row.state {
        State::NotAllowed => "Its administrators do not allow personal links".into(),
        State::NotLinked | State::Unknown(_) => String::new(),
        _ if !row.reaches.is_empty() => row.reaches.join(", "),
        _ if !row.reached_by.is_empty() => {
            format!("No linked place; reached by {}", row.reached_by.join(", "))
        }
        _ => "No linked place".into(),
    }
}

impl Render for LinkedView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let rows = linking::rows(&self.known, Utc::now());
        let header = h_flex()
            .px(px(10.))
            .py(px(5.))
            .gap(px(10.))
            .bg(p.surface_2)
            .text_size(px(10.5))
            .text_color(p.muted)
            .child(div().w(px(150.)).flex_none().child("PLACE"))
            .child(div().w(px(170.)).flex_none().child("STATE"))
            .child(div().flex_1().child("REACHES DIRECTLY"));
        let mut table = v_flex()
            .max_w(px(640.))
            .border_1()
            .border_color(p.border)
            .rounded(px(6.))
            .overflow_hidden()
            .child(header);
        for row in &rows {
            let on = self.selected.as_deref() == Some(row.id.as_str());
            let off = matches!(row.state, State::NotAllowed);
            let id = row.id.clone();
            table = table.child(
                h_flex()
                    .id(SharedString::from(format!("linked-row-{}", row.id)))
                    .debug_selector({
                        let id = row.id.clone();
                        move || format!("linked-row-{id}")
                    })
                    .px(px(10.))
                    .py(px(6.))
                    .gap(px(10.))
                    .border_t_1()
                    .border_color(p.border)
                    .cursor_pointer()
                    .when(on, |d| d.bg(p.accent_soft))
                    .when(!on, |d| d.hover(|s| s.bg(p.surface_2)))
                    .when(off, |d| d.text_color(p.muted))
                    .child(
                        v_flex()
                            .w(px(150.))
                            .flex_none()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(row.name.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(10.5))
                                    .text_color(p.muted)
                                    .child(row.kind),
                            ),
                    )
                    .child(state_cell(&row.state, &p).w(px(170.)).flex_none())
                    .child(div().flex_1().min_w_0().child(reach_cell(row)))
                    .on_click(cx.listener(move |v, _, _, cx| {
                        v.selected = if v.selected.as_deref() == Some(id.as_str()) {
                            None
                        } else {
                            Some(id.clone())
                        };
                        cx.notify();
                    })),
            );
            if on {
                table = table.child(self.detail(row, &p, cx));
            }
        }
        let linkable = self
            .known
            .iter()
            .filter(|k| k.links.as_ref().is_ok_and(|l| l.allowed))
            .count();
        v_flex()
            .gap(px(8.))
            .child(
                div()
                    .max_w(px(620.))
                    .text_size(px(11.5))
                    .text_color(p.muted)
                    .child(
                        "Linked places share your packs and their files, personal templates, SSH keys, sign-in keys and account settings. Each place works on its own and catches up whenever it can reach another linked place.",
                    ),
            )
            .child(if self.loaded {
                table.into_any_element()
            } else {
                div()
                    .text_color(p.muted)
                    .child("Reading your places…")
                    .into_any_element()
            })
            .child(
                h_flex()
                    .gap(px(6.))
                    .child(if linkable >= 2 && !self.busy {
                        button("linked-link", "Link Places…", &p)
                            .debug_selector(|| "linked-link".into())
                            .on_click(cx.listener(|v, _, window, cx| v.open_sheet(window, cx)))
                    } else {
                        disabled("linked-link", "Link Places…", &p)
                    }),
            )
            .when_some(self.error.clone(), |d, e| {
                d.child(div().max_w(px(620.)).text_color(p.bad).child(e))
            })
            .when(self.loaded, |d| d.child(self.sending(&p, cx)))
    }
}

/// An amount carried, in words.
fn carried(n: u64) -> String {
    match n {
        n if n >= 1_000_000_000 => format!("{:.1} GB", n as f64 / 1e9),
        n if n >= 1_000_000 => format!("{:.1} MB", n as f64 / 1e6),
        n if n >= 1000 => format!("{:.0} kB", n as f64 / 1e3),
        n => format!("{n} B"),
    }
}

impl LinkedView {
    /// Relay through this Mac, for a place: whether it is on, what it has
    /// carried this month, whether it pauses on an expensive network, and
    /// what it carries now.
    fn relay_controls(&self, row: &linking::Row, p: &Palette, cx: &mut Context<Self>) -> gpui::Div {
        use gpui_component::checkbox::Checkbox;
        let relay = crate::relay::Relay::global();
        let c = relay.config();
        let on = c.places.contains(&row.id);
        let id = row.id.clone();
        let now = relay.relaying(&row.id);
        let mut d = v_flex()
            .gap(px(3.))
            .pt(px(4.))
            .text_color(p.text)
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(
                        Checkbox::new(SharedString::from(format!("relay-{}", row.id)))
                            .label("Relay through this Mac")
                            .checked(on)
                            .on_click(cx.listener(move |_, b: &bool, _, cx| {
                                crate::relay::Relay::global().set_place(&id, *b);
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(p.muted)
                            .child(format!("{} this month", carried(relay.carried(&row.id)))),
                    ),
            )
            .child(
                Checkbox::new("relay-pause")
                    .label("Pause on Personal Hotspot, cellular and Low Data Mode")
                    .checked(!c.on_expensive)
                    .on_click(cx.listener(|_, b: &bool, _, cx| {
                        crate::relay::Relay::global().set_on_expensive(!*b);
                        cx.notify();
                    })),
            );
        if on && relay.paused() {
            d = d.child(
                div()
                    .text_color(p.muted)
                    .child("Paused: this Mac's network is expensive or in Low Data Mode."),
            );
        } else if !now.is_empty() {
            d = d.child(
                div()
                    .text_color(p.muted)
                    .child(format!("Carrying its link to {} now.", now.join(", "))),
            );
        } else if on {
            d = d.child(
                div()
                    .text_color(p.muted)
                    .child("Its links are carried whenever neither side reaches the other."),
            );
        }
        d
    }
}

/// The bandwidth limits the page offers, in bytes a second; 0 is none.
const BANDWIDTHS: [i64; 6] = [0, 256_000, 1_000_000, 5_000_000, 10_000_000, 50_000_000];

/// A bandwidth limit, in words.
fn bandwidth_label(b: i64) -> String {
    match b {
        0 => "No limit".into(),
        b if b >= 1_000_000 => format!("{} MB/s", b / 1_000_000),
        b => format!("{} kB/s", b / 1000),
    }
}

impl LinkedView {
    /// The limit the places hold: the person's, the same in every linked
    /// place once they have exchanged records.
    fn bandwidth(&self) -> i64 {
        self.known
            .iter()
            .filter_map(|k| k.links.as_ref().ok())
            .find_map(|l| l.bandwidth)
            .unwrap_or(0)
    }

    /// Sets the bandwidth limit in every place the app reaches.
    fn set_bandwidth(&mut self, b: i64, cx: &mut Context<Self>) {
        let sources = sources(&self.places, cx);
        self.busy = true;
        self.error = None;
        cx.notify();
        let task = cx.background_spawn(async move {
            let mut failed = vec![];
            for s in sources {
                if let Some(c) = &s.client
                    && let Err(e) = c.set_bandwidth(b)
                    && !matches!(e, Error::Status(404, _))
                {
                    failed.push(format!("{}: {e}", s.name));
                }
            }
            failed
        });
        cx.spawn(async move |this, cx| {
            let failed = task.await;
            let _ = this.update(cx, |v, cx| {
                v.busy = false;
                v.error = (!failed.is_empty()).then(|| failed.join("\n"));
                v.load(cx);
            });
        })
        .detach();
    }

    /// How files are sent: the burst rule, set per path on each pack, and
    /// the bandwidth limit.
    fn sending(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_component::button::Button;
        use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
        use gpui_component::{Disableable as _, Sizable as _};
        let chosen = self.bandwidth();
        let me = cx.entity().downgrade();
        let row = |label: &'static str| {
            h_flex()
                .max_w(px(640.))
                .gap(px(8.))
                .px(px(6.))
                .py(px(3.))
                .border_1()
                .border_color(p.border)
                .rounded(px(5.))
                .child(div().flex_1().child(label))
        };
        v_flex()
            .gap(px(4.))
            .child(
                div()
                    .mt(px(8.))
                    .text_size(px(10.5))
                    .text_color(p.muted)
                    .child("SENDING"),
            )
            .child(row("Files written in bursts").child(
                div().text_size(px(10.5)).text_color(p.muted).child(
                    "At most once a second, the latest each time · set per path on each pack",
                ),
            ))
            .child(
                row("Bandwidth per link").child(
                    Button::new("linked-bandwidth")
                        .small()
                        .outline()
                        .label(format!("{} ▾", bandwidth_label(chosen)))
                        .disabled(self.busy)
                        .dropdown_menu(move |mut menu, _, cx| {
                            crate::ui::modal(cx);
                            for b in BANDWIDTHS {
                                let me = me.clone();
                                menu = menu.item(
                                    PopupMenuItem::new(bandwidth_label(b))
                                        .checked(b == chosen)
                                        .on_click(move |_, _, cx| {
                                            let _ = me.update(cx, |v, cx| v.set_bandwidth(b, cx));
                                        }),
                                );
                            }
                            menu
                        }),
                ),
            )
    }

    /// A place's links, below its row, and Unlink.
    fn detail(&self, row: &linking::Row, p: &Palette, cx: &mut Context<Self>) -> gpui::Div {
        let mut d = v_flex()
            .pl(px(170.))
            .pr(px(10.))
            .pt(px(4.))
            .pb(px(9.))
            .gap(px(5.))
            .bg(p.accent_soft)
            .text_size(px(11.5))
            .text_color(p.muted);
        let why = match &row.state {
            State::Unknown(e) => Some(format!("{}: {e}", row.name)),
            State::NotAllowed => Some(format!(
                "{}'s administrators do not allow personal links.",
                row.name
            )),
            State::NotLinked => Some(format!(
                "{} is not linked. Link Places… links it with your other places.",
                row.name
            )),
            State::Waiting(_) => Some(format!(
                "{} reaches no linked place, and none reaches it. Its changes wait until one can, or until this Mac relays them.",
                row.name
            )),
            State::Behind(e) => Some(format!(
                "{}'s records could not be exchanged with a linked place: {e}",
                row.name
            )),
            State::Linked | State::UpToDate => None,
        };
        if let Some(why) = why {
            d = d.child(div().child(why));
        }
        for l in &row.links {
            let reached = match (&l.error, l.reached) {
                (Some(e), _) => format!("not reached: {e}"),
                (None, Some(at)) => format!("reached {}", ago(at)),
                (None, None) => "not reached yet".into(),
            };
            let heard = l
                .heard
                .map(|at| format!(" · heard from {}", ago(at)))
                .unwrap_or_default();
            let synced = match (&l.sync_error, l.synced) {
                (Some(e), _) => format!(" · records not exchanged: {e}"),
                (None, Some(at)) => format!(" · records exchanged {}", ago(at)),
                (None, None) => String::new(),
            };
            d = d.child(
                h_flex()
                    .gap(px(8.))
                    .child(
                        div()
                            .w(px(110.))
                            .flex_none()
                            .truncate()
                            .text_color(p.text)
                            .child(l.name.clone()),
                    )
                    .child(div().min_w_0().child(format!("{reached}{heard}{synced}"))),
            );
        }
        if !row.links.is_empty() {
            d = d.child(self.relay_controls(row, p, cx));
        }
        if !row.links.is_empty() {
            let id = row.id.clone();
            d = d.child(h_flex().child(if self.busy {
                disabled("linked-unlink", "Unlinking…", p)
            } else {
                button("linked-unlink", "Unlink This Place", p)
                    .debug_selector(|| "linked-unlink".into())
                    .on_click(cx.listener(move |v, _, _, cx| v.unlink(&id, cx)))
            }));
        }
        d
    }
}

/// What the Link Places sheet tells the page.
pub enum SheetEvent {
    /// Closed; whether it linked anything.
    Close { linked: bool },
}

/// Link Places: the person's places to link, each one's standing, what is
/// shared, and Link.
pub struct LinkSheet {
    places: Entity<Places>,
    known: Vec<Known>,
    chosen: HashSet<String>,
    busy: bool,
    error: Option<String>,
}

impl EventEmitter<SheetEvent> for LinkSheet {}

impl LinkSheet {
    fn new(
        places: Entity<Places>,
        known: Vec<Known>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Self {
        let chosen = known
            .iter()
            .filter(|k| k.links.as_ref().is_ok_and(|l| l.allowed))
            .map(|k| k.id.clone())
            .collect();
        LinkSheet {
            places,
            known,
            chosen,
            busy: false,
            error: None,
        }
    }

    fn selected(&self) -> Vec<&Known> {
        self.known
            .iter()
            .filter(|k| self.chosen.contains(&k.id))
            .collect()
    }

    fn link(&mut self, cx: &mut Context<Self>) {
        let steps = match linking::link_steps(&self.selected()) {
            Ok(s) => s,
            Err(e) => {
                self.error = Some(e);
                cx.notify();
                return;
            }
        };
        if steps.is_empty() {
            cx.emit(SheetEvent::Close { linked: false });
            return;
        }
        let sources = sources(&self.places, cx);
        self.busy = true;
        self.error = None;
        cx.notify();
        let task = cx.background_spawn(async move { run(steps, sources) });
        cx.spawn(async move |this, cx| {
            let failed = task.await;
            let _ = this.update(cx, |v, cx| {
                v.busy = false;
                if failed.is_empty() {
                    cx.emit(SheetEvent::Close { linked: true });
                } else {
                    v.error = Some(failed.join("\n"));
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

impl Render for LinkSheet {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let mut list = v_flex().gap(px(4.));
        let mut joining: Vec<String> = vec![];
        for k in &self.known {
            let (can, label) = match &k.links {
                Err(e) => (false, e.clone()),
                Ok(l) if !l.allowed => (false, "not allowed by its administrators".into()),
                Ok(l) if !l.items.is_empty() => (true, "linked".into()),
                Ok(_) => (
                    true,
                    match &k.username {
                        Some(u) => format!("new · your account there: {u}"),
                        None => "new".into(),
                    },
                ),
            };
            let on = can && self.chosen.contains(&k.id);
            if on && !k.links.as_ref().is_ok_and(|l| !l.items.is_empty()) {
                joining.push(k.name.clone());
            }
            let id = k.id.clone();
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("link-pick-{}", k.id)))
                    .debug_selector({
                        let id = k.id.clone();
                        move || format!("link-pick-{id}")
                    })
                    .gap(px(8.))
                    .px(px(9.))
                    .py(px(5.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(if on { p.accent } else { p.border })
                    .when(on, |d| d.bg(p.accent_soft))
                    .when(!can, |d| d.opacity(0.55))
                    .when(can && !self.busy, |d| {
                        d.cursor_pointer().on_click(cx.listener(move |s, _, _, cx| {
                            if !s.chosen.remove(&id) {
                                s.chosen.insert(id.clone());
                            }
                            s.error = None;
                            cx.notify();
                        }))
                    })
                    .child(div().child(if on { "☑" } else { "☐" }))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(k.name.clone()),
                    )
                    .child(
                        div()
                            .ml_auto()
                            .text_size(px(11.))
                            .text_color(p.muted)
                            .child(label),
                    ),
            );
        }
        let note = |title: String, text: &'static str| {
            div().text_size(px(11.5)).text_color(p.muted).child(
                gpui::StyledText::new(format!("{title} {text}")).with_highlights(vec![(
                    0..title.len(),
                    gpui::HighlightStyle {
                        color: Some(p.text),
                        font_weight: Some(FontWeight::SEMIBOLD),
                        ..Default::default()
                    },
                )]),
            )
        };
        let can_link = self.selected().len() >= 2 && !self.busy;
        v_flex()
            .id("link-sheet")
            .debug_selector(|| "link-sheet".into())
            .w(px(540.))
            .bg(p.surface)
            .border_1()
            .border_color(p.border_strong)
            .rounded(px(10.))
            .shadow_lg()
            .text_size(px(12.5))
            .child(
                div()
                    .px(px(16.))
                    .py(px(12.))
                    .border_b_1()
                    .border_color(p.border)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Link Places"),
            )
            .child(
                v_flex()
                    .px(px(16.))
                    .py(px(12.))
                    .gap(px(9.))
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(p.muted)
                            .child("Choose the places that share your data. Each links with your account there."),
                    )
                    .child(list)
                    .child(note(
                        "Shared:".into(),
                        "packs and their files, personal templates, SSH keys and sign-in keys, account settings. Environments, access tokens and each place's own templates stay where they are.",
                    ))
                    .when(!joining.is_empty() && self.selected().len() >= 2, |d| {
                        d.child(note(
                            receives(&joining),
                            "Each place keeps them sealed with its own key.",
                        ))
                    })
                    .when_some(self.error.clone(), |d, e| {
                        d.child(div().text_color(p.bad).child(e))
                    })
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .justify_end()
                            .child(
                                button("link-cancel", "Cancel", &p).on_click(cx.listener(
                                    |s, _, _, cx| {
                                        if !s.busy {
                                            cx.emit(SheetEvent::Close { linked: false })
                                        }
                                    },
                                )),
                            )
                            .child(if can_link {
                                primary("link-go", "Link", &p)
                                    .debug_selector(|| "link-go".into())
                                    .on_click(cx.listener(|s, _, _, cx| s.link(cx)))
                            } else if self.busy {
                                disabled("link-go", "Linking…", &p)
                            } else {
                                disabled("link-go", "Link", &p)
                            }),
                    ),
            )
    }
}

/// "Rack receives …", "Rack and Homelab receive …".
fn receives(names: &[String]) -> String {
    let who = match names {
        [] => String::new(),
        [one] => return format!("{one} receives your SSH private keys and sensitive files."),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    };
    format!("{who} receive your SSH private keys and sensitive files.")
}
