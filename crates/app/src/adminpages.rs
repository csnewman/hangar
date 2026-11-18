//! Settings › a place › Users, Workers and Audit Log: the web UI's
//! administration in the app, for a place's administrators. Users are
//! made, made administrators, disabled, given a password and deleted;
//! workers are shown with their machine, labels, GPU, images and the
//! environments the server does not know, their recent log, and revoked
//! or deleted; the audit log is read a page at a time, newest first.

use std::time::Duration;

use gpui::{
    Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled, Task, Window, div,
    prelude::*, px,
};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputState};
use gpui_component::{Sizable, h_flex, v_flex};
use hangar_openapi::clientapi::types;
use serde_json::json;

use crate::api::Client;
use crate::form::{self, field, input, item, note, section, split, text, twice};
use crate::palette::{self, Palette};
use crate::places::Places;
use crate::ui::{button, disabled, primary};

const EVERY: Duration = Duration::from_secs(20);

fn when(at: chrono::DateTime<chrono::Utc>) -> String {
    at.with_timezone(&chrono::Local)
        .format("%-d %b %Y %H:%M")
        .to_string()
}

fn gib(mib: i64) -> String {
    format!("{:.1} GiB", mib as f64 / 1024.)
}

fn bytes(n: i64) -> String {
    match n {
        n if n >= 1_000_000_000 => format!("{:.1} GB", n as f64 / 1e9),
        n if n >= 1_000_000 => format!("{:.1} MB", n as f64 / 1e6),
        n => format!("{:.1} kB", n as f64 / 1e3),
    }
}

/// Polls a place's page every EVERY, from the start.
fn poll<V: 'static>(cx: &mut Context<V>, load: fn(&mut V, &mut Context<V>)) -> Task<()> {
    cx.spawn(async move |this, cx| {
        loop {
            if this.update(cx, load).is_err() {
                return;
            }
            cx.background_executor().timer(EVERY).await;
        }
    })
}

/// Users.
pub struct UsersView {
    places: Entity<Places>,
    place: String,
    users: Vec<types::User>,
    loaded: bool,
    selected: Option<String>,
    username: Option<Entity<InputState>>,
    display: Option<Entity<InputState>>,
    password: Option<Entity<InputState>>,
    make_admin: bool,
    busy: bool,
    error: Option<String>,
    saved: Option<String>,
    armed: bool,
    _poll: Task<()>,
}

impl UsersView {
    pub fn new(places: Entity<Places>, place: &str, cx: &mut Context<Self>) -> Self {
        let p = poll(cx, Self::load);
        UsersView {
            places,
            place: place.to_string(),
            users: vec![],
            loaded: false,
            selected: None,
            username: None,
            display: None,
            password: None,
            make_admin: false,
            busy: false,
            error: None,
            saved: None,
            armed: false,
            _poll: p,
        }
    }

    fn client(&self, cx: &gpui::App) -> Option<Client> {
        self.places.read(cx).client(&self.place)
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(c) = self.client(cx) else { return };
        let task = cx.background_spawn(async move { c.users() });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |v, cx| {
                v.loaded = true;
                match r {
                    Ok(u) => {
                        v.users = u;
                        if v.selected.is_none() {
                            v.selected = v.users.first().map(|u| u.id.clone());
                        }
                    }
                    Err(e) => v.error = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn select(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        self.selected = id;
        self.username = None;
        self.display = None;
        self.password = None;
        self.armed = false;
        self.saved = None;
        self.error = None;
        cx.notify();
    }

    fn act(
        &mut self,
        saved: &str,
        cx: &mut Context<Self>,
        f: impl FnOnce() -> Result<Option<String>, String> + Send + 'static,
    ) {
        self.busy = true;
        self.error = None;
        cx.notify();
        let saved = saved.to_string();
        let task = cx.background_spawn(async move { f() });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |v, cx| {
                v.busy = false;
                v.armed = false;
                match r {
                    Ok(next) => {
                        if let Some(n) = next {
                            v.select(if n.is_empty() { None } else { Some(n) }, cx);
                        }
                        v.saved = Some(saved);
                    }
                    Err(e) => v.error = Some(e),
                }
                v.load(cx);
            });
        })
        .detach();
    }
}

