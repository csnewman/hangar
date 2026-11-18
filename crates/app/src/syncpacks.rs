//! Settings › Sync › Packs: each of the person's packs once, the built-in
//! profile and their own, with the places it is in; and the chosen one's
//! paths (how often a file written in bursts is sent, and whether it is
//! sensitive), what it attaches to, its copies, the places it reaches,
//! and the versions of its files that another place's newer change
//! replaced, with Restore. A person's own pack is the same in every place
//! they link, so a change is made in one place that holds it and reaches
//! the rest; the profile's paths are each place's administrators'.

use std::time::Duration;

use gpui::{
    Context, Entity, FontWeight, IntoElement, ParentElement, Render, SharedString, Styled, Task,
    Window, div, prelude::*, px,
};
use gpui_component::button::Button;
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{Sizable, h_flex, v_flex};
use hangar_openapi::clientapi::types;

use crate::api::{Client, Error};
use crate::palette::{self, Palette};
use crate::places::Places;
use crate::ui::{button, disabled, primary};

/// How often the page reads the places again while it is open.
const EVERY: Duration = Duration::from_secs(15);

/// How often a path's files may be sent, as the page offers it: seconds,
/// 0 for every change as written.
const SEND_CHOICES: [i64; 6] = [0, 5, 10, 30, 60, 300];

/// A place as the app has it, to read.
#[derive(Clone)]
struct Source {
    id: String,
    name: String,
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
            client: places.client(&c.id),
        })
        .collect()
}

/// What one place says of the person's packs.
#[derive(Clone)]
struct Held {
    place: String,
    name: String,
    /// The user's ID there.
    me: Option<String>,
    packs: Result<Vec<types::Pack>, String>,
    /// Whether its administrators allow personal links, and whether it
    /// has any.
    allowed: bool,
    linked: bool,
    /// Its ID as linked places know it.
    place_id: Option<String>,
}

fn read(sources: Vec<Source>) -> Vec<Held> {
    std::thread::scope(|s| {
        let reads: Vec<_> = sources
            .into_iter()
            .map(|src| {
                s.spawn(move || {
                    let Some(c) = &src.client else {
                        return Held {
                            place: src.id,
                            name: src.name,
                            me: None,
                            packs: Err("Sign in to read its packs.".into()),
                            allowed: false,
                            linked: false,
                            place_id: None,
                        };
                    };
                    let links = c.links().ok();
                    Held {
                        place: src.id.clone(),
                        name: src.name.clone(),
                        me: c.me().ok().map(|m| m.id),
                        packs: c.packs().map_err(words),
                        allowed: links.as_ref().is_some_and(|l| l.allowed),
                        linked: links.as_ref().is_some_and(|l| !l.items.is_empty()),
                        place_id: links.as_ref().map(|l| l.place.id.clone()),
                    }
                })
            })
            .collect();
        reads.into_iter().filter_map(|r| r.join().ok()).collect()
    })
}

fn words(e: Error) -> String {
    match e {
        Error::Status(404, _) => "Its server is too old to list packs here.".into(),
        Error::Transport(_) => "Offline".into(),
        e => e.to_string(),
    }
}

/// The name every place knows a pack by: the profile, or a person's own
/// pack by its ID; none for any other.
fn key(k: &types::Pack, me: Option<&str>) -> Option<String> {
    if k.builtin {
        Some("profile".into())
    } else if k.team_id.is_none() && me.is_some() && k.owner_id.as_deref() == me {
        Some(k.id.clone())
    } else {
        None
    }
}

/// A pack as the page lists it: its key, its name, and the places it is
/// in, each with its ID there.
#[derive(Clone)]
struct Entry {
    key: String,
    name: String,
    builtin: bool,
    sensitive: bool,
    /// Place, its name, the pack's ID there.
    places: Vec<(String, String, String)>,
}

