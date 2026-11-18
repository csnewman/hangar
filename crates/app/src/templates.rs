//! Settings › a place › Templates: the place's templates the user may see,
//! and the chosen one as the web UI's template editor has it, as one
//! form: general (name, description, owner, who can use it), the machine,
//! the workspace (repositories, projects, trusted folders, web servers),
//! what it is given of its owner's, its packs, naming, placement, and its
//! collaborators. Saved with Save Changes; a template the user may not
//! change is shown, not changed.

use std::time::Duration;

use gpui::{
    Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled, Task, Window, div,
    prelude::*, px,
};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputState};
use gpui_component::{Sizable, h_flex, v_flex};
use hangar_openapi::clientapi::types;
use serde_json::{Value, json};

use crate::api::Client;
use crate::form::{self, choice, field, input, item, note, section, split, text, twice};
use crate::palette::{self, Palette};
use crate::places::Places;
use crate::ui::{button, disabled, primary};

/// How often the list is read again while the page is open.
const EVERY: Duration = Duration::from_secs(30);

/// What the page reads of the place.
#[derive(Default, Clone)]
struct Read {
    templates: Vec<types::Template>,
    teams: Vec<types::Team>,
    people: Vec<types::Person>,
    packs: Vec<types::Pack>,
}

/// A repository's fields, as typed.
struct RepoDraft {
    url: Entity<InputState>,
    path: Entity<InputState>,
    git_ref: Entity<InputState>,
    branch: Entity<InputState>,
}

/// The form: a template as it is being changed.
struct Draft {
    name: Entity<InputState>,
    description: Entity<InputState>,
    image: Entity<InputState>,
    cpus: Entity<InputState>,
    memory_gib: Entity<InputState>,
    swap_gib: Entity<InputState>,
    projects: Entity<InputState>,
    trusted: Entity<InputState>,
    web_names: Entity<InputState>,
    pattern: Entity<InputState>,
    hint: Entity<InputState>,
    placement: Entity<InputState>,
    repos: Vec<RepoDraft>,
    /// Each pack listed, and the copy pinned.
    packs: Vec<(String, Option<String>)>,
    owner: String,
    visibility: String,
    display: String,
    gpu: String,
    dax: bool,
    reclaim: bool,
    /// no_self_attached, no_sensitive_files, no_ssh_keys, no_registry,
    /// no_editor_trust.
    access: [bool; 5],
    users: Vec<String>,
    teams: Vec<String>,
}

const ACCESS: [(&str, &str); 5] = [
    (
        "no_self_attached",
        "Packs attaching themselves, the profile among them",
    ),
    (
        "no_sensitive_files",
        "Sensitive files, such as Claude's sign-in",
    ),
    ("no_ssh_keys", "Signatures from the owner's SSH keys"),
    ("no_registry", "A sign-in to this place's registry"),
    ("no_editor_trust", "The Code tab trusting its folders"),
];

fn gib(mib: i64) -> String {
    let g = mib as f64 / 1024.;
    if g.fract() == 0. {
        format!("{g:.0}")
    } else {
        format!("{g:.1}")
    }
}

fn list(items: &[String]) -> String {
    items.join(", ")
}

