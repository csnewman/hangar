//! Settings › a place › Teams: the place's teams the user is in (every
//! team, for an administrator), a new team, and the chosen one's name,
//! description and members, each a viewer (uses what the team owns), a
//! member (also changes it) or an admin (also runs the team).

use std::time::Duration;

use gpui::{
    Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled, Task, Window, div,
    prelude::*, px,
};
use gpui_component::input::{Input, InputState};
use gpui_component::{Sizable, h_flex, v_flex};
use hangar_openapi::clientapi::types;
use serde_json::json;

use crate::api::Client;
use crate::form::{self, choice, field, input, item, note, section, split, text, twice};
use crate::palette::{self, Palette};
use crate::places::Places;
use crate::ui::{button, disabled, primary};

const EVERY: Duration = Duration::from_secs(30);

const ROLES: [(&str, &str); 3] = [
    ("Viewer", "viewer"),
    ("Member", "member"),
    ("Admin", "admin"),
];

fn role_name(r: &str) -> &'static str {
    ROLES
        .iter()
        .find(|(_, v)| *v == r)
        .map_or("Member", |(l, _)| l)
}

struct Draft {
    name: Entity<InputState>,
    slug: Entity<InputState>,
    description: Entity<InputState>,
}

pub struct TeamsView {
    places: Entity<Places>,
    place: String,
    teams: Vec<types::Team>,
    people: Vec<types::Person>,
    loaded: bool,
    /// The team shown: its ID, or "" for one being made.
    selected: Option<String>,
    team: Option<types::Team>,
    draft: Option<Draft>,
    new_role: String,
    busy: bool,
    error: Option<String>,
    armed: Option<String>,
    _poll: Task<()>,
}

impl TeamsView {
    pub fn new(places: Entity<Places>, place: &str, cx: &mut Context<Self>) -> Self {
        let poll = cx.spawn(async move |this, cx| {
            loop {
                if this.update(cx, |v, cx| v.load(cx)).is_err() {
                    return;
                }
                cx.background_executor().timer(EVERY).await;
            }
        });
        TeamsView {
            places,
            place: place.to_string(),
            teams: vec![],
            people: vec![],
            loaded: false,
            selected: None,
            team: None,
            draft: None,
            new_role: "member".into(),
            busy: false,
            error: None,
            armed: None,
            _poll: poll,
        }
    }

    fn client(&self, cx: &gpui::App) -> Option<Client> {
        self.places.read(cx).client(&self.place)
    }

    /// Whether the user runs the place: only an administrator makes and
    /// deletes teams.
    fn admin(&self, cx: &gpui::App) -> bool {
        self.places
            .read(cx)
            .get(&self.place)
            .and_then(|(_, s)| s.me.as_ref().map(|m| m.admin))
            .unwrap_or(false)
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(c) = self.client(cx) else {
            self.loaded = true;
            self.error = Some("Sign in to this place to see its teams.".into());
            cx.notify();
            return;
        };
        let selected = self.selected.clone().filter(|s| !s.is_empty());
        let task = cx.background_spawn(async move {
            let teams = c.teams().map_err(|e| e.to_string())?;
            let people = c.people().unwrap_or_default();
            let team = selected.and_then(|s| c.team(&s).ok());
            Ok::<_, String>((teams, people, team))
        });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |v, cx| {
                v.loaded = true;
                match r {
                    Ok((teams, people, team)) => {
                        v.teams = teams;
                        v.people = people;
                        if let Some(t) = team
                            && v.selected.as_deref() == Some(t.id.as_str())
                        {
                            v.team = Some(t);
                        }
                        if v.selected.is_none()
                            && let Some(first) = v.teams.first()
                        {
                            let id = first.id.clone();
                            v.select(Some(id), cx);
                        }
                    }
                    Err(e) => v.error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn select(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        self.selected = id;
        self.team = None;
        self.draft = None;
        self.armed = None;
        self.error = None;
        cx.notify();
        if self.selected.as_ref().is_some_and(|s| !s.is_empty()) {
            self.load(cx);
        }
    }

    fn act(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce() -> Result<Option<String>, String> + Send + 'static,
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
                match r {
                    Ok(Some(id)) => v.select(Some(id), cx),
                    Ok(None) => {}
                    Err(e) => v.error = Some(e),
                }
                v.draft = None;
                v.load(cx);
            });
        })
        .detach();
    }
}