fn entries(held: &[Held]) -> Vec<Entry> {
    let mut out: Vec<Entry> = vec![];
    for h in held {
        let Ok(packs) = &h.packs else { continue };
        for k in packs {
            let Some(key) = key(k, h.me.as_deref()) else {
                continue;
            };
            let at = (h.place.clone(), h.name.clone(), k.id.clone());
            match out.iter_mut().find(|e| e.key == key) {
                Some(e) => e.places.push(at),
                None => out.push(Entry {
                    name: if k.builtin {
                        "Profile".into()
                    } else {
                        k.name.clone()
                    },
                    builtin: k.builtin,
                    sensitive: k.paths.iter().any(|p| p.sensitive),
                    key,
                    places: vec![at],
                }),
            }
        }
    }
    out.sort_by(|a, b| {
        (!a.builtin, a.name.to_lowercase()).cmp(&(!b.builtin, b.name.to_lowercase()))
    });
    out
}

/// The chosen pack at one place: the pack, its copies, and the history of
/// the person's own copy there.
#[derive(Clone)]
struct Detail {
    place: String,
    name: String,
    pack: types::Pack,
    copies: Vec<types::Copy>,
    own: Option<String>,
    history: Vec<types::ReplacedFile>,
}

fn details(entry: &Entry, sources: &[Source]) -> Vec<Detail> {
    std::thread::scope(|s| {
        let reads: Vec<_> = entry
            .places
            .iter()
            .filter_map(|(place, name, id)| {
                let c = sources.iter().find(|s| &s.id == place)?.client.clone()?;
                Some(s.spawn(move || {
                    let pack = c.packs().ok()?.into_iter().find(|k| &k.id == id)?;
                    let copies = c.copies(id).unwrap_or_default();
                    let own = copies.iter().find(|c| c.personal).map(|c| c.id.clone());
                    let history = own
                        .as_deref()
                        .and_then(|o| c.copy_history(o).ok())
                        .unwrap_or_default();
                    Some(Detail {
                        place: place.clone(),
                        name: name.clone(),
                        pack,
                        copies,
                        own,
                        history,
                    })
                }))
            })
            .collect();
        reads
            .into_iter()
            .filter_map(|r| r.join().ok().flatten())
            .collect()
    })
}

/// How often a path's files are sent, in words.
fn send_label(n: Option<i64>) -> String {
    match n {
        None => "Once a second".into(),
        Some(0) => "As written".into(),
        Some(n) if n % 60 == 0 => format!("At most every {} min", n / 60),
        Some(n) => format!("At most every {n} s"),
    }
}

/// When a kept version goes, in a few words: "25 Nov".
fn day(at: chrono::DateTime<chrono::Utc>) -> String {
    at.with_timezone(&chrono::Local)
        .format("%-d %b")
        .to_string()
}

/// What replaced a kept version, in words.
fn replaced_by(r: &types::ReplacedFile, place: &str) -> String {
    if r.merged == Some(true) {
        format!("{place}'s, replaced when two packs were merged")
    } else if r.by_here {
        format!("{place}'s, replaced by a restore")
    } else if let Some(by) = &r.by {
        format!("{place}'s, replaced by {by}'s newer change")
    } else {
        format!("{place}'s, replaced by another place's newer change")
    }
}

pub struct SyncPacksView {
    places: Entity<Places>,
    held: Vec<Held>,
    loaded: bool,
    /// The pack shown, by key.
    selected: Option<String>,
    details: Vec<Detail>,
    busy: bool,
    error: Option<String>,
    /// The path to add, and the name of a pack to make.
    new_path: Option<Entity<InputState>>,
    new_pack: Option<Entity<InputState>>,
    _poll: Task<()>,
    _subs: Vec<gpui::Subscription>,
}

