//! Settings › Sync › Templates: the person's own templates, each once,
//! with the places it is in. A person's own template is the same in every
//! place they link: changed in one, it changes in the rest. The chosen
//! one is summed up, and opens in a place's Templates page to change.

use std::time::Duration;

use gpui::{
    Context, Entity, EventEmitter, IntoElement, ParentElement, Render, SharedString, Styled, Task,
    Window, div, prelude::*, px,
};
use gpui_component::{h_flex, v_flex};
use hangar_openapi::clientapi::types;

use crate::form::{self, chip, field, item, note, section, split};
use crate::palette::{self, Palette};
use crate::places::Places;
use crate::ui::button;

const EVERY: Duration = Duration::from_secs(20);

/// What the page asks of the settings window.
pub enum SyncTemplatesEvent {
    /// Show a place's Templates page.
    Open(String),
}

/// One place's templates of the person's own.
#[derive(Clone)]
struct Held {
    place: String,
    name: String,
    templates: Result<Vec<types::Template>, String>,
}

/// A template as the page lists it: the places it is in, and how the first
/// place has it.
#[derive(Clone)]
struct Entry {
    id: String,
    template: types::Template,
    places: Vec<(String, String)>,
}

fn entries(held: &[Held]) -> Vec<Entry> {
    let mut out: Vec<Entry> = vec![];
    for h in held {
        let Ok(ts) = &h.templates else { continue };
        for t in ts {
            match out.iter_mut().find(|e| e.id == t.id) {
                Some(e) => e.places.push((h.place.clone(), h.name.clone())),
                None => out.push(Entry {
                    id: t.id.clone(),
                    template: t.clone(),
                    places: vec![(h.place.clone(), h.name.clone())],
                }),
            }
        }
    }
    out.sort_by(|a, b| {
        a.template
            .name
            .to_lowercase()
            .cmp(&b.template.name.to_lowercase())
    });
    out
}

pub struct SyncTemplatesView {
    places: Entity<Places>,
    held: Vec<Held>,
    loaded: bool,
    selected: Option<String>,
    _poll: Task<()>,
}

impl EventEmitter<SyncTemplatesEvent> for SyncTemplatesView {}