impl Render for TeamsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        if !self.loaded {
            return note("Reading the place's teams…", &p).into_any_element();
        }
        let making = self.selected.as_deref() == Some("");
        if self.draft.is_none() && (making || self.team.is_some()) {
            let t = self.team.as_ref();
            self.draft = Some(Draft {
                name: input(
                    &t.map_or(String::new(), |t| t.name.clone()),
                    "Name",
                    window,
                    cx,
                ),
                slug: input("", "its name in paths, such as core", window, cx),
                description: input(
                    &t.map_or(String::new(), |t| t.description.clone()),
                    "What the team does",
                    window,
                    cx,
                ),
            });
        }
        let mut left = v_flex().gap(px(4.));
        for t in &self.teams {
            let on = self.selected.as_deref() == Some(t.id.as_str());
            let id = t.id.clone();
            left = left.child(
                item(
                    SharedString::from(format!("team-{}", t.id)),
                    t.name.clone(),
                    t.role
                        .as_ref()
                        .map_or(String::new(), |r| role_name(&r.to_string()).to_string()),
                    Some(
                        form::small(
                            format!(
                                "{} member{} · {} template{}",
                                t.member_count,
                                if t.member_count == 1 { "" } else { "s" },
                                t.templates,
                                if t.templates == 1 { "" } else { "s" }
                            ),
                            &p,
                        )
                        .into_any_element(),
                    ),
                    on,
                    &p,
                )
                .on_click(cx.listener(move |v, _, _, cx| v.select(Some(id.clone()), cx))),
            );
        }
        if self.teams.is_empty() {
            left = left.child(note("You are in no team here.", &p));
        }
        if self.admin(cx) {
            left = left.child(
                button("team-new", "New Team", &p)
                    .on_click(cx.listener(|v, _, _, cx| v.select(Some(String::new()), cx))),
            );
        }
        let right = if making || self.team.is_some() {
            self.detail(&p, cx).into_any_element()
        } else if self.selected.is_some() {
            note("Reading the team…", &p).into_any_element()
        } else {
            note("Choose a team, or make one.", &p).into_any_element()
        };
        v_flex()
            .gap(px(8.))
            .child(note(
                "Teams own templates, packs and images together: viewers use what the team owns, members also change it, and admins also run the team.",
                &p,
            ))
            .child(split(left, right))
            .when_some(self.error.clone(), |d, e| d.child(form::error(e, &p)))
            .into_any_element()
    }
}