impl Render for UsersView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        if !self.loaded {
            return note("Reading the place's users…", &p).into_any_element();
        }
        let user = self
            .selected
            .as_ref()
            .and_then(|s| self.users.iter().find(|u| &u.id == s))
            .cloned();
        let making = self.selected.as_deref() == Some("");
        if self.display.is_none() && (making || user.is_some()) {
            self.username = Some(input("", "username", window, cx));
            self.display = Some(input(
                &user
                    .as_ref()
                    .map_or(String::new(), |u| u.display_name.clone()),
                "Full name",
                window,
                cx,
            ));
            self.password = Some(cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder("at least 8 characters")
            }));
        }
        let mut left = v_flex().gap(px(4.));
        for u in &self.users {
            let id = u.id.clone();
            let mut aside = vec![];
            if u.admin {
                aside.push("admin");
            }
            if u.disabled {
                aside.push("disabled");
            }
            left = left.child(
                item(
                    SharedString::from(format!("user-{}", u.id)),
                    u.username.clone(),
                    aside.join(", "),
                    Some(
                        form::small(
                            format!(
                                "{}{} environment{}",
                                if u.display_name.is_empty() {
                                    String::new()
                                } else {
                                    format!("{} · ", u.display_name)
                                },
                                u.environments,
                                if u.environments == 1 { "" } else { "s" }
                            ),
                            &p,
                        )
                        .into_any_element(),
                    ),
                    self.selected.as_deref() == Some(u.id.as_str()),
                    &p,
                )
                .on_click(cx.listener(move |v, _, _, cx| v.select(Some(id.clone()), cx))),
            );
        }
        left = left.child(
            button("user-new", "New User", &p)
                .on_click(cx.listener(|v, _, _, cx| v.select(Some(String::new()), cx))),
        );
        let right = if making || user.is_some() {
            self.detail(user.as_ref(), &p, cx).into_any_element()
        } else {
            note("Choose a user, or make one.", &p).into_any_element()
        };
        v_flex()
            .gap(px(8.))
            .child(note("Who signs in to this place. A disabled user cannot sign in; their environments are kept.", &p))
            .child(split(left, right))
            .when_some(self.error.clone(), |d, e| d.child(form::error(e, &p)))
            .into_any_element()
    }
}

