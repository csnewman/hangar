//! Settings › a place › Packs: the web UI's Files page in the app. The
//! packs the user may use at the place, and the chosen one's settings and
//! paths (changed with Save Changes), its copies (made, renamed, used by
//! default, deleted), the files of the copy chosen (opened, changed,
//! their mode set, deleted, new ones made), the versions another place's
//! newer change replaced, with Restore, and, for a pack renamed on
//! linking, Merge or Keep Apart.

use std::time::Duration;

use gpui::{
    Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled, Task, Window, div,
    prelude::*, px,
};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputState, Textarea, TextareaState};
use gpui_component::{Sizable, h_flex, v_flex};
use hangar_openapi::clientapi::types;
use serde_json::json;

use crate::api::Client;
use crate::form::{self, choice, field, input, item, note, section, split, text, twice};
use crate::palette::{self, Palette};
use crate::places::Places;
use crate::ui::{button, disabled, primary};

/// How often the packs are read again while the page is open.
const EVERY: Duration = Duration::from_secs(30);

/// How often a path's files may be sent to linked places, as offered:
/// seconds, 0 as written.
const SEND_CHOICES: [i64; 6] = [0, 5, 10, 30, 60, 300];

fn send_label(n: Option<i64>) -> String {
    match n {
        None => "Once a second".into(),
        Some(0) => "As written".into(),
        Some(n) if n % 60 == 0 => format!("At most every {} min", n / 60),
        Some(n) => format!("At most every {n} s"),
    }
}

/// A path as it is being changed.
struct PathDraft {
    path: Entity<InputState>,
    sensitive: bool,
    send_every: Option<i64>,
}

/// A pack's settings as they are being changed.
struct PackDraft {
    name: Entity<InputState>,
    description: Entity<InputState>,
    attach: String,
    squash: String,
    paths: Vec<PathDraft>,
}