impl SyncTemplatesView {
    pub fn new(places: Entity<Places>, cx: &mut Context<Self>) -> Self {
        let poll = cx.spawn(async move |this, cx| {
            loop {
                if this.update(cx, |v, cx| v.load(cx)).is_err() {
                    return;
                }
                cx.background_executor().timer(EVERY).await;
            }
        });
        SyncTemplatesView {
            places,
            held: vec![],
            loaded: false,
            selected: None,
            _poll: poll,
        }
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let places = self.places.read(cx);
        let sources: Vec<(String, String, Option<crate::api::Client>, Option<String>)> = places
            .list()
            .iter()
            .map(|(c, s)| {
                (
                    c.id.clone(),
                    c.name.clone(),
                    places.client(&c.id),
                    s.me.as_ref().map(|m| m.username.clone()),
                )
            })
            .collect();
        let task = cx.background_spawn(async move {
            std::thread::scope(|s| {
                let reads: Vec<_> = sources
                    .into_iter()
                    .map(|(place, name, client, me)| {
                        s.spawn(move || Held {
                            templates: match (client, me) {
                                (Some(c), Some(me)) => c
                                    .templates()
                                    .map(|ts| {
                                        ts.into_iter()
                                            .filter(|t| t.owner == me && t.team.is_none())
                                            .collect()
                                    })
                                    .map_err(|e| e.to_string()),
                                _ => Err("Sign in to read its templates.".into()),
                            },
                            place,
                            name,
                        })
                    })
                    .collect();
                reads
                    .into_iter()
                    .filter_map(|r| r.join().ok())
                    .collect::<Vec<_>>()
            })
        });
        cx.spawn(async move |this, cx| {
            let held = task.await;
            let _ = this.update(cx, |v, cx| {
                v.held = held;
                v.loaded = true;
                let list = entries(&v.held);
                if v.selected
                    .as_ref()
                    .is_none_or(|s| !list.iter().any(|e| &e.id == s))
                {
                    v.selected = list.first().map(|e| e.id.clone());
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for SyncTemplatesView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        if !self.loaded {
            return note("Reading your places…", &p).into_any_element();
        }
        let list = entries(&self.held);
        let mut left = v_flex().gap(px(4.));
        for e in &list {
            let id = e.id.clone();
            left = left.child(
                item(
                    SharedString::from(format!("sync-template-{}", e.id)),
                    e.template.name.clone(),
                    "yours",
                    Some(
                        h_flex()
                            .gap(px(3.))
                            .flex_wrap()
                            .children(e.places.iter().map(|(_, n)| chip(n.clone(), &p)))
                            .into_any_element(),
                    ),
                    self.selected.as_ref() == Some(&e.id),
                    &p,
                )
                .on_click(cx.listener(move |v, _, _, cx| {
                    v.selected = Some(id.clone());
                    cx.notify()
                })),
            );
        }
        if list.is_empty() {
            left = left.child(note("You have no templates of your own yet.", &p));
        }
        let right = match list.iter().find(|e| self.selected.as_ref() == Some(&e.id)) {
            Some(e) => self.detail(e, &p, cx).into_any_element(),
            None => note(
                "Make a template under any place's Templates; it reaches your linked places.",
                &p,
            )
            .into_any_element(),
        };
        let unread: Vec<String> = self
            .held
            .iter()
            .filter_map(|h| {
                h.templates
                    .as_ref()
                    .err()
                    .map(|e| format!("{}: {e}", h.name))
            })
            .collect();
        v_flex()
            .gap(px(8.))
            .child(note(
                "Your own templates, each once, with the places each is in. One changed in a place changes in every linked place; a team's templates stay in their place.",
                &p,
            ))
            .child(split(left, right))
            .when(!unread.is_empty(), |d| d.child(note(unread.join("\n"), &p)))
            .into_any_element()
    }
}

impl SyncTemplatesView {
    fn detail(&self, e: &Entry, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let t = &e.template;
        let mut out = v_flex()
            .gap(px(6.))
            .child(section("Template", p))
            .when(!t.description.is_empty(), |d| {
                d.child(field("Description", p).child(div().child(t.description.clone())))
            })
            .child(field("Image", p).child(form::mono(t.image.clone())))
            .child(field("Machine", p).child(div().child(format!(
                "{} vCPUs, {:.0} GiB, {}{}",
                t.cpus,
                t.memory_mib as f64 / 1024.,
                if t.display == "desktop" {
                    "with a desktop"
                } else {
                    "headless"
                },
                if t.gpu != "none" {
                    format!(", {} GPU", t.gpu)
                } else {
                    String::new()
                }
            ))));
        for r in &t.repos {
            out = out.child(field("Clones", p).child(form::mono(r.url.to_string())));
        }
        out = out.child(section("In your places", p));
        for (place, name) in &e.places {
            let place = place.clone();
            out = out.child(
                h_flex()
                    .gap(px(8.))
                    .child(div().w(px(160.)).child(name.clone()))
                    .child(
                        button(
                            SharedString::from(format!("sync-template-open-{place}")),
                            "Change It Here",
                            p,
                        )
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(SyncTemplatesEvent::Open(place.clone()))
                        })),
                    ),
            );
        }
        let missing: Vec<&str> = self
            .held
            .iter()
            .filter(|h| h.templates.is_ok() && !e.places.iter().any(|(pl, _)| pl == &h.place))
            .map(|h| h.name.as_str())
            .collect();
        if !missing.is_empty() {
            out = out.child(note(
                format!(
                    "Not in {}: a place reaches it once linked with one that has it.",
                    missing.join(", ")
                ),
                p,
            ));
        }
        out
    }
}