impl UsersView {
    fn detail(
        &self,
        user: Option<&types::User>,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (Some(username), Some(display), Some(password)) =
            (&self.username, &self.display, &self.password)
        else {
            return v_flex();
        };
        let mut out = v_flex().gap(px(6.)).child(section("User", p));
        match user {
            None => {
                out = out
                    .child(
                        field("Username", p)
                            .child(div().flex_1().child(Input::new(username).small())),
                    )
                    .child(
                        field("Full name", p)
                            .child(div().flex_1().child(Input::new(display).small())),
                    )
                    .child(
                        field("Password", p)
                            .child(div().flex_1().child(Input::new(password).small())),
                    )
                    .child(
                        Checkbox::new("user-new-admin")
                            .label("An administrator")
                            .checked(self.make_admin)
                            .on_click(cx.listener(|v, b: &bool, _, cx| {
                                v.make_admin = *b;
                                cx.notify()
                            })),
                    )
                    .child(h_flex().child(if self.busy {
                        disabled("user-make", "Make User", p)
                    } else {
                        primary("user-make", "Make User", p).on_click(cx.listener(|v, _, _, cx| {
                            let (Some(u), Some(d), Some(pw), Some(c)) = (
                                v.username.clone(),
                                v.display.clone(),
                                v.password.clone(),
                                v.client(cx),
                            ) else {
                                return;
                            };
                            let body = json!({
                                "username": text(&u, cx),
                                "display_name": text(&d, cx),
                                "password": pw.read(cx).value().to_string(),
                                "admin": v.make_admin,
                            });
                            v.act("Made.", cx, move || {
                                c.create_user(body)
                                    .map(|u| Some(u.id))
                                    .map_err(|e| e.to_string())
                            });
                        }))
                    }));
            }
            Some(u) => {
                let id = u.id.clone();
                out = out
                    .child(field("Username", p).child(div().child(u.username.clone())))
                    .child(field("Made", p).child(div().child(when(u.created_at))))
                    .child(
                        field("Signs in with", p).child(div().child(if u.has_password {
                            "a password, or the place's sign-in"
                        } else {
                            "the place's sign-in"
                        })),
                    )
                    .child(
                        field("Full name", p)
                            .child(div().flex_1().child(Input::new(display).small())),
                    )
                    .child(
                        h_flex().child(button("user-save-name", "Save Name", p).on_click(
                            cx.listener({
                                let id = id.clone();
                                move |v, _, _, cx| {
                                    let (Some(d), Some(c)) = (v.display.clone(), v.client(cx))
                                    else {
                                        return;
                                    };
                                    let body = json!({ "display_name": text(&d, cx) });
                                    let id = id.clone();
                                    v.act("Saved.", cx, move || {
                                        c.update_user(&id, body)
                                            .map(|_| None)
                                            .map_err(|e| e.to_string())
                                    });
                                }
                            }),
                        )),
                    )
                    .child(
                        Checkbox::new("user-admin")
                            .label("An administrator")
                            .checked(u.admin)
                            .on_click(cx.listener({
                                let id = id.clone();
                                move |v, b: &bool, _, cx| {
                                    let Some(c) = v.client(cx) else { return };
                                    let (id, body) = (id.clone(), json!({ "admin": *b }));
                                    v.act("Saved.", cx, move || {
                                        c.update_user(&id, body)
                                            .map(|_| None)
                                            .map_err(|e| e.to_string())
                                    });
                                }
                            })),
                    )
                    .child(
                        Checkbox::new("user-disabled")
                            .label("Disabled: cannot sign in")
                            .checked(u.disabled)
                            .on_click(cx.listener({
                                let id = id.clone();
                                move |v, b: &bool, _, cx| {
                                    let Some(c) = v.client(cx) else { return };
                                    let (id, body) = (id.clone(), json!({ "disabled": *b }));
                                    v.act("Saved.", cx, move || {
                                        c.update_user(&id, body)
                                            .map(|_| None)
                                            .map_err(|e| e.to_string())
                                    });
                                }
                            })),
                    )
                    .child(section("Password", p))
                    .child(
                        field("New password", p)
                            .child(div().flex_1().child(Input::new(password).small())),
                    )
                    .child(
                        h_flex().child(button("user-password", "Set Password", p).on_click(
                            cx.listener({
                                let id = id.clone();
                                move |v, _, window, cx| {
                                    let (Some(pw), Some(c)) = (v.password.clone(), v.client(cx))
                                    else {
                                        return;
                                    };
                                    let body =
                                        json!({ "password": pw.read(cx).value().to_string() });
                                    pw.update(cx, |s, cx| s.set_value("", window, cx));
                                    let id = id.clone();
                                    v.act("Password set.", cx, move || {
                                        c.update_user(&id, body)
                                            .map(|_| None)
                                            .map_err(|e| e.to_string())
                                    });
                                }
                            }),
                        )),
                    )
                    .child(
                        h_flex()
                            .pt(px(8.))
                            .gap(px(6.))
                            .when_some(self.saved.clone(), |d, s| d.child(form::small(s, p)))
                            .child(div().flex_1())
                            .child(
                                twice(
                                    "user-delete",
                                    "Delete User…",
                                    "Delete them and their environments?",
                                    self.armed,
                                    self.busy,
                                    p,
                                )
                                .on_click(cx.listener(
                                    move |v, _, _, cx| {
                                        if !v.armed {
                                            v.armed = true;
                                            cx.notify();
                                            return;
                                        }
                                        let Some(c) = v.client(cx) else { return };
                                        let id = id.clone();
                                        v.act("Deleted.", cx, move || {
                                            c.delete_user(&id)
                                                .map(|_| Some(String::new()))
                                                .map_err(|e| e.to_string())
                                        });
                                    },
                                )),
                            ),
                    );
            }
        }
        out
    }
}

/// Workers.
pub struct WorkersView {
    places: Entity<Places>,
    place: String,
    workers: Vec<types::Worker>,
    loaded: bool,
    selected: Option<String>,
    log: Option<String>,
    busy: bool,
    error: Option<String>,
    armed: Option<String>,
    _poll: Task<()>,
}

impl WorkersView {
    pub fn new(places: Entity<Places>, place: &str, cx: &mut Context<Self>) -> Self {
        let p = poll(cx, Self::load);
        WorkersView {
            places,
            place: place.to_string(),
            workers: vec![],
            loaded: false,
            selected: None,
            log: None,
            busy: false,
            error: None,
            armed: None,
            _poll: p,
        }
    }

