//! Add Folder to Workspace, as the web's: a path typed with its folders
//! listed under it (Tab completes, the arrows choose), and the git
//! repositories the Code service finds in the home folder, a click putting
//! one in the field and a double click adding it. Adding checks the folder
//! is one, and makes it a project beside the others.

use gpui::{
    App, Context, Entity, Focusable, FontWeight, IntoElement, KeyDownEvent, ParentElement, Render,
    ScrollHandle, SharedString, Styled, Subscription, Window, div, prelude::*, px, svg,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Sizable, WindowExt, h_flex, v_flex};
use serde_json::json;

use crate::assets;
use crate::palette::{self, Palette};
use crate::project::Entry;
use crate::workspace::Workspace;

/// A path typed as the folder it is in (with its slash) and the start of
/// a name in it: "/home/dev/pro" is "/home/dev/" and "pro".
pub fn split(typed: &str) -> (String, String) {
    match typed.rfind('/') {
        Some(i) => (typed[..=i].to_string(), typed[i + 1..].to_string()),
        None => ("/".to_string(), typed.to_string()),
    }
}

/// The folders offered under what is typed: those starting with it, any
/// case, hidden ones only once a dot is typed.
pub fn offered(dirs: &[String], partial: &str) -> Vec<String> {
    let lower = partial.to_lowercase();
    dirs.iter()
        .filter(|d| d.to_lowercase().starts_with(&lower))
        .filter(|d| partial.starts_with('.') || !d.starts_with('.'))
        .cloned()
        .collect()
}

/// The folder to add for what is typed, without its trailing slash: an
/// absolute path not in the workspace already.
pub fn wanted(typed: &str, have: &[String]) -> Result<String, String> {
    let typed = typed.trim();
    let want = match typed.trim_end_matches('/') {
        "" if typed.starts_with('/') => "/".to_string(),
        t => t.to_string(),
    };
    if !want.starts_with('/') {
        return Err("A folder is an absolute path.".into());
    }
    if have.contains(&want) {
        return Err(format!("{want} is in the workspace already."));
    }
    Ok(want)
}

pub struct AddFolder {
    workspace: Entity<Workspace>,
    input: Entity<InputState>,
    /// The folder listed, and its folders.
    parent: String,
    dirs: Vec<String>,
    /// The repositories in the home folder.
    repos: Vec<String>,
    /// The folder chosen with the arrows, of those offered.
    chosen: Option<usize>,
    error: Option<String>,
    scroll: ScrollHandle,
    _subs: Vec<Subscription>,
}

/// Opens the dialog over the window.
pub fn open(workspace: Entity<Workspace>, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| AddFolder::new(workspace, window, cx));
    let focus = view.read(cx).input.focus_handle(cx);
    let first = std::cell::Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if first.replace(false) {
            window.focus(&focus, cx);
        }
        dialog
            .w(px(520.))
            .p_0()
            .min_h(px(0.))
            .centered()
            .close_button(false)
            .overlay(false)
            // Enter is the field's own: it completes or adds.
            .on_ok(|_, _, _| false)
            .child(view.clone())
    });
}

impl AddFolder {
    fn new(workspace: Entity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let home = workspace.read(cx).home().unwrap_or("").to_string();
        let input = cx.new(|cx| {
            let start = format!("{home}/");
            let end = start.len();
            let mut s = InputState::new(window, cx)
                .default_value(start)
                .placeholder("/home/dev/project");
            s.set_selected_range(end..end, cx);
            s
        });
        let subs =
            vec![
                cx.subscribe_in(&input, window, |v, _, e: &InputEvent, window, cx| match e {
                    InputEvent::Change => {
                        v.error = None;
                        v.chosen = None;
                        v.list(cx);
                    }
                    InputEvent::PressEnter { .. } => match v.chosen {
                        Some(_) => v.complete(window, cx),
                        None => v.add(window, cx),
                    },
                    _ => {}
                }),
            ];
        let mut v = AddFolder {
            workspace,
            input,
            parent: String::new(),
            dirs: vec![],
            repos: vec![],
            chosen: None,
            error: None,
            scroll: ScrollHandle::new(),
            _subs: subs,
        };
        v.list(cx);
        v.read_repos(cx);
        v
    }

