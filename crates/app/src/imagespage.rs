//! Settings › a place › Images: the repositories of the place's registry
//! the user may pull, and the chosen one's description, who may pull it,
//! who besides its owner pushes to it, and its tags, each deleted when
//! pressed twice, as the web UI's Images page has them.

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

fn bytes(n: i64) -> String {
    match n {
        n if n >= 1_000_000_000 => format!("{:.1} GB", n as f64 / 1e9),
        n if n >= 1_000_000 => format!("{:.1} MB", n as f64 / 1e6),
        n if n >= 1000 => format!("{:.1} kB", n as f64 / 1e3),
        n => format!("{n} B"),
    }
}

pub struct ImagesView {
    places: Entity<Places>,
    place: String,
    repos: Vec<types::ImageRepository>,
    people: Vec<types::Person>,
    teams: Vec<types::Team>,
    loaded: bool,
    selected: Option<String>,
    repo: Option<types::ImageRepository>,
    description: Option<Entity<InputState>>,
    visibility: String,
    busy: bool,
    error: Option<String>,
    armed: Option<String>,
    _poll: Task<()>,
}

impl ImagesView {
    pub fn new(places: Entity<Places>, place: &str, cx: &mut Context<Self>) -> Self {
        let poll = cx.spawn(async move |this, cx| {
            loop {
                if this.update(cx, |v, cx| v.load(cx)).is_err() {
                    return;
                }
                cx.background_executor().timer(EVERY).await;
            }
        });
        ImagesView {
            places,
            place: place.to_string(),
            repos: vec![],
            people: vec![],
            teams: vec![],
            loaded: false,
            selected: None,
            repo: None,
            description: None,
            visibility: "private".into(),
            busy: false,
            error: None,
            armed: None,
            _poll: poll,
        }
    }