impl TeamsView {
    fn detail(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let d = self.draft.as_ref().expect("a draft");
        let t = self.team.as_ref();
        let can = t.is_none_or(|t| t.can_manage) && !self.busy;
        let me = cx.entity().downgrade();
        let mut out = v_flex()
            .gap(px(6.))
            .child(section("Team", p))
            .child(field("Name", p).child(div().flex_1().child(Input::new(&d.name).small())));
        if t.is_none() {
            out = out
                .child(field("Slug", p).child(div().flex_1().child(Input::new(&d.slug).small())));
        }
        out = out
            .child(
                field("Description", p)
                    .child(div().flex_1().child(Input::new(&d.description).small())),
            )
            .child(h_flex().gap(px(6.)).child(if can {
                primary(
                    "team-save",
                    if t.is_some() {
                        "Save Changes"
                    } else {
                        "Make Team"
                    },
                    p,
                )
                .on_click(cx.listener(|v, _, _, cx| {
                    let (Some(d), Some(c)) = (&v.draft, v.client(cx)) else {
                        return;
                    };
                    let (name, desc, slug) = (
                        text(&d.name, cx),
                        text(&d.description, cx),
                        text(&d.slug, cx),
                    );
                    let id = v.team.as_ref().map(|t| t.id.clone());
                    v.act(cx, move || match id {
                        Some(id) => c
                            .update_team(&id, json!({ "name": name, "description": desc }))
                            .map(|_| None)
                            .map_err(|e| e.to_string()),
                        None => c
                            .create_team(json!({ "name": name, "slug": slug, "description": desc }))
                            .map(|t| Some(t.id))
                            .map_err(|e| e.to_string()),
                    });
                }))
            } else {
                disabled("team-save", "Save Changes", p)
            }));
        let Some(t) = t else {
            return out;
        };
        out = out.child(section("Members", p));
        for m in &t.members {
            let user = m.person.id.clone();
            let user2 = user.clone();
            let team = t.id.clone();
            let team2 = team.clone();
            let key = format!("member-{user}");
            let mut r = form::row(p).child(
                div()
                    .flex_1()
                    .child(format!("{} ({})", m.person.display_name, m.person.username)),
            );
            if t.can_manage {
                r = r
                    .child(choice(
                        SharedString::from(format!("team-role-{user}")),
                        ROLES
                            .iter()
                            .map(|(l, v)| (l.to_string(), v.to_string()))
                            .collect(),
                        Some(m.role.to_string()),
                        me.clone(),
                        move |v: &mut Self, role, _, cx| {
                            let Some(c) = v.client(cx) else { return };
                            let (team, user) = (team.clone(), user.clone());
                            v.act(cx, move || {
                                c.set_team_member(&team, &user, &role)
                                    .map(|_| None)
                                    .map_err(|e| e.to_string())
                            });
                        },
                    ))
                    .child(
                        twice(
                            SharedString::from(format!("team-remove-{user2}")),
                            "Remove…",
                            "Remove them?",
                            self.armed.as_deref() == Some(key.as_str()),
                            self.busy,
                            p,
                        )
                        .on_click(cx.listener(move |v, _, _, cx| {
                            if v.armed.as_deref() != Some(key.as_str()) {
                                v.armed = Some(key.clone());
                                cx.notify();
                                return;
                            }
                            let Some(c) = v.client(cx) else { return };
                            let (team, user) = (team2.clone(), user2.clone());
                            v.act(cx, move || {
                                c.remove_team_member(&team, &user)
                                    .map(|_| None)
                                    .map_err(|e| e.to_string())
                            });
                        })),
                    );
            } else {
                r = r.child(form::small(role_name(&m.role.to_string()), p));
            }
            out = out.child(r);
        }
        if t.can_manage {
            let people: Vec<(String, String)> =
                std::iter::once(("Add a Person".to_string(), String::new()))
                    .chain(
                        self.people
                            .iter()
                            .filter(|x| !t.members.iter().any(|m| m.person.id == x.id))
                            .map(|x| {
                                (format!("{} ({})", x.display_name, x.username), x.id.clone())
                            }),
                    )
                    .collect();
            let team = t.id.clone();
            out = out.child(
                h_flex()
                    .gap(px(6.))
                    .child(choice(
                        "team-add",
                        people,
                        Some(String::new()),
                        me.clone(),
                        move |v: &mut Self, user, _, cx| {
                            if user.is_empty() {
                                return;
                            }
                            let Some(c) = v.client(cx) else { return };
                            let (team, role) = (team.clone(), v.new_role.clone());
                            v.act(cx, move || {
                                c.set_team_member(&team, &user, &role)
                                    .map(|_| None)
                                    .map_err(|e| e.to_string())
                            });
                        },
                    ))
                    .child(choice(
                        "team-add-role",
                        ROLES
                            .iter()
                            .map(|(l, v)| (format!("as {}", l.to_lowercase()), v.to_string()))
                            .collect(),
                        Some(self.new_role.clone()),
                        me.clone(),
                        |v: &mut Self, r, _, cx| {
                            v.new_role = r;
                            cx.notify()
                        },
                    )),
            );
        }
        if self.admin(cx) {
            out = out.child(
                h_flex().pt(px(8.)).child(div().flex_1()).child(
                    twice(
                        "team-delete",
                        "Delete Team…",
                        "Delete the team? It must own nothing.",
                        self.armed.as_deref() == Some("team"),
                        self.busy,
                        p,
                    )
                    .on_click(cx.listener(|v, _, _, cx| {
                        if v.armed.as_deref() != Some("team") {
                            v.armed = Some("team".into());
                            cx.notify();
                            return;
                        }
                        let (Some(t), Some(c)) = (v.team.clone(), v.client(cx)) else {
                            return;
                        };
                        v.selected = None;
                        v.team = None;
                        v.act(cx, move || {
                            c.delete_team(&t.id)
                                .map(|_| None)
                                .map_err(|e| e.to_string())
                        });
                    })),
                ),
            );
        }
        out
    }
}