impl PackDraft {
    fn new(k: Option<&types::Pack>, window: &mut Window, cx: &mut gpui::App) -> PackDraft {
        PackDraft {
            name: input(
                &k.map_or(String::new(), |k| k.name.clone()),
                "Name",
                window,
                cx,
            ),
            description: input(
                &k.map_or(String::new(), |k| k.description.clone()),
                "What it holds",
                window,
                cx,
            ),
            attach: k.map_or("owner".into(), |k| k.attach.clone()),
            squash: k.map_or("all_squash".into(), |k| k.squash.clone()),
            paths: k
                .map(|k| {
                    k.paths
                        .iter()
                        .map(|x| PathDraft {
                            path: input(&x.path, "~/.config/tool/", window, cx),
                            sensitive: x.sensitive,
                            send_every: x.send_every,
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    fn input(&self, team: Option<&str>, cx: &gpui::App) -> serde_json::Value {
        let mut v = json!({
            "name": text(&self.name, cx),
            "description": text(&self.description, cx),
            "attach": self.attach,
            "squash": self.squash,
            "paths": self.paths.iter().filter(|x| !text(&x.path, cx).is_empty()).map(|x| {
                let mut o = json!({ "path": text(&x.path, cx), "sensitive": x.sensitive });
                if let Some(n) = x.send_every { o["send_every"] = n.into(); }
                o
            }).collect::<Vec<_>>(),
        });
        if let Some(t) = team.filter(|t| !t.is_empty()) {
            v["team_id"] = t.into();
        }
        v
    }
}

/// A file opened from a copy: its path, what it held, and the editor.
struct Open {
    path: String,
    original: String,
    editor: Entity<TextareaState>,
    mode: Entity<InputState>,
    sensitive: bool,
}

pub struct PlacePacksView {
    places: Entity<Places>,
    place: String,
    packs: Vec<types::Pack>,
    teams: Vec<types::Team>,
    loaded: bool,
    /// The pack shown: its ID, or "" for one being made.
    selected: Option<String>,
    draft: Option<PackDraft>,
    /// The owner of a pack being made: "" the user, else a team's ID.
    new_owner: String,
    copies: Vec<types::Copy>,
    copy: Option<String>,
    files: Vec<types::PackFile>,
    history: Vec<types::ReplacedFile>,
    open: Option<Open>,
    /// A file read, waiting for the window to make its editor.
    pending: Option<types::PackFileContent>,
    new_copy: Option<Entity<InputState>>,
    new_copy_owner: String,
    rename: Option<Entity<InputState>>,
    new_file: Option<Entity<InputState>>,
    busy: bool,
    error: Option<String>,
    saved: Option<String>,
    /// The button pressed once, of those that delete.
    armed: Option<String>,
    _poll: Task<()>,
}

/// What reading a pack found: its copies, and of the copy chosen its files
/// and history.
struct PackRead {
    copies: Vec<types::Copy>,
    copy: Option<String>,
    files: Vec<types::PackFile>,
    history: Vec<types::ReplacedFile>,
}

fn read_pack(
    c: &Client,
    pack: &str,
    want: Option<String>,
    default: Option<String>,
) -> Result<PackRead, String> {
    let copies = c.copies(pack).map_err(|e| e.to_string())?;
    let copy = want
        .filter(|w| copies.iter().any(|x| &x.id == w))
        .or_else(|| default.filter(|d| copies.iter().any(|x| &x.id == d)))
        .or_else(|| copies.iter().find(|x| x.personal).map(|x| x.id.clone()))
        .or_else(|| copies.first().map(|x| x.id.clone()));
    let (files, history) = match &copy {
        Some(id) => (
            c.copy_files(id).map(|f| f.files).unwrap_or_default(),
            c.copy_history(id).unwrap_or_default(),
        ),
        None => (vec![], vec![]),
    };
    Ok(PackRead {
        copies,
        copy,
        files,
        history,
    })
}

impl PlacePacksView {
    pub fn new(places: Entity<Places>, place: &str, cx: &mut Context<Self>) -> Self {
        let poll = cx.spawn(async move |this, cx| {
            loop {
                if this.update(cx, |v, cx| v.load(cx)).is_err() {
                    return;
                }
                cx.background_executor().timer(EVERY).await;
            }
        });
        PlacePacksView {
            places,
            place: place.to_string(),
            packs: vec![],
            teams: vec![],
            loaded: false,
            selected: None,
            draft: None,
            new_owner: String::new(),
            copies: vec![],
            copy: None,
            files: vec![],
            history: vec![],
            open: None,
            pending: None,
            new_copy: None,
            new_copy_owner: String::new(),
            rename: None,
            new_file: None,
            busy: false,
            error: None,
            saved: None,
            armed: None,
            _poll: poll,
        }
    }

    fn client(&self, cx: &gpui::App) -> Option<Client> {
        self.places.read(cx).client(&self.place)
    }

    fn admin(&self, cx: &gpui::App) -> bool {
        self.places
            .read(cx)
            .get(&self.place)
            .and_then(|(_, s)| s.me.as_ref().map(|m| m.admin))
            .unwrap_or(false)
    }

    fn pack(&self) -> Option<&types::Pack> {
        let id = self.selected.as_ref()?;
        self.packs.iter().find(|k| &k.id == id)
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(c) = self.client(cx) else {
            self.loaded = true;
            self.error = Some("Sign in to this place to see its packs.".into());
            cx.notify();
            return;
        };
        let task = cx.background_spawn(async move {
            Ok::<_, String>((
                c.packs().map_err(|e| e.to_string())?,
                c.teams().unwrap_or_default(),
            ))
        });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |v, cx| {
                v.loaded = true;
                match r {
                    Ok((packs, teams)) => {
                        v.packs = packs;
                        v.teams = teams;
                        let gone = v
                            .selected
                            .as_ref()
                            .is_some_and(|s| !s.is_empty() && !v.packs.iter().any(|k| &k.id == s));
                        if v.selected.is_none() || gone {
                            let first = v.packs.first().map(|k| k.id.clone());
                            v.select(first, cx);
                        } else {
                            v.read_copies(cx);
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
        if self.selected != id {
            self.draft = None;
            self.copies.clear();
            self.copy = None;
            self.files.clear();
            self.history.clear();
            self.open = None;
            self.armed = None;
            self.saved = None;
        }
        self.selected = id;
        self.error = None;
        self.read_copies(cx);
        cx.notify();
    }

    /// Reads the chosen pack's copies, and the chosen copy's files.
    fn read_copies(&mut self, cx: &mut Context<Self>) {
        let (Some(k), Some(c)) = (self.pack().cloned(), self.client(cx)) else {
            return;
        };
        let want = self.copy.clone();
        let task = cx
            .background_spawn(async move { read_pack(&c, &k.id, want, k.default_copy_id.clone()) });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |v, cx| {
                match r {
                    Ok(read) => {
                        v.copies = read.copies;
                        v.copy = read.copy;
                        v.files = read.files;
                        v.history = read.history;
                    }
                    Err(e) => v.error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Runs a call in the background, then reads again.
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

    /// Asks before deleting: true once the button was pressed twice.
    fn twice(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        if self.armed.as_deref() == Some(key) {
            return true;
        }
        self.armed = Some(key.to_string());
        cx.notify();
        false
    }

    fn save_pack(&mut self, cx: &mut Context<Self>) {
        let (Some(d), Some(c)) = (&self.draft, self.client(cx)) else {
            return;
        };
        let existing = self.pack().map(|k| k.id.clone());
        let body = d.input(existing.is_none().then_some(self.new_owner.as_str()), cx);
        self.busy = true;
        self.error = None;
        cx.notify();
        let task = cx.background_spawn(async move {
            match &existing {
                Some(id) => c.update_pack(
                    id,
                    &serde_json::from_value(body).map_err(|e| e.to_string())?,
                ),
                None => c.create_pack(&serde_json::from_value(body).map_err(|e| e.to_string())?),
            }
            .map_err(|e| e.to_string())
        });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |v, cx| {
                v.busy = false;
                match r {
                    Ok(k) => {
                        v.selected = Some(k.id);
                        v.draft = None;
                        v.saved = Some("Saved.".into());
                        v.load(cx);
                    }
                    Err(e) => v.error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn open_file(&mut self, path: String, cx: &mut Context<Self>) {
        let (Some(copy), Some(c)) = (self.copy.clone(), self.client(cx)) else {
            return;
        };
        let task = cx.background_spawn(async move { c.copy_file(&copy, &path) });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |v, cx| {
                match r {
                    Ok(f) => v.pending = Some(f),
                    Err(e) => v.error = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl PlacePacksView {
    fn save_file(&mut self, cx: &mut Context<Self>) {
        let (Some(o), Some(copy), Some(c)) = (&self.open, self.copy.clone(), self.client(cx))
        else {
            return;
        };
        let content = o.editor.read(cx).value().to_string();
        let mode = i64::from_str_radix(&text(&o.mode, cx), 8).ok();
        let path = o.path.clone();
        self.act(cx, move || {
            c.put_copy_file(&copy, &path, &content, mode)
                .map(|_| ())
                .map_err(|e| e.to_string())
        });
        self.saved = Some("File saved.".into());
    }
}

impl PlacePacksView {
    fn take_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(f) = self.pending.take() else { return };
        let mode = format!("{:o}", f.mode);
        self.open = Some(Open {
            original: f.content.clone(),
            editor: form::textarea(&f.content, window, cx),
            mode: input(&mode, "644", window, cx),
            sensitive: f.sensitive,
            path: f.path,
        });
    }
}

impl Render for PlacePacksView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        if !self.loaded {
            return note("Reading the place's packs…", &p).into_any_element();
        }
        self.take_pending(window, cx);
        let making = self.selected.as_deref() == Some("");
        if self.draft.is_none() && (making || self.pack().is_some()) {
            let k = self.pack().cloned();
            self.draft = Some(PackDraft::new(k.as_ref(), window, cx));
        }
        for slot in [&mut self.new_copy, &mut self.rename, &mut self.new_file] {
            if slot.is_none() {
                *slot = Some(input("", "", window, cx));
            }
        }
        let mut left = v_flex().gap(px(4.));
        for k in &self.packs {
            let on = self.selected.as_deref() == Some(k.id.as_str());
            let id = k.id.clone();
            let whose = if k.builtin {
                "built-in".to_string()
            } else if let Some(t) = &k.team {
                format!("team {t}")
            } else {
                k.owner.clone().unwrap_or_default()
            };
            let lock = k.paths.iter().any(|x| x.sensitive);
            left = left.child(
                item(
                    SharedString::from(format!("place-pack-{}", k.id)),
                    if lock {
                        format!("{} 🔒", k.name)
                    } else {
                        k.name.clone()
                    },
                    whose,
                    None,
                    on,
                    &p,
                )
                .on_click(cx.listener(move |v, _, _, cx| v.select(Some(id.clone()), cx))),
            );
        }
        left = left.child(
            button("place-pack-new", "New Pack", &p)
                .on_click(cx.listener(|v, _, _, cx| v.select(Some(String::new()), cx))),
        );
        let right = if making || self.pack().is_some() {
            self.detail(&p, cx).into_any_element()
        } else {
            note("Choose a pack, or make one.", &p).into_any_element()
        };
        v_flex()
            .gap(px(8.))
            .child(note(
                "The packs you may use here, each a list of paths whose files environments share, and their copies' files. Your own packs are also under Sync › Packs, across your linked places.",
                &p,
            ))
            .child(split(left, right))
            .when_some(self.error.clone(), |d, e| d.child(form::error(e, &p)))
            .into_any_element()
    }
}

impl PlacePacksView {
    fn detail(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let k = self.pack();
        let d = self.draft.as_ref();
        let can = k.is_none_or(|k| k.can_change) && !self.busy;
        let me = cx.entity().downgrade();
        let admin = self.admin(cx);
        let mut out = v_flex().gap(px(6.));

        if let Some(k) = k
            && let Some(other) = k
                .same_name
                .as_ref()
                .and_then(|o| self.packs.iter().find(|x| &x.id == o))
        {
            let (id, id2, name) = (k.id.clone(), k.id.clone(), other.name.clone());
            out = out.child(
                v_flex()
                    .gap(px(6.))
                    .p(px(8.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(p.warn_border)
                    .bg(p.warn_bg)
                    .child(format!(
                        "This pack was made in another of your places with the name of {name}, and was renamed when the places were linked. Merging it into {name} joins their paths and files: of a file both have, the one changed later is kept, and the other is kept in the copy's history for 7 days. The same happens in every linked place."
                    ))
                    .child(
                        h_flex()
                            .gap(px(6.))
                            .child(primary("place-pack-merge", format!("Merge into {name}"), p).on_click(cx.listener(
                                move |v, _, _, cx| {
                                    if let Some(c) = v.client(cx) {
                                        let id = id.clone();
                                        v.act(cx, move || c.merge_pack(&id).map(|_| ()).map_err(|e| e.to_string()));
                                    }
                                },
                            )))
                            .child(button("place-pack-apart", "Keep Apart", p).on_click(cx.listener(
                                move |v, _, _, cx| {
                                    if let Some(c) = v.client(cx) {
                                        let id = id2.clone();
                                        v.act(cx, move || c.keep_pack_apart(&id).map(|_| ()).map_err(|e| e.to_string()));
                                    }
                                },
                            ))),
                    ),
            );
        }
        if let Some(k) = k
            && !k.can_change
        {
            out = out.child(note(
                if k.builtin {
                    "An administrator changes the built-in packs."
                } else {
                    "Only the pack's owner, or its team's members, change it."
                },
                p,
            ));
        }

        // Settings.
        if let Some(d) = d {
            out = out
                .child(section("Settings", p))
                .child(field("Name", p).child(div().flex_1().child(Input::new(&d.name).small())))
                .child(
                    field("Description", p)
                        .child(div().flex_1().child(Input::new(&d.description).small())),
                );
            if k.is_none() {
                let owners: Vec<(String, String)> =
                    std::iter::once(("You".to_string(), String::new()))
                        .chain(
                            self.teams
                                .iter()
                                .filter(|x| {
                                    admin
                                        || x.role.as_ref().is_some_and(|r| {
                                            matches!(r.to_string().as_str(), "member" | "admin")
                                        })
                                })
                                .map(|x| (format!("Team {}", x.name), x.id.clone())),
                        )
                        .collect();
                out = out.child(field("Owner", p).child(choice(
                    "place-pack-owner",
                    owners,
                    Some(self.new_owner.clone()),
                    me.clone(),
                    |v: &mut Self, x, _, cx| {
                        v.new_owner = x;
                        cx.notify()
                    },
                )));
            }
            let team = k.is_some_and(|k| k.team_id.is_some()) || !self.new_owner.is_empty();
            let builtin = k.is_some_and(|k| k.builtin);
            let mut attach = vec![(
                "Only environments whose template lists it".to_string(),
                "listed".to_string(),
            )];
            if builtin {
                attach.push(("Everyone's environments".into(), "everyone".into()));
            } else {
                attach.push(if team {
                    ("The team's members' environments".into(), "team".into())
                } else {
                    ("All your environments".into(), "owner".into())
                });
                if admin {
                    attach.push(("Everyone's environments".into(), "everyone".into()));
                }
            }
            out = out
                .child(field("Attaches to", p).child(choice(
                    "place-pack-attach",
                    attach,
                    Some(d.attach.clone()),
                    me.clone(),
                    |v: &mut Self, x, _, cx| {
                        if let Some(d) = &mut v.draft {
                            d.attach = x;
                        }
                        cx.notify()
                    },
                )))
                .child(field("Ownership", p).child(choice(
                    "place-pack-squash",
                    vec![
                        (
                            "Every caller as the environments' user".into(),
                            "all_squash".to_string(),
                        ),
                        (
                            "Root as nobody, others as themselves".into(),
                            "root_squash".to_string(),
                        ),
                        (
                            "Each caller as themselves".into(),
                            "no_root_squash".to_string(),
                        ),
                    ],
                    Some(d.squash.clone()),
                    me.clone(),
                    |v: &mut Self, x, _, cx| {
                        if let Some(d) = &mut v.draft {
                            d.squash = x;
                        }
                        cx.notify()
                    },
                )));

            // Paths.
            out = out.child(section("Paths", p));
            for (i, x) in d.paths.iter().enumerate() {
                let chosen = x.send_every;
                let excluded = text(&x.path, cx).starts_with('!');
                out =
                    out.child(
                        form::row(p)
                            .child(div().flex_1().min_w_0().child(Input::new(&x.path).small()))
                            .when(excluded, |r| r.child(form::small("excluded", p)))
                            .when(!excluded, |r| {
                                r.child(choice(
                                    SharedString::from(format!("place-path-send-{i}")),
                                    std::iter::once((send_label(None), None))
                                        .chain(
                                            SEND_CHOICES
                                                .iter()
                                                .map(|n| (send_label(Some(*n)), Some(*n))),
                                        )
                                        .collect(),
                                    Some(chosen),
                                    me.clone(),
                                    move |v: &mut Self, n, _, cx| {
                                        if let Some(d) = &mut v.draft
                                            && let Some(x) = d.paths.get_mut(i)
                                        {
                                            x.send_every = n;
                                        }
                                        cx.notify()
                                    },
                                ))
                                .child(
                                    Checkbox::new(SharedString::from(format!(
                                        "place-path-sensitive-{i}"
                                    )))
                                    .label("Sensitive")
                                    .checked(x.sensitive)
                                    .on_click(cx.listener(move |v, b: &bool, _, cx| {
                                        if let Some(d) = &mut v.draft
                                            && let Some(x) = d.paths.get_mut(i)
                                        {
                                            x.sensitive = *b;
                                        }
                                        cx.notify()
                                    })),
                                )
                            })
                            .child(
                                div()
                                    .id(SharedString::from(format!("place-path-remove-{i}")))
                                    .px(px(4.))
                                    .cursor_pointer()
                                    .text_color(p.muted)
                                    .child("×")
                                    .on_click(cx.listener(move |v, _, _, cx| {
                                        if let Some(d) = &mut v.draft
                                            && i < d.paths.len()
                                        {
                                            d.paths.remove(i);
                                        }
                                        cx.notify()
                                    })),
                            ),
                    );
            }
            out = out
                .child(h_flex().gap(px(6.)).child(button("place-path-add", "Add a Path", p).on_click(cx.listener(
                    |v, _, window, cx| {
                        let path = input("", "~/.config/tool/ or !~/.config/tool/cache/", window, cx);
                        if let Some(d) = &mut v.draft {
                            d.paths.push(PathDraft { path, sensitive: false, send_every: None });
                        }
                        cx.notify()
                    },
                ))))
                .child(note(
                    "In the home directory with ~/, or absolute. One ending in / shares a directory; one starting with ! leaves a path in such a directory to each environment. A sensitive path's files are kept from environments given none. A file written in bursts is sent to your linked places at most once a second unless its path says otherwise.",
                    p,
                ));
            if let Some(k) = k
                && k.can_change
                && let defaults = &k.default_paths
                && !defaults.is_empty()
                && defaults
                    .iter()
                    .map(|x| &x.path)
                    .ne(k.paths.iter().map(|x| &x.path))
            {
                let id = k.id.clone();
                let defaults = defaults.clone();
                let (name, desc, attach, squash) = (
                    k.name.clone(),
                    k.description.clone(),
                    k.attach.clone(),
                    k.squash.clone(),
                );
                out = out.child(h_flex().child(
                    button("place-pack-defaults", "Return to Hangar's Paths", p).on_click(
                        cx.listener(move |v, _, _, cx| {
                            let Some(c) = v.client(cx) else { return };
                            let body = types::PackInput {
                                name: name.clone(),
                                description: Some(desc.clone()),
                                attach: Some(attach.clone()),
                                squash: Some(squash.clone()),
                                paths: defaults.clone(),
                                team_id: None,
                            };
                            let id = id.clone();
                            v.draft = None;
                            v.act(cx, move || {
                                c.update_pack(&id, &body)
                                    .map(|_| ())
                                    .map_err(|e| e.to_string())
                            });
                        }),
                    ),
                ));
            }
            let label = if k.is_some() {
                "Save Changes"
            } else {
                "Make Pack"
            };
            out = out.child(
                h_flex()
                    .gap(px(6.))
                    .pt(px(6.))
                    .child(if can {
                        primary("place-pack-save", label, p)
                            .on_click(cx.listener(|v, _, _, cx| v.save_pack(cx)))
                    } else {
                        disabled("place-pack-save", label, p)
                    })
                    .child(
                        button("place-pack-revert", "Revert", p).on_click(cx.listener(
                            |v, _, _, cx| {
                                v.draft = None;
                                if v.pack().is_none() {
                                    v.selected = None;
                                }
                                cx.notify()
                            },
                        )),
                    )
                    .when_some(self.saved.clone(), |d, s| d.child(form::small(s, p)))
                    .child(div().flex_1())
                    .when(k.is_some_and(|k| k.can_delete), |d| {
                        d.child(
                            twice(
                                "place-pack-delete",
                                "Delete Pack…",
                                "Delete it and every copy?",
                                self.armed.as_deref() == Some("pack"),
                                self.busy,
                                p,
                            )
                            .on_click(cx.listener(|v, _, _, cx| {
                                if !v.twice("pack", cx) {
                                    return;
                                }
                                let (Some(k), Some(c)) = (v.pack().cloned(), v.client(cx)) else {
                                    return;
                                };
                                v.selected = None;
                                v.draft = None;
                                v.act(cx, move || c.delete_pack(&k.id).map_err(|e| e.to_string()));
                            })),
                        )
                    }),
            );
        }
        if let Some(k) = k {
            out = out
                .child(self.copies_part(k, p, cx))
                .child(self.files_part(p, cx));
        }
        out
    }

    fn copies_part(
        &self,
        k: &types::Pack,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let me = cx.entity().downgrade();
        let admin = self.admin(cx);
        let default = k.default_copy_id.clone().or_else(|| {
            self.copies
                .iter()
                .find(|c| c.personal)
                .map(|c| c.id.clone())
        });
        let mut out = v_flex().gap(px(4.)).child(section("Copies", p)).child(note(
            "Each copy is one set of the pack's files: everyone has their own, and a shared copy is used by those who choose it.",
            p,
        ));
        for c in &self.copies {
            let on = self.copy.as_deref() == Some(c.id.as_str());
            let name = if c.personal {
                format!("{}'s own", c.owner.clone().unwrap_or_default())
            } else {
                c.name.clone()
            };
            let id = c.id.clone();
            let is_default = default.as_deref() == Some(c.id.as_str());
            let mut r = form::row(p)
                .id(SharedString::from(format!("place-copy-{}", c.id)))
                .cursor_pointer()
                .when(on, |d| d.bg(p.accent_soft).border_color(p.accent))
                .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(name))
                .when_some(c.team.clone(), |d, t| {
                    d.child(form::chip(format!("team {t}"), p))
                })
                .child(div().flex_1())
                .when(is_default, |d| d.child(form::small("used by default", p)));
            if !is_default {
                let (pack, copy) = (k.id.clone(), c.id.clone());
                let personal = c.personal;
                r = r.child(
                    button(
                        SharedString::from(format!("place-copy-default-{}", c.id)),
                        "Use by Default",
                        p,
                    )
                    .on_click(cx.listener(move |v, _, _, cx| {
                        cx.stop_propagation();
                        let Some(cl) = v.client(cx) else { return };
                        let (pack, copy) = (pack.clone(), copy.clone());
                        v.act(cx, move || {
                            cl.set_default_copy(&pack, (!personal).then_some(copy.as_str()))
                                .map_err(|e| e.to_string())
                        });
                    })),
                );
            }
            if c.can_delete {
                let key = format!("copy-{}", c.id);
                let copy = c.id.clone();
                r = r.child(
                    twice(
                        SharedString::from(format!("place-copy-delete-{}", c.id)),
                        "Delete…",
                        "Delete it and its files?",
                        self.armed.as_deref() == Some(key.as_str()),
                        self.busy,
                        p,
                    )
                    .on_click(cx.listener(move |v, _, _, cx| {
                        cx.stop_propagation();
                        if !v.twice(&key, cx) {
                            return;
                        }
                        let Some(cl) = v.client(cx) else { return };
                        let copy = copy.clone();
                        v.copy = None;
                        v.act(cx, move || cl.delete_copy(&copy).map_err(|e| e.to_string()));
                    })),
                );
            }
            out = out.child(r.on_click(cx.listener(move |v, _, _, cx| {
                v.copy = Some(id.clone());
                v.open = None;
                v.read_copies(cx);
            })));
        }
        let owners: Vec<(String, String)> = std::iter::once(("You".to_string(), String::new()))
            .chain(
                self.teams
                    .iter()
                    .filter(|x| {
                        admin
                            || x.role.as_ref().is_some_and(|r| {
                                matches!(r.to_string().as_str(), "member" | "admin")
                            })
                    })
                    .map(|x| (format!("Team {}", x.name), x.id.clone())),
            )
            .collect();
        if let Some(name) = &self.new_copy {
            let pack = k.id.clone();
            out =
                out.child(
                    h_flex()
                        .gap(px(6.))
                        .child(div().w(px(180.)).child(Input::new(name).small()))
                        .child(choice(
                            "place-copy-owner",
                            owners,
                            Some(self.new_copy_owner.clone()),
                            me.clone(),
                            |v: &mut Self, x, _, cx| {
                                v.new_copy_owner = x;
                                cx.notify()
                            },
                        ))
                        .child(button("place-copy-new", "New Shared Copy", p).on_click(
                            cx.listener(move |v, _, window, cx| {
                                let (Some(name), Some(cl)) = (v.new_copy.clone(), v.client(cx))
                                else {
                                    return;
                                };
                                let n = text(&name, cx);
                                if n.is_empty() {
                                    return;
                                }
                                name.update(cx, |s, cx| s.set_value("", window, cx));
                                let mut body = json!({ "name": n });
                                if !v.new_copy_owner.is_empty() {
                                    body["team_id"] = v.new_copy_owner.clone().into();
                                }
                                let pack = pack.clone();
                                v.act(cx, move || {
                                    cl.create_copy(&pack, body)
                                        .map(|_| ())
                                        .map_err(|e| e.to_string())
                                });
                            }),
                        )),
                );
        }
        if let (Some(rename), Some(c)) = (
            &self.rename,
            self.copies
                .iter()
                .find(|c| Some(&c.id) == self.copy.as_ref()),
        ) && !c.personal
            && c.can_write
        {
            let copy = c.id.clone();
            out = out.child(
                h_flex()
                    .gap(px(6.))
                    .child(div().w(px(180.)).child(Input::new(rename).small()))
                    .child(
                        button("place-copy-rename", format!("Rename {}", c.name), p).on_click(
                            cx.listener(move |v, _, window, cx| {
                                let (Some(r), Some(cl)) = (v.rename.clone(), v.client(cx)) else {
                                    return;
                                };
                                let n = text(&r, cx);
                                if n.is_empty() {
                                    return;
                                }
                                r.update(cx, |s, cx| s.set_value("", window, cx));
                                let copy = copy.clone();
                                v.act(cx, move || {
                                    cl.rename_copy(&copy, &n)
                                        .map(|_| ())
                                        .map_err(|e| e.to_string())
                                });
                            }),
                        ),
                    ),
            );
        }
        out
    }

    fn files_part(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(copy) = self
            .copies
            .iter()
            .find(|c| Some(&c.id) == self.copy.as_ref())
        else {
            return v_flex();
        };
        let writable = copy.can_write && !self.busy;
        let mut out = v_flex().gap(px(4.)).child(section("Files", p));
        if self.files.is_empty() {
            out = out.child(note("This copy has no files yet.", p));
        }
        for f in &self.files {
            let on = self.open.as_ref().is_some_and(|o| o.path == f.path);
            let path = f.path.clone();
            let mut marks = vec![format!("{:o}", f.mode), size(f.size)];
            if f.sensitive {
                marks.push("sensitive".into());
            }
            if !f.shared {
                marks.push("no longer shared".into());
            }
            out = out.child(
                form::row(p)
                    .id(SharedString::from(format!("place-file-{}", f.path)))
                    .cursor_pointer()
                    .when(on, |d| d.bg(p.accent_soft).border_color(p.accent))
                    .child(form::mono(f.path.clone()).flex_1().min_w_0().truncate())
                    .child(form::small(marks.join(" · "), p))
                    .on_click(cx.listener(move |v, _, _, cx| v.open_file(path.clone(), cx))),
            );
        }
        if let Some(o) = &self.open {
            let changed = o.editor.read(cx).value().as_ref() != o.original.as_str();
            let key = format!("file-{}", o.path);
            out = out
                .child(
                    h_flex()
                        .mt(px(8.))
                        .gap(px(6.))
                        .child(form::mono(o.path.clone()))
                        .when(o.sensitive, |d| d.child(form::chip("sensitive", p))),
                )
                .child(
                    div()
                        .h(px(220.))
                        .border_1()
                        .border_color(p.border)
                        .rounded(px(5.))
                        .font_family(crate::diff::mono())
                        .text_size(px(11.5))
                        .child(Textarea::new(&o.editor).h(px(218.))),
                )
                .child(
                    h_flex()
                        .gap(px(6.))
                        .child(form::small("Mode", p))
                        .child(div().w(px(60.)).child(Input::new(&o.mode).small()))
                        .child(if writable {
                            primary(
                                "place-file-save",
                                if changed { "Save File" } else { "Save" },
                                p,
                            )
                            .on_click(cx.listener(|v, _, _, cx| v.save_file(cx)))
                        } else {
                            disabled("place-file-save", "Save", p)
                        })
                        .child(button("place-file-close", "Close", p).on_click(cx.listener(
                            |v, _, _, cx| {
                                v.open = None;
                                cx.notify()
                            },
                        )))
                        .child(div().flex_1())
                        .when(writable, |d| {
                            d.child(
                                twice(
                                    "place-file-delete",
                                    "Delete File…",
                                    "Delete it from the copy and its environments?",
                                    self.armed.as_deref() == Some(key.as_str()),
                                    self.busy,
                                    p,
                                )
                                .on_click(cx.listener(
                                    move |v, _, _, cx| {
                                        if !v.twice(&key, cx) {
                                            return;
                                        }
                                        let (Some(o), Some(copy), Some(cl)) =
                                            (v.open.take(), v.copy.clone(), v.client(cx))
                                        else {
                                            return;
                                        };
                                        v.act(cx, move || {
                                            cl.delete_copy_file(&copy, &o.path)
                                                .map_err(|e| e.to_string())
                                        });
                                    },
                                )),
                            )
                        }),
                );
        }
        if writable && let Some(newf) = &self.new_file {
            out = out.child(
                h_flex()
                    .gap(px(6.))
                    .child(div().flex_1().child(Input::new(newf).small()))
                    .child(
                        button("place-file-new", "New File", p).on_click(cx.listener(
                            |v, _, window, cx| {
                                let Some(i) = v.new_file.clone() else { return };
                                let path = text(&i, cx);
                                if path.is_empty() {
                                    return;
                                }
                                i.update(cx, |s, cx| s.set_value("", window, cx));
                                v.open = Some(Open {
                                    original: String::new(),
                                    editor: form::textarea("", window, cx),
                                    mode: input("", "default", window, cx),
                                    sensitive: false,
                                    path,
                                });
                                cx.notify()
                            },
                        )),
                    ),
            );
            out = out.child(form::small(
                "A new file's path, under one of the pack's paths: ~/.npmrc",
                p,
            ));
        }
        if !self.history.is_empty() {
            out = out.child(section("Replaced by another place", p));
            for r in &self.history {
                let entry = r.id.clone();
                let why = if r.merged == Some(true) {
                    "replaced when two packs were merged".to_string()
                } else if r.by_here {
                    "replaced by a restore".to_string()
                } else {
                    format!(
                        "replaced by {}'s newer change",
                        r.by.clone().unwrap_or_else(|| "another place".into())
                    )
                };
                out = out.child(
                    form::row(p)
                        .child(form::mono(r.path.clone()))
                        .child(
                            form::small(
                                format!(
                                    "{why} · kept until {}",
                                    r.kept_until.with_timezone(&chrono::Local).format("%-d %b")
                                ),
                                p,
                            )
                            .flex_1(),
                        )
                        .child(
                            button(
                                SharedString::from(format!("place-restore-{}", r.id)),
                                "Restore",
                                p,
                            )
                            .on_click(cx.listener(
                                move |v, _, _, cx| {
                                    let (Some(copy), Some(cl)) = (v.copy.clone(), v.client(cx))
                                    else {
                                        return;
                                    };
                                    let entry = entry.clone();
                                    v.act(cx, move || {
                                        cl.restore_copy_file(&copy, &entry)
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

fn size(n: i64) -> String {
    match n {
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / 1e6),
        n if n >= 1000 => format!("{:.1} kB", n as f64 / 1e3),
        n => format!("{n} B"),
    }
}