    fn client(&self, cx: &gpui::App) -> Option<Client> {
        self.places.read(cx).client(&self.place)
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(c) = self.client(cx) else {
            self.loaded = true;
            self.error = Some("Sign in to this place to see its images.".into());
            cx.notify();
            return;
        };
        let selected = self.selected.clone();
        let task = cx.background_spawn(async move {
            let repos = c.images().map_err(|e| e.to_string())?;
            let repo = selected.and_then(|s| c.image(&s).ok());
            Ok::<_, String>((
                repos,
                c.people().unwrap_or_default(),
                c.teams().unwrap_or_default(),
                repo,
            ))
        });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |v, cx| {
                v.loaded = true;
                match r {
                    Ok((repos, people, teams, repo)) => {
                        v.repos = repos;
                        v.people = people;
                        v.teams = teams;
                        if let Some(r) = repo
                            && v.selected.as_deref() == Some(r.id.as_str())
                        {
                            if v.repo.as_ref().is_none_or(|o| o.id != r.id) {
                                v.description = None;
                                v.visibility = r.visibility.to_string();
                            }
                            v.repo = Some(r);
                        }
                        if v.selected.is_none()
                            && let Some(first) = v.repos.first()
                        {
                            let id = first.id.clone();
                            v.select(id, cx);
                        }
                    }
                    Err(e) => v.error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn select(&mut self, id: String, cx: &mut Context<Self>) {
        self.selected = Some(id);
        self.repo = None;
        self.description = None;
        self.armed = None;
        self.error = None;
        self.load(cx);
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
}

impl Render for ImagesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        if !self.loaded {
            return note("Reading the place's images…", &p).into_any_element();
        }
        if self.description.is_none()
            && let Some(r) = &self.repo
        {
            self.description = Some(input(&r.description, "What the image is for", window, cx));
        }
        let mut left = v_flex().gap(px(4.));
        for r in &self.repos {
            let on = self.selected.as_deref() == Some(r.id.as_str());
            let id = r.id.clone();
            left = left.child(
                item(
                    SharedString::from(format!("image-{}", r.id)),
                    r.path.clone(),
                    format!(
                        "{} tag{}",
                        r.tag_count,
                        if r.tag_count == 1 { "" } else { "s" }
                    ),
                    Some(
                        form::small(
                            if r.visibility.to_string() == "shared" {
                                "everyone pulls it"
                            } else {
                                "its owner and collaborators pull it"
                            },
                            &p,
                        )
                        .into_any_element(),
                    ),
                    on,
                    &p,
                )
                .on_click(cx.listener(move |v, _, _, cx| v.select(id.clone(), cx))),
            );
        }
        if self.repos.is_empty() {
            left = left.child(note(
                "No images yet. Push one to the place's registry to make a repository.",
                &p,
            ));
        }
        let right = match &self.repo {
            Some(_) => self.detail(&p, cx).into_any_element(),
            None if self.selected.is_some() => {
                note("Reading the repository…", &p).into_any_element()
            }
            None => note("Choose a repository.", &p).into_any_element(),
        };
        v_flex()
            .gap(px(8.))
            .child(note(
                "The place's registry: sign in with your username and an access token, push an image to <host>/<your username>/<name>, and a template can use it.",
                &p,
            ))
            .child(split(left, right))
            .when_some(self.error.clone(), |d, e| d.child(form::error(e, &p)))
            .into_any_element()
    }
}

impl ImagesView {
    fn detail(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let r = self.repo.as_ref().expect("a repository");
        let me = cx.entity().downgrade();
        let mut out = v_flex()
            .gap(px(6.))
            .child(section("Repository", p))
            .child(field("Name", p).child(form::mono(r.name.clone())))
            .child(
                field("Owner", p).child(
                    div().child(
                        r.team
                            .as_ref()
                            .map(|t| format!("Team {}", t.name))
                            .or_else(|| r.owner.as_ref().map(|o| o.display_name.clone()))
                            .unwrap_or_default(),
                    ),
                ),
            );
        if r.can_manage {
            if let Some(desc) = &self.description {
                out = out.child(
                    field("Description", p).child(div().flex_1().child(Input::new(desc).small())),
                );
            }
            out = out
                .child(field("Who pulls it", p).child(choice(
                    "image-visibility",
                    vec![
                        ("Its owner and collaborators".into(), "private".to_string()),
                        ("Everyone here".into(), "shared".to_string()),
                    ],
                    Some(self.visibility.clone()),
                    me.clone(),
                    |v: &mut Self, x, _, cx| {
                        v.visibility = x;
                        cx.notify()
                    },
                )))
                .child(h_flex().child(if self.busy {
                    disabled("image-save", "Save Changes", p)
                } else {
                    primary("image-save", "Save Changes", p).on_click(cx.listener(|v, _, _, cx| {
                        let (Some(r), Some(c), Some(d)) =
                            (v.repo.clone(), v.client(cx), v.description.clone())
                        else {
                            return;
                        };
                        let body =
                            json!({ "visibility": v.visibility, "description": text(&d, cx) });
                        v.act(cx, move || {
                            c.update_image(&r.id, body)
                                .map(|_| ())
                                .map_err(|e| e.to_string())
                        });
                    }))
                }));
        } else if !r.description.is_empty() {
            out = out.child(field("Description", p).child(div().child(r.description.clone())));
        }

        // Collaborators.
        out = out.child(section("Collaborators", p)).child(note(
            "People and teams who push to it besides its owner.",
            p,
        ));
        let users: Vec<String> = r.collaborators.iter().map(|x| x.id.clone()).collect();
        let teams: Vec<String> = r.collaborator_teams.iter().map(|x| x.id.clone()).collect();
        let mut chips = h_flex().gap(px(6.)).flex_wrap();
        for x in &r.collaborators {
            let (id, users, teams) = (x.id.clone(), users.clone(), teams.clone());
            chips = chips.child(
                h_flex()
                    .id(SharedString::from(format!("image-collab-{}", x.id)))
                    .gap(px(4.))
                    .child(form::chip(x.display_name.clone(), p))
                    .when(r.can_push, |d| {
                        d.child(div().cursor_pointer().text_color(p.muted).child("×"))
                            .on_click(cx.listener(move |v, _, _, cx| {
                                let (Some(r), Some(c)) = (v.repo.clone(), v.client(cx)) else {
                                    return;
                                };
                                let users: Vec<String> =
                                    users.iter().filter(|u| **u != id).cloned().collect();
                                let teams = teams.clone();
                                v.act(cx, move || {
                                    c.set_image_collaborators(&r.id, &users, &teams)
                                        .map(|_| ())
                                        .map_err(|e| e.to_string())
                                });
                            }))
                    }),
            );
        }
        for x in &r.collaborator_teams {
            let (id, users, teams) = (x.id.clone(), users.clone(), teams.clone());
            chips = chips.child(
                h_flex()
                    .id(SharedString::from(format!("image-collab-team-{}", x.id)))
                    .gap(px(4.))
                    .child(form::chip(format!("Team {}", x.name), p))
                    .when(r.can_push, |d| {
                        d.child(div().cursor_pointer().text_color(p.muted).child("×"))
                            .on_click(cx.listener(move |v, _, _, cx| {
                                let (Some(r), Some(c)) = (v.repo.clone(), v.client(cx)) else {
                                    return;
                                };
                                let teams: Vec<String> =
                                    teams.iter().filter(|t| **t != id).cloned().collect();
                                let users = users.clone();
                                v.act(cx, move || {
                                    c.set_image_collaborators(&r.id, &users, &teams)
                                        .map(|_| ())
                                        .map_err(|e| e.to_string())
                                });
                            }))
                    }),
            );
        }
        out = out.child(chips);
        if r.can_push {
            let people: Vec<(String, String)> =
                std::iter::once(("Add a Person".to_string(), String::new()))
                    .chain(
                        self.people
                            .iter()
                            .filter(|x| !users.contains(&x.id))
                            .map(|x| {
                                (format!("{} ({})", x.display_name, x.username), x.id.clone())
                            }),
                    )
                    .collect();
            let team_choices: Vec<(String, String)> =
                std::iter::once(("Add a Team".to_string(), String::new()))
                    .chain(
                        self.teams
                            .iter()
                            .filter(|x| !teams.contains(&x.id))
                            .map(|x| (x.name.clone(), x.id.clone())),
                    )
                    .collect();
            let (u1, t1, u2, t2) = (users.clone(), teams.clone(), users.clone(), teams.clone());
            out = out.child(
                h_flex()
                    .gap(px(6.))
                    .child(choice(
                        "image-collab-add",
                        people,
                        Some(String::new()),
                        me.clone(),
                        move |v: &mut Self, x, _, cx| {
                            if x.is_empty() {
                                return;
                            }
                            let (Some(r), Some(c)) = (v.repo.clone(), v.client(cx)) else {
                                return;
                            };
                            let mut users = u1.clone();
                            users.push(x);
                            let teams = t1.clone();
                            v.act(cx, move || {
                                c.set_image_collaborators(&r.id, &users, &teams)
                                    .map(|_| ())
                                    .map_err(|e| e.to_string())
                            });
                        },
                    ))
                    .child(choice(
                        "image-collab-team-add",
                        team_choices,
                        Some(String::new()),
                        me.clone(),
                        move |v: &mut Self, x, _, cx| {
                            if x.is_empty() {
                                return;
                            }
                            let (Some(r), Some(c)) = (v.repo.clone(), v.client(cx)) else {
                                return;
                            };
                            let mut teams = t2.clone();
                            teams.push(x);
                            let users = u2.clone();
                            v.act(cx, move || {
                                c.set_image_collaborators(&r.id, &users, &teams)
                                    .map(|_| ())
                                    .map_err(|e| e.to_string())
                            });
                        },
                    )),
            );
        }

        // Tags.
        out = out.child(section("Tags", p));
        if r.tags.is_empty() {
            out = out.child(note("No tags.", p));
        }
        for tag in &r.tags {
            let key = format!("tag-{}", tag.name);
            let name = tag.name.clone();
            out = out.child(
                form::row(p)
                    .child(form::mono(tag.name.clone()).w(px(160.)).truncate())
                    .child(form::mono(tag.digest.chars().take(19).collect::<String>()).flex_1())
                    .child(form::small(
                        format!(
                            "{} · {}",
                            bytes(tag.size_bytes),
                            tag.updated_at
                                .with_timezone(&chrono::Local)
                                .format("%-d %b %Y")
                        ),
                        p,
                    ))
                    .when(r.can_push, |d| {
                        d.child(
                            twice(
                                SharedString::from(format!("image-tag-delete-{}", tag.name)),
                                "Delete…",
                                "Delete the tag?",
                                self.armed.as_deref() == Some(key.as_str()),
                                self.busy,
                                p,
                            )
                            .on_click(cx.listener(
                                move |v, _, _, cx| {
                                    if !v.armed(&key, cx) {
                                        return;
                                    }
                                    let (Some(r), Some(c)) = (v.repo.clone(), v.client(cx)) else {
                                        return;
                                    };
                                    let name = name.clone();
                                    v.act(cx, move || {
                                        c.delete_image_tag(&r.id, &name)
                                            .map(|_| ())
                                            .map_err(|e| e.to_string())
                                    });
                                },
                            )),
                        )
                    }),
            );
        }
        if r.can_manage {
            out = out.child(
                h_flex().pt(px(8.)).child(div().flex_1()).child(
                    twice(
                        "image-delete",
                        "Delete Repository…",
                        "Delete it and every tag?",
                        self.armed.as_deref() == Some("repo"),
                        self.busy,
                        p,
                    )
                    .on_click(cx.listener(|v, _, _, cx| {
                        if !v.armed("repo", cx) {
                            return;
                        }
                        let (Some(r), Some(c)) = (v.repo.clone(), v.client(cx)) else {
                            return;
                        };
                        v.selected = None;
                        v.repo = None;
                        v.act(cx, move || c.delete_image(&r.id).map_err(|e| e.to_string()));
                    })),
                ),
            );
        }
        out.child(h_flex().child(
            button("image-refresh", "Refresh", p).on_click(cx.listener(|v, _, _, cx| v.load(cx))),
        ))
    }
}