    fn typed(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    /// The folders offered under what is typed.
    fn shown(&self, cx: &App) -> Vec<String> {
        offered(&self.dirs, &split(&self.typed(cx)).1)
    }

    /// Lists the folder typed into, when it is another than the one listed.
    fn list(&mut self, cx: &mut Context<Self>) {
        let (parent, _) = split(&self.typed(cx));
        if parent == self.parent {
            cx.notify();
            return;
        }
        self.parent = parent.clone();
        self.dirs.clear();
        let Some(code) = self.workspace.read(cx).primary() else {
            return;
        };
        let call = code.call("fs.list", json!({ "path": parent }));
        cx.spawn(async move |this, cx| {
            let dirs: Vec<String> = call
                .await
                .ok()
                .and_then(|v| serde_json::from_value::<Vec<Entry>>(v).ok())
                .unwrap_or_default()
                .into_iter()
                .filter(|e| e.dir)
                .map(|e| e.name)
                .collect();
            let _ = this.update(cx, |v, cx| {
                if v.parent == parent {
                    let mut dirs = dirs;
                    dirs.sort_by_key(|d| d.to_lowercase());
                    v.dirs = dirs;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Asks the Code service for the repositories in the home folder.
    fn read_repos(&mut self, cx: &mut Context<Self>) {
        let Some(code) = self.workspace.read(cx).primary() else {
            return;
        };
        let call = code.call("workspace.repos", json!({}));
        cx.spawn(async move |this, cx| {
            let repos: Vec<String> = call
                .await
                .ok()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            let _ = this.update(cx, |v, cx| {
                v.repos = repos;
                cx.notify();
            });
        })
        .detach();
    }

    fn set(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |i, cx| i.set_value(path, window, cx));
    }

    /// Takes the folder chosen, or the only one offered, into the field.
    fn complete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self.shown(cx);
        let Some(d) = shown.get(self.chosen.unwrap_or(0)) else {
            return;
        };
        let path = format!("{}{d}/", self.parent);
        self.set(path, window, cx);
    }

    /// Adds the folder typed to the workspace, once the Code service has
    /// listed it.
    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.typed(cx);
        self.add_path(typed, window, cx);
    }

    fn add_path(&mut self, typed: String, window: &mut Window, cx: &mut Context<Self>) {
        let want = match wanted(&typed, &self.workspace.read(cx).roots()) {
            Ok(w) => w,
            Err(e) => {
                self.error = Some(e);
                cx.notify();
                return;
            }
        };
        let Some(code) = self.workspace.read(cx).primary() else {
            return;
        };
        let call = code.call("fs.list", json!({ "path": want }));
        let ws = self.workspace.clone();
        cx.spawn_in(window, async move |this, cx| {
            let listed = call.await;
            let _ = this.update_in(cx, |v, window, cx| match listed {
                Err(e) => {
                    v.error = Some(e);
                    cx.notify();
                }
                Ok(_) => {
                    window.close_dialog(cx);
                    crate::treeops::change_workspace(
                        &ws,
                        crate::treeops::Reorder::Add(want),
                        window,
                        cx,
                    );
                }
            });
        })
        .detach();
    }

    /// The arrows choose among the folders offered; Tab completes; Escape
    /// closes.
    fn key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.shown(cx).len();
        match e.keystroke.key.as_str() {
            "tab" if !e.keystroke.modifiers.shift => self.complete(window, cx),
            "down" if n > 0 => {
                self.chosen = Some(self.chosen.map_or(0, |c| (c + 1).min(n - 1)));
                self.reveal();
            }
            "up" if n > 0 => {
                self.chosen = match self.chosen {
                    Some(0) | None => None,
                    Some(c) => Some(c - 1),
                };
                self.reveal();
            }
            "escape" => window.close_dialog(cx),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn reveal(&self) {
        if let Some(c) = self.chosen {
            self.scroll.scroll_to_item(c);
        }
    }

    fn row(
        &self,
        id: impl Into<gpui::ElementId>,
        icon: &'static str,
        name: String,
        detail: Option<String>,
        on: bool,
        p: &Palette,
    ) -> gpui::Stateful<gpui::Div> {
        h_flex()
            .id(id)
            .h(px(24.))
            .flex_none()
            .px(px(12.))
            .gap(px(6.))
            .cursor_pointer()
            .when(on, |d| d.bg(p.selection))
            .when(!on, |d| d.hover(|s| s.bg(p.surface_2)))
            .child(
                svg()
                    .path(assets::icon(icon))
                    .size(px(14.))
                    .flex_none()
                    .text_color(p.accent),
            )
            .child(div().flex_none().child(name))
            .when_some(detail, |d, detail| {
                d.child(div().min_w_0().truncate().text_color(p.muted).child(detail))
            })
    }
}

impl Render for AddFolder {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let shown = self.shown(cx);
        let have = self.workspace.read(cx).roots();
        let ws = self.workspace.read(cx);
        let repos: Vec<(String, String)> = self
            .repos
            .iter()
            .filter(|r| !have.contains(r))
            .map(|r| (r.clone(), ws.shown(r)))
            .collect();
        let mut list = v_flex()
            .id("add-folder-list")
            .h(px(260.))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .py(px(4.));
        for (i, d) in shown.iter().enumerate() {
            let path = format!("{}{d}/", self.parent);
            let add = format!("{}{d}", self.parent);
            list = list.child(
                self.row(
                    ("add-folder-dir", i),
                    "folder",
                    d.clone(),
                    None,
                    self.chosen == Some(i),
                    &p,
                )
                .on_click(cx.listener(
                    move |v, e: &gpui::ClickEvent, window, cx| {
                        if e.click_count() >= 2 {
                            v.add_path(add.clone(), window, cx);
                        } else {
                            v.set(path.clone(), window, cx);
                        }
                    },
                )),
            );
        }
        if shown.is_empty() {
            list = list.child(
                div()
                    .px(px(12.))
                    .py(px(4.))
                    .text_color(p.muted)
                    .child("No folders here."),
            );
        }
        if !repos.is_empty() {
            list = list.child(
                div()
                    .px(px(12.))
                    .pt(px(8.))
                    .pb(px(2.))
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.muted)
                    .child("Repositories on this machine"),
            );
        }
        for (i, (abs, shown_as)) in repos.into_iter().enumerate() {
            let name = abs.rsplit('/').next().unwrap_or(&abs).to_string();
            let path = format!("{abs}/");
            list = list.child(
                self.row(
                    ("add-folder-repo", i),
                    "branch",
                    name,
                    Some(shown_as),
                    false,
                    &p,
                )
                .on_click(cx.listener(
                    move |v, e: &gpui::ClickEvent, window, cx| {
                        if e.click_count() >= 2 {
                            v.add_path(abs.clone(), window, cx);
                        } else {
                            v.set(path.clone(), window, cx);
                        }
                    },
                )),
            );
        }
        v_flex()
            .text_size(px(12.))
            .child(
                h_flex()
                    .px(px(12.))
                    .pt(px(9.))
                    .pb(px(7.))
                    .gap(px(8.))
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.text)
                            .child("Add Folder to Workspace"),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(p.muted)
                            .child("a project beside the others"),
                    ),
            )
            .child(
                h_flex()
                    .h(px(32.))
                    .pl(px(1.))
                    .pr(px(4.))
                    .bg(p.surface)
                    .border_t_1()
                    .border_b_1()
                    .border_color(p.border)
                    .capture_key_down(
                        cx.listener(|v, e: &KeyDownEvent, window, cx| v.key(e, window, cx)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.input).appearance(false)),
                    ),
            )
            .child(list)
            .when_some(self.error.clone(), |d, e| {
                d.child(
                    div()
                        .px(px(12.))
                        .py(px(4.))
                        .text_color(p.bad)
                        .child(SharedString::from(e)),
                )
            })
            .child(
                h_flex()
                    .px(px(12.))
                    .py(px(8.))
                    .gap(px(8.))
                    .border_t_1()
                    .border_color(p.border)
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(p.muted)
                            .child("Tab completes · Enter adds"),
                    )
                    .child(div().flex_1())
                    .child(
                        gpui_component::button::Button::new("add-folder-cancel")
                            .small()
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child({
                        use gpui_component::button::ButtonVariants;
                        gpui_component::button::Button::new("add-folder-add")
                            .small()
                            .primary()
                            .label("Add to Workspace")
                            .on_click(cx.listener(|v, _, window, cx| v.add(window, cx)))
                    }),
            )
    }
}