    fn client(&self, cx: &gpui::App) -> Option<Client> {
        self.places.read(cx).client(&self.place)
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(c) = self.client(cx) else { return };
        let task = cx.background_spawn(async move { c.workers() });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |v, cx| {
                v.loaded = true;
                match r {
                    Ok(w) => {
                        v.workers = w;
                        if v.selected.is_none() {
                            v.selected = v.workers.first().map(|w| w.id.clone());
                        }
                    }
                    Err(e) => v.error = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn act(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce() -> Result<(), String> + Send + 'static,
    ) {
        self.busy = true;
        self.error = None;
        self.armed = None;
        cx.notify();
        let task = cx.background_spawn(async move { f() });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |v, cx| {
                v.busy = false;
                if let Err(e) = r {
                    v.error = Some(e);
                }
                v.load(cx);
            });
        })
        .detach();
    }

    fn armed(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        if self.armed.as_deref() == Some(key) {
            return true;
        }
        self.armed = Some(key.to_string());
        cx.notify();
        false
    }

    fn read_log(&mut self, cx: &mut Context<Self>) {
        let (Some(id), Some(c)) = (self.selected.clone(), self.client(cx)) else {
            return;
        };
        let task = cx.background_spawn(async move { c.worker_log(&id, None) });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |v, cx| {
                match r {
                    Ok(l) => {
                        // The latest lines, as a page shows.
                        let lines: Vec<&str> = l.text.lines().collect();
                        let from = lines.len().saturating_sub(200);
                        v.log = Some(lines[from..].join("\n"));
                    }
                    Err(e) => v.error = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for WorkersView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        if !self.loaded {
            return note("Reading the place's workers…", &p).into_any_element();
        }
        let mut left = v_flex().gap(px(4.));
        for w in &self.workers {
            let id = w.id.clone();
            let state = if w.revoked {
                "revoked"
            } else if w.online {
                "online"
            } else {
                "offline"
            };
            left = left.child(
                item(
                    SharedString::from(format!("worker-{}", w.id)),
                    w.name.clone(),
                    state,
                    Some(
                        form::small(
                            format!(
                                "{} vCPUs · {} · {}",
                                w.capacity.cpus,
                                gib(w.capacity.memory_mib),
                                w.version.clone().unwrap_or_default()
                            ),
                            &p,
                        )
                        .into_any_element(),
                    ),
                    self.selected.as_deref() == Some(w.id.as_str()),
                    &p,
                )
                .on_click(cx.listener(move |v, _, _, cx| {
                    v.selected = Some(id.clone());
                    v.log = None;
                    v.armed = None;
                    cx.notify()
                })),
            );
        }
        if self.workers.is_empty() {
            left = left.child(note("No worker has enrolled.", &p));
        }
        let w = self
            .selected
            .as_ref()
            .and_then(|s| self.workers.iter().find(|w| &w.id == s))
            .cloned();
        let right = match &w {
            Some(w) => self.detail(w, &p, cx).into_any_element(),
            None => note("Choose a worker.", &p).into_any_element(),
        };
        v_flex()
            .gap(px(8.))
            .child(note("The machines this place's environments run on.", &p))
            .child(split(left, right))
            .when_some(self.error.clone(), |d, e| d.child(form::error(e, &p)))
            .into_any_element()
    }
}

impl WorkersView {
    fn detail(&self, w: &types::Worker, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let mut out = v_flex()
            .gap(px(5.))
            .child(section("Worker", p))
            .child(field("State", p).child(div().child(if w.revoked {
                "Revoked".to_string()
            } else if w.online {
                "Online".to_string()
            } else {
                w.last_seen_at.map_or("Offline".into(), |at| {
                    format!("Offline, last seen {}", when(at))
                })
            })))
            .child(
                field("Version", p)
                    .child(div().child(w.version.clone().unwrap_or_else(|| "not said yet".into()))),
            )
            .child(
                field("Runs", p).child(
                    div().child(
                        w.monitor
                            .as_ref()
                            .map_or("not said yet".into(), |m| m.to_string()),
                    ),
                ),
            )
            .child(field("Capacity", p).child(div().child(format!(
                "{} vCPUs, {}",
                w.capacity.cpus,
                gib(w.capacity.memory_mib)
            ))))
            .child(field("Allocated", p).child(div().child(format!(
                "{} vCPUs, {}",
                w.allocated.cpus,
                gib(w.allocated.memory_mib)
            ))));
        let mut labels: Vec<String> = w.labels.iter().map(|(k, v)| format!("{k}={v}")).collect();
        labels.sort();
        out = out
            .child(
                field("Labels", p).child(
                    h_flex()
                        .gap(px(4.))
                        .flex_wrap()
                        .children(labels.into_iter().map(|l| form::chip(l, p))),
                ),
            )
            .child(field("Features", p).child(div().child(w.features.join(", "))));
        if let Some(g) = &w.gpu {
            let what = g
                .unavailable
                .clone()
                .or_else(|| g.device.clone())
                .unwrap_or_else(|| g.renderer.to_string());
            out = out.child(field("GPU", p).child(div().child(what)));
        }
        if let Some(s) = &w.stats {
            out = out
                .child(
                    field("CPU", p)
                        .child(div().child(format!("{:.0}% · load {:.2}", s.cpu_percent, s.load1))),
                )
                .child(field("Memory", p).child(div().child(format!(
                    "{} of {} used · {} in guests",
                    gib(s.memory_used_mib),
                    gib(s.memory_total_mib),
                    gib(s.guest_memory_mib)
                ))))
                .child(field("Disk", p).child(div().child(format!(
                    "{} of {}",
                    bytes(s.disk_used_bytes),
                    bytes(s.disk_total_bytes)
                ))));
        }

        out = out.child(section("Images", p));
        if w.images.is_empty() {
            out = out.child(note("No image copies.", p));
        }
        for img in &w.images {
            let key = format!("image-{}-{}", img.ref_, img.digest);
            let copy: Option<types::ImageCopy> =
                serde_json::from_value(json!({ "ref": img.ref_, "digest": img.digest })).ok();
            let unused = img.environments.is_empty();
            out = out.child(
                form::row(p)
                    .child(form::mono(img.ref_.clone()).flex_1().min_w_0().truncate())
                    .child(form::small(
                        format!(
                            "{} · {}{} · {} environment{}",
                            bytes(img.size_bytes),
                            img.state,
                            if img.current { ", current" } else { "" },
                            img.environments.len(),
                            if img.environments.len() == 1 { "" } else { "s" }
                        ),
                        p,
                    ))
                    .when(unused && copy.is_some(), |d| {
                        d.child(
                            twice(
                                SharedString::from(key.clone()),
                                "Remove…",
                                "Remove the copy?",
                                self.armed.as_deref() == Some(key.as_str()),
                                self.busy,
                                p,
                            )
                            .on_click(cx.listener({
                                let id = w.id.clone();
                                move |v, _, _, cx| {
                                    if !v.armed(&key, cx) {
                                        return;
                                    }
                                    let (Some(c), Some(copy)) = (v.client(cx), copy.clone()) else {
                                        return;
                                    };
                                    let id = id.clone();
                                    v.act(cx, move || {
                                        c.remove_worker_image(&id, &copy).map_err(|e| e.to_string())
                                    });
                                }
                            })),
                        )
                    }),
            );
        }
        if !w.unknown.is_empty() {
            let id = w.id.clone();
            let envs = w.unknown.clone();
            out = out
                .child(section("Environments the server does not know", p))
                .child(note(
                    format!(
                        "{} on the worker, left from before: {}",
                        envs.len(),
                        envs.join(", ")
                    ),
                    p,
                ))
                .child(
                    h_flex().child(
                        twice(
                            "worker-unknown",
                            "Remove Them…",
                            "Delete them from the worker?",
                            self.armed.as_deref() == Some("unknown"),
                            self.busy,
                            p,
                        )
                        .on_click(cx.listener(move |v, _, _, cx| {
                            if !v.armed("unknown", cx) {
                                return;
                            }
                            let Some(c) = v.client(cx) else { return };
                            let (id, envs) = (id.clone(), envs.clone());
                            v.act(cx, move || {
                                c.remove_unknown(&id, &envs).map_err(|e| e.to_string())
                            });
                        })),
                    ),
                );
        }

        out = out.child(section("Log", p)).child(
            h_flex().child(
                button(
                    "worker-log",
                    if self.log.is_some() {
                        "Read Again"
                    } else {
                        "Read Its Log"
                    },
                    p,
                )
                .on_click(cx.listener(|v, _, _, cx| v.read_log(cx))),
            ),
        );
        if let Some(log) = &self.log {
            out = out.child(
                div()
                    .id("worker-log-text")
                    .max_h(px(260.))
                    .overflow_y_scroll()
                    .p(px(6.))
                    .rounded(px(5.))
                    .bg(p.term_bg)
                    .text_color(p.term_fg)
                    .font_family(crate::diff::mono())
                    .text_size(px(10.5))
                    .child(log.clone()),
            );
        }
        let id = w.id.clone();
        let id2 = w.id.clone();
        out.child(
            h_flex()
                .pt(px(8.))
                .gap(px(6.))
                .child(div().flex_1())
                .when(!w.revoked, |d| {
                    d.child(
                        twice(
                            "worker-revoke",
                            "Revoke…",
                            "Revoke its credential? It stops running environments.",
                            self.armed.as_deref() == Some("revoke"),
                            self.busy,
                            p,
                        )
                        .on_click(cx.listener(move |v, _, _, cx| {
                            if !v.armed("revoke", cx) {
                                return;
                            }
                            let Some(c) = v.client(cx) else { return };
                            let id = id.clone();
                            v.act(cx, move || c.revoke_worker(&id).map_err(|e| e.to_string()));
                        })),
                    )
                })
                .child(
                    twice(
                        "worker-delete",
                        "Delete…",
                        "Delete the worker's record?",
                        self.armed.as_deref() == Some("delete"),
                        self.busy,
                        p,
                    )
                    .on_click(cx.listener(move |v, _, _, cx| {
                        if !v.armed("delete", cx) {
                            return;
                        }
                        let Some(c) = v.client(cx) else { return };
                        let id = id2.clone();
                        v.selected = None;
                        v.act(cx, move || c.delete_worker(&id).map_err(|e| e.to_string()));
                    })),
                ),
        )
    }
}

/// The audit log.
pub struct AuditView {
    places: Entity<Places>,
    place: String,
    events: Vec<types::AuditEvent>,
    loaded: bool,
    more: bool,
    error: Option<String>,
    _poll: Task<()>,
}

const PAGE: i64 = 50;

impl AuditView {
    pub fn new(places: Entity<Places>, place: &str, cx: &mut Context<Self>) -> Self {
        let p = poll(cx, Self::load);
        AuditView {
            places,
            place: place.to_string(),
            events: vec![],
            loaded: false,
            more: false,
            error: None,
            _poll: p,
        }
    }

    fn client(&self, cx: &gpui::App) -> Option<Client> {
        self.places.read(cx).client(&self.place)
    }

    /// The newest page, kept with what was read before it.
    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(c) = self.client(cx) else { return };
        let task = cx.background_spawn(async move { c.audit(None, PAGE) });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |v, cx| {
                v.loaded = true;
                match r {
                    Ok(page) => {
                        let newest = v.events.first().map(|e| e.id);
                        let fresh: Vec<types::AuditEvent> = page
                            .iter()
                            .filter(|e| newest.is_none_or(|n| e.id > n))
                            .cloned()
                            .collect();
                        if v.events.is_empty() {
                            v.more = page.len() as i64 == PAGE;
                            v.events = page;
                        } else {
                            v.events.splice(0..0, fresh);
                        }
                    }
                    Err(e) => v.error = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn older(&mut self, cx: &mut Context<Self>) {
        let (Some(c), Some(last)) = (self.client(cx), self.events.last().map(|e| e.id)) else {
            return;
        };
        let task = cx.background_spawn(async move { c.audit(Some(last), PAGE) });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |v, cx| {
                match r {
                    Ok(page) => {
                        v.more = page.len() as i64 == PAGE;
                        v.events.extend(page);
                    }
                    Err(e) => v.error = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for AuditView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        if !self.loaded {
            return note("Reading the audit log…", &p).into_any_element();
        }
        let mut list = v_flex().gap(px(3.));
        for e in &self.events {
            let target = if e.target.name.is_empty() {
                e.target.type_.clone()
            } else {
                format!("{} {}", e.target.type_, e.target.name)
            };
            list = list.child(
                form::row(&p)
                    .child(form::small(when(e.at), &p).w(px(120.)).flex_none())
                    .child(
                        div()
                            .w(px(110.))
                            .flex_none()
                            .truncate()
                            .child(e.actor.name.clone()),
                    )
                    .child(
                        form::mono(e.action.clone())
                            .w(px(170.))
                            .flex_none()
                            .truncate(),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(target))
                    .when_some(e.ip.clone(), |d, ip| d.child(form::small(ip, &p))),
            );
        }
        v_flex()
            .gap(px(8.))
            .child(note(
                "Everything people, workers and Hangar itself did here, newest first.",
                &p,
            ))
            .child(list)
            .when(self.more, |d| {
                d.child(
                    h_flex().child(
                        button("audit-older", "Older Events", &p)
                            .on_click(cx.listener(|v, _, _, cx| v.older(cx))),
                    ),
                )
            })
            .when_some(self.error.clone(), |d, e| d.child(form::error(e, &p)))
            .into_any_element()
    }
}