fn split_list(text: &str) -> Vec<String> {
    text.split([',', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

impl Draft {
    fn new(t: Option<&types::TemplateDetail>, window: &mut Window, cx: &mut gpui::App) -> Draft {
        let spec = t.map(|t| &t.spec);
        let s = |v: Option<String>| v.unwrap_or_default();
        let repos = spec
            .map(|sp| {
                sp.repos
                    .iter()
                    .map(|r| RepoDraft {
                        url: input(&r.url, "git@github.com:you/repo.git", window, cx),
                        path: input(&r.path, "/workspace/repo", window, cx),
                        git_ref: input(&s(r.ref_.clone()), "default branch", window, cx),
                        branch: input(&s(r.branch.clone()), "none", window, cx),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let access = spec.and_then(|sp| sp.access.clone()).unwrap_or_default();
        Draft {
            name: input(
                &t.map_or(String::new(), |t| t.name.clone()),
                "Name",
                window,
                cx,
            ),
            description: input(
                &t.map_or(String::new(), |t| t.description.clone()),
                "What it is for",
                window,
                cx,
            ),
            image: input(
                &spec.map_or("ghcr.io/csnewman/hangar/base:ubuntu-26.04".into(), |sp| {
                    sp.image.to_string()
                }),
                "an image reference",
                window,
                cx,
            ),
            cpus: input(
                &spec.map_or("2".into(), |sp| sp.cpus.to_string()),
                "2",
                window,
                cx,
            ),
            memory_gib: input(
                &spec.map_or("4".into(), |sp| gib(sp.memory_mib)),
                "4",
                window,
                cx,
            ),
            swap_gib: input(
                &spec.and_then(|sp| sp.swap_mib).map(gib).unwrap_or_default(),
                "the worker's",
                window,
                cx,
            ),
            projects: input(
                &list(spec.map(|sp| sp.projects.as_slice()).unwrap_or_default()),
                "its repositories' folders",
                window,
                cx,
            ),
            trusted: input(
                &list(
                    spec.map(|sp| sp.trusted_folders.as_slice())
                        .unwrap_or_default(),
                ),
                "none beyond its repositories and projects",
                window,
                cx,
            ),
            web_names: input(
                &list(spec.map(|sp| sp.web_names.as_slice()).unwrap_or_default()),
                "dashboard, docs",
                window,
                cx,
            ),
            pattern: input(
                &t.and_then(|t| t.name_pattern.clone()).unwrap_or_default(),
                "any name, or a pattern such as (?P<ticket>[A-Z]+-[0-9]+)",
                window,
                cx,
            ),
            hint: input(
                &t.and_then(|t| t.name_hint.clone()).unwrap_or_default(),
                "what the pattern wants, in words",
                window,
                cx,
            ),
            placement: input(
                &spec
                    .map(|sp| {
                        let mut l: Vec<String> = sp
                            .placement
                            .iter()
                            .map(|(k, v)| format!("{k}={v}"))
                            .collect();
                        l.sort();
                        l.join(", ")
                    })
                    .unwrap_or_default(),
                "runtime=krun, any worker without",
                window,
                cx,
            ),
            repos,
            packs: spec
                .map(|sp| {
                    sp.packs
                        .iter()
                        .map(|r| (r.pack.clone(), r.copy.clone()))
                        .collect()
                })
                .unwrap_or_default(),
            owner: t
                .and_then(|t| t.team.as_ref().map(|x| x.id.clone()))
                .unwrap_or_default(),
            visibility: t.map_or("private".into(), |t| t.visibility.to_string()),
            display: spec.map_or("desktop".into(), |sp| sp.display.to_string()),
            gpu: spec.map_or("none".into(), |sp| sp.gpu.to_string()),
            dax: spec.and_then(|sp| sp.dax).unwrap_or(false),
            reclaim: spec.and_then(|sp| sp.memory_reclaim).unwrap_or(false),
            access: [
                access.no_self_attached.unwrap_or(false),
                access.no_sensitive_files.unwrap_or(false),
                access.no_ssh_keys.unwrap_or(false),
                access.no_registry.unwrap_or(false),
                access.no_editor_trust.unwrap_or(false),
            ],
            users: t.map_or(vec![], |t| {
                t.collaborators.iter().map(|p| p.id.clone()).collect()
            }),
            teams: t.map_or(vec![], |t| {
                t.collaborator_teams.iter().map(|x| x.id.clone()).collect()
            }),
        }
    }

    /// The template as the form says, for making or changing it.
    fn input(&self, existing: Option<&types::TemplateDetail>, cx: &gpui::App) -> Value {
        let number = |e: &Entity<InputState>| text(e, cx).replace(',', ".").parse::<f64>().ok();
        let mut placement = serde_json::Map::new();
        for each in split_list(&text(&self.placement, cx)) {
            if let Some((k, v)) = each.split_once('=') {
                placement.insert(k.trim().into(), v.trim().into());
            }
        }
        let mut access = serde_json::Map::new();
        for (on, (key, _)) in self.access.iter().zip(ACCESS) {
            if *on {
                access.insert(key.into(), true.into());
            }
        }
        let mut spec = json!({
            "image": text(&self.image, cx),
            "cpus": number(&self.cpus).map_or(2, |n| n as i64),
            "memory_mib": number(&self.memory_gib).map_or(4096, |g| (g * 1024.) as i64),
            "display": self.display,
            "gpu": self.gpu,
            "repos": self.repos.iter().filter(|r| !text(&r.url, cx).is_empty()).map(|r| {
                let mut o = json!({ "url": text(&r.url, cx), "path": text(&r.path, cx) });
                let (gr, br) = (text(&r.git_ref, cx), text(&r.branch, cx));
                if !gr.is_empty() { o["ref"] = gr.into(); }
                if !br.is_empty() { o["branch"] = br.into(); }
                o
            }).collect::<Vec<_>>(),
            "projects": split_list(&text(&self.projects, cx)),
            "trusted_folders": split_list(&text(&self.trusted, cx)),
            "web_names": split_list(&text(&self.web_names, cx)),
            "packs": self.packs.iter().map(|(pack, copy)| {
                let mut o = json!({ "pack": pack });
                if let Some(c) = copy { o["copy"] = c.clone().into(); }
                o
            }).collect::<Vec<_>>(),
            "access": access,
            "dax": self.dax,
            "memory_reclaim": self.reclaim,
            "placement": placement,
        });
        if let Some(g) = number(&self.swap_gib) {
            spec["swap_mib"] = ((g * 1024.) as i64).into();
        }
        let mut out = json!({
            "name": text(&self.name, cx),
            "description": text(&self.description, cx),
            "visibility": self.visibility,
            "spec": spec,
            "name_pattern": text(&self.pattern, cx),
            "name_hint": text(&self.hint, cx),
        });
        let was = existing.and_then(|t| t.team.as_ref().map(|x| x.id.clone()));
        if existing.is_none() || was.unwrap_or_default() != self.owner {
            out["team_id"] = self.owner.clone().into();
        }
        out
    }
}

pub struct TemplatesView {
    places: Entity<Places>,
    place: String,
    read: Read,
    loaded: bool,
    /// The template shown: its ID, or "" for one being made.
    selected: Option<String>,
    detail: Option<types::TemplateDetail>,
    draft: Option<Draft>,
    busy: bool,
    error: Option<String>,
    saved: Option<String>,
    /// The delete button pressed once.
    armed: bool,
    _poll: Task<()>,
}

impl TemplatesView {
    pub fn new(places: Entity<Places>, place: &str, cx: &mut Context<Self>) -> Self {
        let poll = cx.spawn(async move |this, cx| {
            loop {
                if this.update(cx, |v, cx| v.load(cx)).is_err() {
                    return;
                }
                cx.background_executor().timer(EVERY).await;
            }
        });
        TemplatesView {
            places,
            place: place.to_string(),
            read: Read::default(),
            loaded: false,
            selected: None,
            detail: None,
            draft: None,
            busy: false,
            error: None,
            saved: None,
            armed: false,
            _poll: poll,
        }
    }

    fn client(&self, cx: &gpui::App) -> Option<Client> {
        self.places.read(cx).client(&self.place)
    }

    fn me(&self, cx: &gpui::App) -> (String, bool) {
        self.places
            .read(cx)
            .get(&self.place)
            .and_then(|(_, s)| s.me.clone())
            .map_or((String::new(), false), |m| (m.id, m.admin))
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(c) = self.client(cx) else {
            self.loaded = true;
            self.error = Some("Sign in to this place to see its templates.".into());
            cx.notify();
            return;
        };
        let task = cx.background_spawn(async move {
            Ok::<_, String>(Read {
                templates: c.templates().map_err(|e| e.to_string())?,
                teams: c.teams().unwrap_or_default(),
                people: c.people().unwrap_or_default(),
                packs: c.packs().unwrap_or_default(),
            })
        });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |v, cx| {
                v.loaded = true;
                match r {
                    Ok(read) => {
                        v.read = read;
                        if v.selected.is_none()
                            && let Some(first) = v.read.templates.first()
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

    /// Shows a template, or with "" a new one's form.
    pub fn select(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        self.selected = id.clone();
        self.detail = None;
        self.draft = None;
        self.armed = false;
        self.saved = None;
        self.error = None;
        cx.notify();
        let Some(id) = id.filter(|i| !i.is_empty()) else {
            return;
        };
        let Some(c) = self.client(cx) else { return };
        let task = cx.background_spawn(async move { c.template(&id) });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |v, cx| {
                match r {
                    Ok(t) if v.selected.as_deref() == Some(t.id.as_str()) => v.detail = Some(t),
                    Ok(_) => {}
                    Err(e) => v.error = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let (Some(draft), Some(c)) = (&self.draft, self.client(cx)) else {
            return;
        };
        let body = draft.input(self.detail.as_ref(), cx);
        let id = self.detail.as_ref().map(|t| t.id.clone());
        let (users, teams) = (draft.users.clone(), draft.teams.clone());
        let collaborators_changed = self.detail.as_ref().is_some_and(|t| {
            let mut a: Vec<String> = t.collaborators.iter().map(|p| p.id.clone()).collect();
            let mut b: Vec<String> = t.collaborator_teams.iter().map(|x| x.id.clone()).collect();
            let (mut u, mut tm) = (users.clone(), teams.clone());
            a.sort();
            b.sort();
            u.sort();
            tm.sort();
            a != u || b != tm
        });
        self.busy = true;
        self.error = None;
        cx.notify();
        let task = cx.background_spawn(async move {
            let t = match &id {
                Some(id) => c.update_template(id, body),
                None => c.create_template(body),
            }
            .map_err(|e| e.to_string())?;
            if collaborators_changed {
                return c
                    .set_template_collaborators(&t.id, &users, &teams)
                    .map_err(|e| e.to_string());
            }
            Ok(t)
        });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |v, cx| {
                v.busy = false;
                match r {
                    Ok(t) => {
                        v.selected = Some(t.id.clone());
                        v.saved = Some("Saved.".into());
                        v.detail = Some(t);
                        v.draft = None;
                        v.load(cx);
                    }
                    Err(e) => v.error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn delete(&mut self, cx: &mut Context<Self>) {
        if !self.armed {
            self.armed = true;
            cx.notify();
            return;
        }
        let (Some(t), Some(c)) = (self.detail.clone(), self.client(cx)) else {
            return;
        };
        self.busy = true;
        cx.notify();
        let task = cx.background_spawn(async move { c.delete_template(&t.id) });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |v, cx| {
                v.busy = false;
                v.armed = false;
                match r {
                    Ok(()) => {
                        v.selected = None;
                        v.detail = None;
                        v.draft = None;
                        v.load(cx);
                    }
                    Err(e) => v.error = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn edit(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Draft)) {
        if let Some(d) = &mut self.draft {
            f(d);
            self.saved = None;
            cx.notify();
        }
    }
}

impl Render for TemplatesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        if !self.loaded {
            return note("Reading the place's templates…", &p).into_any_element();
        }
        let showing = self.selected.as_deref() == Some("") || self.detail.is_some();
        if self.draft.is_none() && showing {
            self.draft = Some(Draft::new(self.detail.as_ref(), window, cx));
        }
        let mut left = v_flex().gap(px(4.));
        for t in &self.read.templates {
            let on = self.selected.as_deref() == Some(t.id.as_str());
            let id = t.id.clone();
            let owner = t.team.clone().unwrap_or_else(|| t.owner.clone());
            left = left.child(
                item(
                    SharedString::from(format!("template-{}", t.id)),
                    t.name.clone(),
                    owner,
                    (!t.description.is_empty()).then(|| {
                        form::small(t.description.clone(), &p)
                            .truncate()
                            .into_any_element()
                    }),
                    on,
                    &p,
                )
                .on_click(cx.listener(move |v, _, _, cx| v.select(Some(id.clone()), cx))),
            );
        }
        if self.read.templates.is_empty() {
            left = left.child(note("No templates yet.", &p));
        }
        left = left.child(
            button("template-new", "New Template", &p)
                .on_click(cx.listener(|v, _, _, cx| v.select(Some(String::new()), cx))),
        );
        let right = match (&self.draft, showing) {
            (Some(_), true) => self.form(&p, cx).into_any_element(),
            _ => note("Choose a template, or make one.", &p).into_any_element(),
        };
        v_flex()
            .gap(px(8.))
            .child(note(
                "The templates you may use here: what an environment made from each is given. Templates belong to this place; a team's are changed by the people the team allows.",
                &p,
            ))
            .child(split(left, right))
            .when_some(self.error.clone(), |d, e| d.child(form::error(e, &p)))
            .into_any_element()
    }
}

impl TemplatesView {
    fn form(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let d = self.draft.as_ref().expect("a draft");
        let t = self.detail.as_ref();
        let can = t.is_none_or(|t| t.can_edit) && !self.busy;
        let manage = t.is_none_or(|t| t.can_manage);
        let me = cx.entity().downgrade();
        let (my_id, admin) = self.me(cx);
        let mut f = v_flex().gap(px(6.));
        if let Some(t) = t {
            f = f.child(form::small(
                format!(
                    "Made by {}{} · {} environment{} made from it",
                    if t.owner.display_name.is_empty() {
                        &t.owner.username
                    } else {
                        &t.owner.display_name
                    },
                    t.team
                        .as_ref()
                        .map(|x| format!(" for {}", x.name))
                        .unwrap_or_default(),
                    t.environments,
                    if t.environments == 1 { "" } else { "s" }
                ),
                p,
            ));
        }
        let label = if t.is_some() {
            "Save Changes"
        } else {
            "Make Template"
        };
        let bar = h_flex()
            .gap(px(6.))
            .child(if can {
                primary("template-save", label, p).on_click(cx.listener(|v, _, _, cx| v.save(cx)))
            } else {
                disabled("template-save", label, p)
            })
            .child(
                button("template-revert", "Revert", p).on_click(cx.listener(|v, _, _, cx| {
                    v.draft = None;
                    v.saved = None;
                    if v.detail.is_none() {
                        v.selected = None;
                    }
                    cx.notify();
                })),
            )
            .when_some(self.saved.clone(), |d, s| d.child(form::small(s, p)))
            .child(div().flex_1())
            .when(t.is_some_and(|t| t.can_manage), |d| {
                d.child(
                    twice(
                        "template-delete",
                        "Delete Template…",
                        "Delete it and keep its environments?",
                        self.armed,
                        self.busy,
                        p,
                    )
                    .on_click(cx.listener(|v, _, _, cx| v.delete(cx))),
                )
            });
        f = f.child(bar);
        if !can && !self.busy {
            f = f.child(note(
                "Only its owner, its collaborators, and its team's members and admins change it.",
                p,
            ));
        }

        // General.
        let owners: Vec<(String, String)> = std::iter::once(("You".to_string(), String::new()))
            .chain(
                self.read
                    .teams
                    .iter()
                    .filter(|x| {
                        admin
                            || x.role.as_ref().is_some_and(|r| {
                                matches!(r.to_string().as_str(), "member" | "admin")
                            })
                            || x.id == d.owner
                    })
                    .map(|x| (format!("Team {}", x.name), x.id.clone())),
            )
            .collect();
        f = f
            .child(section("General", p))
            .child(field("Name", p).child(div().flex_1().child(Input::new(&d.name).small())))
            .child(
                field("Description", p)
                    .child(div().flex_1().child(Input::new(&d.description).small())),
            )
            .child(field("Owner", p).child(if manage {
                choice(
                    "template-owner",
                    owners,
                    Some(d.owner.clone()),
                    me.clone(),
                    |v: &mut Self, o, _, cx| v.edit(cx, |d| d.owner = o),
                )
                .into_any_element()
            } else {
                form::small(
                    t.and_then(|t| t.team.as_ref())
                        .map_or("its maker".into(), |x| x.name.clone()),
                    p,
                )
                .into_any_element()
            }))
            .child(field("Who can use it", p).child(choice(
                "template-visibility",
                vec![
                        (
                            if d.owner.is_empty() {
                                "Owner and collaborators"
                            } else {
                                "The team and collaborators"
                            }
                            .to_string(),
                            "private".to_string(),
                        ),
                        ("Everyone here".to_string(), "shared".to_string()),
                    ],
                Some(d.visibility.clone()),
                me.clone(),
                |v: &mut Self, x, _, cx| v.edit(cx, |d| d.visibility = x),
            )));

        // Machine.
        let short = |e: &Entity<InputState>| div().w(px(80.)).child(Input::new(e).small());
        f = f
            .child(section("Machine", p))
            .child(field("Image", p).child(div().flex_1().child(Input::new(&d.image).small())))
            .child(
                field("vCPUs, memory", p).child(
                    h_flex()
                        .gap(px(6.))
                        .child(short(&d.cpus))
                        .child(form::small("vCPUs", p))
                        .child(short(&d.memory_gib))
                        .child(form::small("GiB", p)),
                ),
            )
            .child(
                field("Swap", p).child(
                    h_flex()
                        .gap(px(6.))
                        .child(div().w(px(120.)).child(Input::new(&d.swap_gib).small()))
                        .child(form::small("GiB; empty is the worker's, 0 none", p)),
                ),
            )
            .child(field("Display", p).child(choice(
                "template-display",
                vec![
                    ("Desktop".into(), "desktop".to_string()),
                    ("Headless".into(), "none".to_string()),
                ],
                Some(d.display.clone()),
                me.clone(),
                |v: &mut Self, x, _, cx| v.edit(cx, |d| d.display = x),
            )))
            .child(field("GPU", p).child(choice(
                "template-gpu",
                vec![
                    ("None".into(), "none".to_string()),
                    ("Virtual".into(), "virtual".to_string()),
                    ("Passthrough".into(), "passthrough".to_string()),
                ],
                Some(d.gpu.clone()),
                me.clone(),
                |v: &mut Self, x, _, cx| v.edit(cx, |d| d.gpu = x),
            )))
            .child(
                field("Image files", p).child(
                    Checkbox::new("template-dax")
                        .label("Map from the worker's page cache (DAX)")
                        .checked(d.dax)
                        .on_click(cx.listener(|v, b: &bool, _, cx| {
                            let b = *b;
                            v.edit(cx, |d| d.dax = b)
                        })),
                ),
            )
            .child(
                field("Idle memory", p).child(
                    Checkbox::new("template-reclaim")
                        .label("Hand back what programs leave idle (experimental)")
                        .checked(d.reclaim)
                        .on_click(cx.listener(|v, b: &bool, _, cx| {
                            let b = *b;
                            v.edit(cx, |d| d.reclaim = b)
                        })),
                ),
            );

        // Workspace.
        f = f.child(section("Workspace", p)).child(note(
            "Cloned when an environment is made. Paths and branches may use {name}, {owner}, {short_id}, {template} and the name pattern's groups.",
            p,
        ));
        for (i, r) in d.repos.iter().enumerate() {
            f = f.child(
                v_flex()
                    .gap(px(4.))
                    .p(px(6.))
                    .border_1()
                    .border_color(p.border)
                    .rounded(px(5.))
                    .child(
                        field("Repository URL", p)
                            .child(div().flex_1().child(Input::new(&r.url).small())),
                    )
                    .child(
                        field("Clone to", p)
                            .child(div().flex_1().child(Input::new(&r.path).small())),
                    )
                    .child(
                        h_flex()
                            .gap(px(6.))
                            .child(
                                field("Check out", p)
                                    .child(div().w(px(120.)).child(Input::new(&r.git_ref).small())),
                            )
                            .child(form::small("new branch", p))
                            .child(div().flex_1().child(Input::new(&r.branch).small()))
                            .child(
                                div()
                                    .id(SharedString::from(format!("template-repo-remove-{i}")))
                                    .px(px(4.))
                                    .cursor_pointer()
                                    .text_color(p.muted)
                                    .child("×")
                                    .on_click(cx.listener(move |v, _, _, cx| {
                                        v.edit(cx, |d| {
                                            if i < d.repos.len() {
                                                d.repos.remove(i);
                                            }
                                        })
                                    })),
                            ),
                    ),
            );
        }
        f = f
            .child(
                h_flex().child(button("template-repo-add", "Add a Repository", p).on_click(
                    cx.listener(|v, _, window, cx| {
                        let r = RepoDraft {
                            url: input("", "git@github.com:you/repo.git", window, cx),
                            path: input("", "/workspace/repo", window, cx),
                            git_ref: input("", "default branch", window, cx),
                            branch: input("", "none", window, cx),
                        };
                        v.edit(cx, |d| d.repos.push(r));
                    }),
                )),
            )
            .child(
                field("Projects", p).child(div().flex_1().child(Input::new(&d.projects).small())),
            )
            .child(
                field("Trusted folders", p)
                    .child(div().flex_1().child(Input::new(&d.trusted).small())),
            )
            .child(
                field("Web servers", p)
                    .child(div().flex_1().child(Input::new(&d.web_names).small())),
            );

        // What it is given.
        f = f
            .child(section("Kept from its environments", p))
            .child(note(
                "For code its owner does not trust: what each environment made from it is not given of its owner's.",
                p,
            ));
        for (i, (_, label)) in ACCESS.iter().enumerate() {
            f = f.child(
                Checkbox::new(SharedString::from(format!("template-access-{i}")))
                    .label(*label)
                    .checked(d.access[i])
                    .on_click(cx.listener(move |v, b: &bool, _, cx| {
                        let b = *b;
                        v.edit(cx, |d| d.access[i] = b)
                    })),
            );
        }
        f = f.child(
            h_flex().child(
                button(
                    "template-untrusted",
                    "Untrusted Code: Keep All Four Credentials",
                    p,
                )
                .on_click(cx.listener(|v, _, _, cx| {
                    v.edit(cx, |d| {
                        for a in &mut d.access[1..] {
                            *a = true;
                        }
                    })
                })),
            ),
        );

        // Packs.
        f = f.child(section("Packs", p)).child(note(
            "The packs its environments have, before those attaching themselves: a path two of them name is the first's.",
            p,
        ));
        for (i, (pack, copy)) in d.packs.iter().enumerate() {
            let name = self
                .read
                .packs
                .iter()
                .find(|k| &k.id == pack)
                .map_or_else(|| "A pack you cannot see".to_string(), |k| k.name.clone());
            f = f.child(
                form::row(p)
                    .child(div().flex_1().child(name))
                    .child(form::small(
                        copy.as_ref()
                            .map_or("each owner's own copy".to_string(), |c| {
                                format!("pinned to copy {c}")
                            }),
                        p,
                    ))
                    .child(
                        div()
                            .id(SharedString::from(format!("template-pack-remove-{i}")))
                            .px(px(4.))
                            .cursor_pointer()
                            .text_color(p.muted)
                            .child("×")
                            .on_click(cx.listener(move |v, _, _, cx| {
                                v.edit(cx, |d| {
                                    if i < d.packs.len() {
                                        d.packs.remove(i);
                                    }
                                })
                            })),
                    ),
            );
        }
        let addable: Vec<(String, String)> = self
            .read
            .packs
            .iter()
            .filter(|k| !d.packs.iter().any(|(x, _)| x == &k.id))
            .map(|k| (k.name.clone(), k.id.clone()))
            .collect();
        if !addable.is_empty() {
            f = f.child(
                h_flex().child(choice(
                    "template-pack-add",
                    std::iter::once(("Add a Pack".to_string(), String::new()))
                        .chain(addable)
                        .collect(),
                    Some(String::new()),
                    me.clone(),
                    |v: &mut Self, x, _, cx| {
                        if !x.is_empty() {
                            v.edit(cx, |d| d.packs.push((x, None)))
                        }
                    },
                )),
            );
        }

        // Naming and placement.
        f = f
            .child(section("Naming", p))
            .child(
                field("Name pattern", p)
                    .child(div().flex_1().child(Input::new(&d.pattern).small())),
            )
            .child(field("Hint", p).child(div().flex_1().child(Input::new(&d.hint).small())))
            .child(section("Placement", p))
            .child(
                field("Worker labels", p)
                    .child(div().flex_1().child(Input::new(&d.placement).small())),
            );

        // Collaborators.
        if t.is_some() {
            f = f.child(section("Collaborators", p)).child(note(
                "People edit it; a team's viewers use it and its members edit it.",
                p,
            ));
            let mut chips = h_flex().gap(px(6.)).flex_wrap();
            for u in &d.users {
                let name = self
                    .read
                    .people
                    .iter()
                    .find(|x| &x.id == u)
                    .map_or(u.clone(), |x| x.display_name.clone());
                let u = u.clone();
                chips = chips.child(
                    h_flex()
                        .id(SharedString::from(format!("template-collab-{u}")))
                        .gap(px(4.))
                        .child(form::chip(name, p))
                        .child(div().text_color(p.muted).cursor_pointer().child("×"))
                        .on_click(cx.listener(move |v, _, _, cx| {
                            let u = u.clone();
                            v.edit(cx, move |d| d.users.retain(|x| *x != u))
                        })),
                );
            }
            for tm in &d.teams {
                let name = self
                    .read
                    .teams
                    .iter()
                    .find(|x| &x.id == tm)
                    .map_or(tm.clone(), |x| format!("Team {}", x.name));
                let tm = tm.clone();
                chips = chips.child(
                    h_flex()
                        .id(SharedString::from(format!("template-collab-team-{tm}")))
                        .gap(px(4.))
                        .child(form::chip(name, p))
                        .child(div().text_color(p.muted).cursor_pointer().child("×"))
                        .on_click(cx.listener(move |v, _, _, cx| {
                            let tm = tm.clone();
                            v.edit(cx, move |d| d.teams.retain(|x| *x != tm))
                        })),
                );
            }
            let people: Vec<(String, String)> =
                std::iter::once(("Add a Person".to_string(), String::new()))
                    .chain(
                        self.read
                            .people
                            .iter()
                            .filter(|x| x.id != my_id && !d.users.contains(&x.id))
                            .map(|x| {
                                (format!("{} ({})", x.display_name, x.username), x.id.clone())
                            }),
                    )
                    .collect();
            let teams: Vec<(String, String)> =
                std::iter::once(("Add a Team".to_string(), String::new()))
                    .chain(
                        self.read
                            .teams
                            .iter()
                            .filter(|x| !d.teams.contains(&x.id))
                            .map(|x| (x.name.clone(), x.id.clone())),
                    )
                    .collect();
            f = f.child(chips).child(
                h_flex()
                    .gap(px(6.))
                    .child(choice(
                        "template-collab-add",
                        people,
                        Some(String::new()),
                        me.clone(),
                        |v: &mut Self, x, _, cx| {
                            if !x.is_empty() {
                                v.edit(cx, |d| d.users.push(x))
                            }
                        },
                    ))
                    .child(choice(
                        "template-collab-team-add",
                        teams,
                        Some(String::new()),
                        me.clone(),
                        |v: &mut Self, x, _, cx| {
                            if !x.is_empty() {
                                v.edit(cx, |d| d.teams.push(x))
                            }
                        },
                    )),
            );
        }

        f
    }
}