impl SyncPacksView {
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
        SyncPacksView {
            places,
            held: vec![],
            loaded: false,
            selected: None,
            details: vec![],
            busy: false,
            error: None,
            new_path: None,
            new_pack: None,
            _poll: poll,
            _subs: subs,
        }
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let sources = sources(&self.places, cx);
        let task = cx.background_spawn(async move { read(sources) });
        cx.spawn(async move |this, cx| {
            let held = task.await;
            let _ = this.update(cx, |v, cx| {
                v.held = held;
                v.loaded = true;
                let list = entries(&v.held);
                if v.selected
                    .as_ref()
                    .is_none_or(|s| !list.iter().any(|e| &e.key == s))
                {
                    v.selected = list.first().map(|e| e.key.clone());
                }
                v.load_detail(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn load_detail(&mut self, cx: &mut Context<Self>) {
        let Some(entry) = self.entry() else {
            self.details.clear();
            return;
        };
        let sources = sources(&self.places, cx);
        let task = cx.background_spawn(async move { details(&entry, &sources) });
        cx.spawn(async move |this, cx| {
            let found = task.await;
            let _ = this.update(cx, |v, cx| {
                // Only for the pack still shown.
                if v.entry().is_some_and(|e| {
                    found
                        .iter()
                        .all(|d| e.places.iter().any(|(p, _, _)| p == &d.place))
                }) {
                    v.details = found;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn entry(&self) -> Option<Entry> {
        let key = self.selected.as_ref()?;
        entries(&self.held).into_iter().find(|e| &e.key == key)
    }

    fn select(&mut self, key: String, cx: &mut Context<Self>) {
        if self.selected.as_ref() != Some(&key) {
            self.selected = Some(key);
            self.details.clear();
            self.error = None;
            self.load_detail(cx);
            cx.notify();
        }
    }

    /// The place a change to the chosen pack is made at: the first that
    /// holds it and lets the person change it.
    fn editable(&self) -> Option<&Detail> {
        self.details
            .iter()
            .find(|d| d.pack.can_change && !d.pack.builtin)
    }

    fn client(&self, place: &str, cx: &gpui::App) -> Option<Client> {
        self.places.read(cx).client(place)
    }

    /// Runs a call in the background, then reads the places again.
    fn act(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce() -> Result<(), String> + Send + 'static,
    ) {
        self.busy = true;
        self.error = None;
        cx.notify();
        let task = cx.background_spawn(async move { f() });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |v, cx| {
                v.busy = false;
                v.error = r.err();
                v.load(cx);
            });
        })
        .detach();
    }

    /// Changes the chosen pack's paths or attach rule where it is changed.
    fn change(
        &mut self,
        edit: impl FnOnce(&mut types::PackInput) + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(d) = self.editable().cloned() else {
            return;
        };
        let Some(c) = self.client(&d.place, cx) else {
            return;
        };
        let mut input = types::PackInput {
            name: d.pack.name.clone(),
            description: Some(d.pack.description.clone()),
            attach: Some(d.pack.attach.clone()),
            squash: Some(d.pack.squash.clone()),
            paths: d.pack.paths.clone(),
            team_id: None,
        };
        edit(&mut input);
        self.act(cx, move || {
            c.update_pack(&d.pack.id, &input)
                .map(|_| ())
                .map_err(|e| e.to_string())
        });
    }

    fn add_path(&mut self, cx: &mut Context<Self>) {
        let Some(input) = &self.new_path else { return };
        let path = input.read(cx).value().trim().to_string();
        if path.is_empty() {
            return;
        }
        input.update(cx, |_, cx| cx.notify());
        self.change(
            move |i| {
                i.paths.push(types::PackPath {
                    path,
                    sensitive: false,
                    send_every: None,
                })
            },
            cx,
        );
    }

    fn make_pack(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self.new_pack.clone() else {
            return;
        };
        let name = input.read(cx).value().trim().to_string();
        if name.is_empty() {
            return;
        }
        // Made in the first place that links; the rest take it from there.
        let at = self
            .held
            .iter()
            .find(|h| h.linked && h.packs.is_ok())
            .or_else(|| self.held.iter().find(|h| h.packs.is_ok()))
            .map(|h| h.place.clone());
        let Some(c) = at.and_then(|p| self.client(&p, cx)) else {
            return;
        };
        input.update(cx, |s, cx| s.set_value("", window, cx));
        self.act(cx, move || {
            let body = types::PackInput {
                name,
                description: None,
                attach: Some("owner".into()),
                squash: None,
                paths: vec![],
                team_id: None,
            };
            c.create_pack(&body).map(|_| ()).map_err(|e| e.to_string())
        });
    }
}

impl Render for SyncPacksView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        if self.new_path.is_none() {
            self.new_path =
                Some(cx.new(|cx| {
                    InputState::new(window, cx).placeholder("~/.config/tool/ or ~/.npmrc")
                }));
        }
        if self.new_pack.is_none() {
            self.new_pack = Some(cx.new(|cx| InputState::new(window, cx).placeholder("Name")));
        }
        if !self.loaded {
            return div()
                .text_color(p.muted)
                .child("Reading your places…")
                .into_any_element();
        }
        let list = entries(&self.held);
        let mut left = v_flex().w(px(220.)).flex_none().gap(px(4.));
        for e in &list {
            let on = self.selected.as_ref() == Some(&e.key);
            let k = e.key.clone();
            left = left.child(
                v_flex()
                    .id(SharedString::from(format!("sync-pack-{}", e.key)))
                    .debug_selector({
                        let k = e.key.clone();
                        move || format!("sync-pack-{k}")
                    })
                    .px(px(8.))
                    .py(px(5.))
                    .gap(px(2.))
                    .rounded(px(6.))
                    .border_1()
                    .cursor_pointer()
                    .border_color(if on { p.accent } else { p.border })
                    .when(on, |d| d.bg(p.accent_soft))
                    .when(!on, |d| d.hover(|s| s.bg(p.surface_2)))
                    .child(
                        h_flex()
                            .gap(px(6.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(if e.sensitive {
                                        format!("{} 🔒", e.name)
                                    } else {
                                        e.name.clone()
                                    }),
                            )
                            .child(
                                div()
                                    .text_size(px(10.5))
                                    .text_color(p.muted)
                                    .child(if e.builtin { "built-in" } else { "yours" }),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap(px(3.))
                            .flex_wrap()
                            .children(e.places.iter().map(|(_, name, _)| reach(name, &p))),
                    )
                    .on_click(cx.listener(move |v, _, _, cx| v.select(k.clone(), cx))),
            );
        }
        if let Some(input) = &self.new_pack {
            left = left.child(
                h_flex()
                    .gap(px(4.))
                    .pt(px(4.))
                    .child(div().flex_1().min_w_0().child(Input::new(input).small()))
                    .child(if self.busy {
                        disabled("sync-pack-new", "New Pack", &p)
                    } else {
                        button("sync-pack-new", "New Pack", &p)
                            .on_click(cx.listener(|v, _, window, cx| v.make_pack(window, cx)))
                    }),
            );
        }
        let unread: Vec<String> = self
            .held
            .iter()
            .filter_map(|h| h.packs.as_ref().err().map(|e| format!("{}: {e}", h.name)))
            .collect();
        v_flex()
            .gap(px(8.))
            .child(note(
                "Your packs, each once, with the places each is in. Your own packs are the same in every linked place; the profile's paths are each place's administrators'.",
                &p,
            ))
            .child(
                h_flex()
                    .items_start()
                    .gap(px(12.))
                    .child(left)
                    .child(self.detail(&p, cx)),
            )
            .when(!unread.is_empty(), |d| {
                d.child(note(&unread.join("\n"), &p))
            })
            .when_some(self.error.clone(), |d, e| {
                d.child(div().max_w(px(620.)).text_color(p.bad).child(e))
            })
            .into_any_element()
    }
}

/// A place a pack is in, as a small rounded label.
fn reach(name: &str, p: &Palette) -> impl IntoElement {
    div()
        .px(px(6.))
        .rounded(px(9.))
        .border_1()
        .border_color(p.border_strong)
        .text_size(px(10.))
        .text_color(p.muted)
        .whitespace_nowrap()
        .child(name.to_string())
}

fn heading(text: &str, p: &Palette) -> impl IntoElement {
    div()
        .mt(px(8.))
        .text_size(px(10.5))
        .text_color(p.muted)
        .child(text.to_uppercase())
}

fn note(text: &str, p: &Palette) -> impl IntoElement {
    div()
        .max_w(px(620.))
        .text_size(px(11.5))
        .text_color(p.muted)
        .child(text.to_string())
}

/// A row of the detail: bordered, its parts in a line.
fn row(p: &Palette) -> gpui::Div {
    h_flex()
        .gap(px(8.))
        .px(px(6.))
        .py(px(3.))
        .min_w_0()
        .border_1()
        .border_color(p.border)
        .rounded(px(5.))
}

impl SyncPacksView {
    fn detail(&self, p: &Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(entry) = self.entry() else {
            return note("You have no packs in your places yet.", p).into_any_element();
        };
        let Some(first) = self.details.first() else {
            return div()
                .flex_1()
                .text_color(p.muted)
                .child("Reading the pack…")
                .into_any_element();
        };
        let edit = self.editable().cloned();
        let shown = edit.clone().unwrap_or_else(|| first.clone());
        let k = &shown.pack;
        let mut d = v_flex().flex_1().min_w_0().gap(px(4.));

        if let Some(other) = k.same_name.as_ref().and_then(|o| {
            self.held
                .iter()
                .filter_map(|h| h.packs.as_ref().ok())
                .flatten()
                .find(|x| &x.id == o)
        }) && let Some(at) = edit.clone()
        {
            let other_name = other.name.clone();
            let id = k.id.clone();
            let place = at.place.clone();
            let id2 = id.clone();
            let place2 = place.clone();
            d = d.child(
                v_flex()
                    .gap(px(6.))
                    .p(px(8.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(p.warn_border)
                    .bg(p.warn_bg)
                    .child(format!(
                        "This pack was made in another of your places with the name of {other_name}, and was renamed when the places were linked. Merging it into {other_name} joins their paths and files: of a file both have, the one changed later is kept, and the other is kept in the copy's history for 7 days. The same happens in every linked place."
                    ))
                    .child(
                        h_flex()
                            .gap(px(6.))
                            .child(if self.busy {
                                disabled("sync-pack-merge", format!("Merge into {other_name}"), p)
                            } else {
                                primary("sync-pack-merge", format!("Merge into {other_name}"), p)
                                    .on_click(cx.listener(move |v, _, _, cx| {
                                        if let Some(c) = v.client(&place, cx) {
                                            let id = id.clone();
                                            v.act(cx, move || {
                                                c.merge_pack(&id).map(|_| ()).map_err(|e| e.to_string())
                                            });
                                        }
                                    }))
                            })
                            .child(if self.busy {
                                disabled("sync-pack-apart", "Keep Apart", p)
                            } else {
                                button("sync-pack-apart", "Keep Apart", p).on_click(cx.listener(
                                    move |v, _, _, cx| {
                                        if let Some(c) = v.client(&place2, cx) {
                                            let id = id2.clone();
                                            v.act(cx, move || {
                                                c.keep_pack_apart(&id)
                                                    .map(|_| ())
                                                    .map_err(|e| e.to_string())
                                            });
                                        }
                                    },
                                ))
                            }),
                    ),
            );
        }

        d = d.child(heading("Paths", p));
        if entry.builtin {
            d = d.child(note(
                "The profile's paths are each place's administrators'; these are the first place's.",
                p,
            ));
        } else if edit.is_none() {
            d = d.child(note(
                "No place that holds this pack can be reached to change it.",
                p,
            ));
        }
        let can = edit.is_some() && !self.busy;
        for (i, path) in k.paths.iter().enumerate() {
            let excluded = path.path.starts_with('!');
            let mut r = row(p).child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(crate::diff::mono())
                    .text_size(px(11.))
                    .child(path.path.clone()),
            );
            if excluded {
                r = r.child(
                    div()
                        .text_size(px(10.5))
                        .text_color(p.muted)
                        .child("excluded"),
                );
            } else if can {
                let me = cx.entity().downgrade();
                let chosen = path.send_every;
                r = r.child(
                    Button::new(SharedString::from(format!("sync-path-send-{i}")))
                        .small()
                        .outline()
                        .label(format!("{} ▾", send_label(chosen)))
                        .dropdown_menu(move |mut menu, _, cx| {
                            crate::ui::modal(cx);
                            let options =
                                std::iter::once(None).chain(SEND_CHOICES.iter().map(|n| Some(*n)));
                            for value in options {
                                let me = me.clone();
                                menu = menu.item(
                                    PopupMenuItem::new(send_label(value))
                                        .checked(value == chosen)
                                        .on_click(move |_, _, cx| {
                                            let _ = me.update(cx, |v, cx| {
                                                v.change(
                                                    move |input| input.paths[i].send_every = value,
                                                    cx,
                                                )
                                            });
                                        }),
                                );
                            }
                            menu
                        }),
                );
                r = r.child(
                    Checkbox::new(SharedString::from(format!("sync-path-sensitive-{i}")))
                        .label("Sensitive")
                        .checked(path.sensitive)
                        .on_click(cx.listener(move |v, b: &bool, _, cx| {
                            let b = *b;
                            v.change(move |input| input.paths[i].sensitive = b, cx)
                        })),
                );
            } else {
                let mut parts = vec![];
                if path.send_every.is_some() {
                    parts.push(format!(
                        "sent {}",
                        send_label(path.send_every).to_lowercase()
                    ));
                }
                parts.push(if path.sensitive {
                    "sensitive".into()
                } else {
                    "not sensitive".into()
                });
                r = r.child(
                    div()
                        .text_size(px(10.5))
                        .text_color(p.muted)
                        .child(parts.join(" · ")),
                );
            }
            if can {
                r = r.child(
                    div()
                        .id(SharedString::from(format!("sync-path-remove-{i}")))
                        .px(px(4.))
                        .cursor_pointer()
                        .text_color(p.muted)
                        .hover(|s| s.text_color(p.text))
                        .child("×")
                        .on_click(cx.listener(move |v, _, _, cx| {
                            v.change(
                                move |input| {
                                    input.paths.remove(i);
                                },
                                cx,
                            )
                        })),
                );
            }
            d = d.child(r);
        }
        if k.paths.is_empty() {
            d = d.child(note("It shares no paths yet.", p));
        }
        if can && let Some(input) = &self.new_path {
            d = d.child(
                h_flex()
                    .gap(px(6.))
                    .child(div().flex_1().min_w_0().child(Input::new(input).small()))
                    .child(
                        button("sync-path-add", "Add a Path", p)
                            .on_click(cx.listener(|v, _, _, cx| v.add_path(cx))),
                    ),
            );
        }
        d = d.child(note(
            "A file written in bursts is sent to your linked places at most once a second, the latest each time, unless its path, or the directory holding it, says otherwise.",
            p,
        ));

        if !entry.builtin {
            d = d.child(heading("Attaches to", p));
            for (value, label, sub) in [
                ("owner", "All your environments", "self-attaching"),
                (
                    "listed",
                    "Only environments whose template lists this pack",
                    "",
                ),
            ] {
                let on = k.attach == value;
                let mut o = h_flex()
                    .id(SharedString::from(format!("sync-attach-{value}")))
                    .max_w(px(420.))
                    .gap(px(8.))
                    .px(px(9.))
                    .py(px(5.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(if on { p.accent } else { p.border })
                    .when(on, |d| d.bg(p.accent_soft))
                    .child(div().flex_1().child(label))
                    .when(!sub.is_empty(), |d| {
                        d.child(div().text_size(px(11.)).text_color(p.muted).child(sub))
                    });
                if can && !on {
                    o = o
                        .cursor_pointer()
                        .hover(|s| s.bg(p.surface_2))
                        .on_click(cx.listener(move |v, _, _, cx| {
                            v.change(move |input| input.attach = Some(value.into()), cx)
                        }));
                }
                d = d.child(o);
            }
        }

        d = d.child(heading("Copies", p));
        let default = k.default_copy_id.clone().or_else(|| shown.own.clone());
        for c in &shown.copies {
            let name = if c.personal {
                "Your own".to_string()
            } else {
                c.name.clone()
            };
            let is_default = default.as_deref() == Some(c.id.as_str());
            d =
                d.child(
                    row(p)
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(name))
                        .child(div().flex_1())
                        .child(div().text_size(px(10.5)).text_color(p.muted).child(
                            if is_default {
                                "used unless an environment selects another copy"
                            } else {
                                ""
                            },
                        )),
                );
        }

        d = d.child(heading("Syncs to", p));
        // The places it may be for: those linked that allow personal links.
        let linked: Vec<(String, String)> = self
            .held
            .iter()
            .filter(|h| h.allowed && h.linked)
            .filter_map(|h| Some((h.name.clone(), h.place_id.clone()?)))
            .collect();
        let chosen: Vec<String> = k.places.clone();
        let mut places = h_flex().gap(px(14.)).flex_wrap();
        for h in &self.held {
            let id = h.place_id.clone();
            let on = id
                .as_ref()
                .is_some_and(|id| chosen.is_empty() || chosen.contains(id));
            let why = if h.packs.is_err() {
                Some("not read")
            } else if !h.allowed {
                Some("not allowed by its administrators")
            } else if !h.linked {
                Some("not linked")
            } else {
                None
            };
            let mut row = h_flex().gap(px(4.));
            match (
                why,
                id,
                edit.as_ref().filter(|_| !entry.builtin && !self.busy),
            ) {
                (None, Some(id), Some(at)) => {
                    let (all, chosen, at) = (linked.clone(), chosen.clone(), at.clone());
                    let pack = k.id.clone();
                    row = row.child(
                        Checkbox::new(SharedString::from(format!("sync-place-{id}")))
                            .label(h.name.clone())
                            .checked(on)
                            .on_click(cx.listener(move |v, b: &bool, _, cx| {
                                // Every place, or those ticked.
                                let mut now: Vec<String> = if chosen.is_empty() {
                                    all.iter().map(|(_, i)| i.clone()).collect()
                                } else {
                                    chosen.clone()
                                };
                                now.retain(|x| x != &id);
                                if *b {
                                    now.push(id.clone());
                                }
                                if all.iter().all(|(_, i)| now.contains(i)) {
                                    now.clear();
                                }
                                let Some(c) = v.client(&at.place, cx) else {
                                    return;
                                };
                                let pack = pack.clone();
                                v.act(cx, move || {
                                    c.set_pack_places(&pack, &now)
                                        .map(|_| ())
                                        .map_err(|e| e.to_string())
                                });
                            })),
                    );
                }
                _ => {
                    row = row.child(h.name.clone()).child(
                        div()
                            .text_color(if on && why.is_none() { p.good } else { p.muted })
                            .child(
                                why.map_or(if on { "✓" } else { "kept away" }, |w| w)
                                    .to_string(),
                            ),
                    );
                }
            }
            places = places.child(row);
        }
        d = d.child(places);
        if !entry.builtin {
            d = d.child(note(
                "A place left out holds the pack's name and paths, but not the pack: it is hidden there, and its files are removed from it and no more sent.",
                p,
            ));
        }

        let history: Vec<(Detail, types::ReplacedFile)> = self
            .details
            .iter()
            .flat_map(|x| x.history.iter().map(move |r| (x.clone(), r.clone())))
            .collect();
        if !history.is_empty() {
            d = d.child(heading("Replaced by another place", p));
            for (i, (x, r)) in history.into_iter().enumerate() {
                let (place, copy, entry_id) = (x.place.clone(), x.own.clone(), r.id.clone());
                d = d.child(
                    row(p)
                        .child(
                            div()
                                .flex_none()
                                .font_family(crate::diff::mono())
                                .text_size(px(11.))
                                .child(r.path.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(10.5))
                                .text_color(p.muted)
                                .child(format!(
                                    "{} · kept until {}",
                                    replaced_by(&r, &x.name),
                                    day(r.kept_until)
                                )),
                        )
                        .child(if self.busy {
                            disabled(
                                SharedString::from(format!("sync-restore-{i}")),
                                "Restore",
                                p,
                            )
                        } else {
                            button(
                                SharedString::from(format!("sync-restore-{i}")),
                                "Restore",
                                p,
                            )
                            .on_click(cx.listener(
                                move |v, _, _, cx| {
                                    let (Some(c), Some(copy)) =
                                        (v.client(&place, cx), copy.clone())
                                    else {
                                        return;
                                    };
                                    let entry_id = entry_id.clone();
                                    v.act(cx, move || {
                                        c.restore_copy_file(&copy, &entry_id)
                                            .map_err(|e| e.to_string())
                                    });
                                },
                            ))
                        }),
                );
            }
            d = d.child(note(
                "When a file changed in two places at once, the newer change is kept everywhere and the other is kept where it was for 7 days.",
                p,
            ));
        }
        d.into_any_element()
    }
}
