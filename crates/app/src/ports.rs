//! The Ports tool window: the TCP ports the environment listens on, read
//! from its Code service every few seconds while shown, each with the
//! program listening and the address it is reached on from here,
//! <port>-env<short> on the server's host; its web servers by name, which
//! reach port 80; and who may reach them, public or private.

use std::time::Duration;

use gpui::{
    App, Context, Entity, FontWeight, IntoElement, ParentElement, Render, SharedString, Styled,
    WeakEntity, Window, div, prelude::*, px, svg,
};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::{h_flex, v_flex};
use serde::Deserialize;
use serde_json::json;

use crate::api::Environment;
use crate::assets;
use crate::gitlog::chip;
use crate::palette::{self, Palette};
use crate::places::Places;
use crate::workspace::{State, Workspace};

/// A port listened on, as the Code service's ports.list gives it.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct Port {
    pub port: u16,
    /// Whether only the machine itself can reach it.
    pub loopback: bool,
    pub pid: Option<u32>,
    pub command: Option<String>,
}

impl Port {
    /// The system's own: reached by the machine alone, by a process the
    /// environment's user cannot see, as its DNS resolver's.
    pub fn system(&self) -> bool {
        self.loopback && self.pid.is_none()
    }

    /// The program listening, its path cut to the name.
    pub fn program(&self) -> Option<String> {
        let c = self.command.as_deref()?;
        let (first, rest) = c.split_once(' ').unwrap_or((c, ""));
        let name = first.rsplit('/').next().unwrap_or(first);
        Some(if rest.is_empty() {
            name.to_string()
        } else {
            format!("{name} {rest}")
        })
    }
}

/// The ports a list shows: the system's own left out.
pub fn shown(ports: &[Port]) -> Vec<Port> {
    ports.iter().filter(|p| !p.system()).cloned().collect()
}

/// Where a host of an environment's is reached: a name or a port number
/// before env<short>, none for its default web server, in the scheme of the
/// server's own address.
pub fn host_url(server: &str, label: Option<&str>, short: &str, suffix: &str) -> String {
    let scheme = server.split_once("://").map_or("https", |(s, _)| s);
    let label = label.map(|l| format!("{l}-")).unwrap_or_default();
    format!("{scheme}://{label}env{short}{suffix}/")
}

/// How often the ports are read while the window is shown.
const POLL: Duration = Duration::from_secs(3);

pub struct PortsView {
    workspace: Entity<Workspace>,
    places: Entity<Places>,
    place: String,
    env: String,
    ports: Option<Vec<Port>>,
    error: Option<String>,
    reading: bool,
    paused: bool,
    /// Whether a change of who may reach them is being made.
    setting: bool,
}

impl PortsView {
    pub fn new(
        workspace: Entity<Workspace>,
        places: Entity<Places>,
        place: &str,
        env: &str,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&places, |_, _, cx| cx.notify()).detach();
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                let alive = this.update(cx, |v, cx| {
                    if !v.paused {
                        v.read(cx)
                    }
                });
                if alive.is_err() {
                    return;
                }
            }
        })
        .detach();
        let mut v = PortsView {
            workspace,
            places,
            place: place.to_string(),
            env: env.to_string(),
            ports: None,
            error: None,
            reading: false,
            paused: false,
            setting: false,
        };
        v.read(cx);
        v
    }

    pub fn ports(&self) -> Option<&[Port]> {
        self.ports.as_deref()
    }

    pub fn paused(&self) -> bool {
        self.paused
    }

    /// Stops reading while the window is hidden, and reads once shown.
    pub fn set_paused(&mut self, paused: bool, cx: &mut Context<Self>) {
        if self.paused != paused {
            self.paused = paused;
            if !paused {
                self.read(cx);
            }
        }
    }

    /// What the window's title says beside its name.
    pub fn summary(&self) -> Option<String> {
        let n = shown(self.ports.as_deref()?).len();
        Some(match n {
            1 => "1 listening".into(),
            n => format!("{n} listening"),
        })
    }

    /// Asks the environment what it listens on.
    pub fn read(&mut self, cx: &mut Context<Self>) {
        if self.reading {
            return;
        }
        let Some(code) = self.workspace.read(cx).primary() else {
            return;
        };
        self.reading = true;
        let call = code.call("ports.list", json!({}));
        cx.spawn(async move |this, cx| {
            let got = call
                .await
                .and_then(|v| serde_json::from_value::<Vec<Port>>(v).map_err(|e| e.to_string()));
            let _ = this.update(cx, |v, cx| {
                v.reading = false;
                match got {
                    Ok(ports) => {
                        v.ports = Some(ports);
                        v.error = None;
                    }
                    Err(e) => v.error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The environment, the server's address, and the hosts' suffix.
    fn about(&self, cx: &App) -> Option<(Environment, String, Option<String>)> {
        let (config, state) = self.places.read(cx).get(&self.place)?;
        let env = state.environment(&self.env)?.clone();
        let suffix = state
            .me
            .as_ref()
            .and_then(|m| m.environment_host_suffix.clone());
        Some((env, config.url.clone(), suffix))
    }

    /// Says who may reach the environment's web servers; the event stream
    /// brings the change back.
    pub fn set_public(&mut self, public: bool, cx: &mut Context<Self>) {
        let Some(client) = self.places.read(cx).client(&self.place) else {
            return;
        };
        self.setting = true;
        let env = self.env.clone();
        let task = cx.background_spawn(async move { client.set_ports_public(&env, public) });
        cx.spawn(async move |this, cx| {
            let got = task.await;
            let _ = this.update(cx, |v, cx| {
                v.setting = false;
                v.error = got.err().map(|e| e.to_string());
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn address(
        &self,
        id: impl Into<SharedString>,
        label: String,
        about: Option<String>,
        url: Option<String>,
        p: &Palette,
    ) -> gpui::AnyElement {
        let id: SharedString = id.into();
        let hover = p.surface_2;
        let icon = |name: &str| {
            svg()
                .path(assets::icon(name))
                .size(px(12.))
                .text_color(p.muted)
        };
        let top = h_flex()
            .gap(px(8.))
            .child(
                div()
                    .flex_none()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(label.clone()),
            )
            .when_some(about, |d, about| {
                d.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(11.))
                        .text_color(p.muted)
                        .child(about),
                )
            });
        let row = v_flex()
            .id(id.clone())
            .debug_selector({
                let id = id.clone();
                move || id.to_string()
            })
            .px(px(6.))
            .py(px(4.))
            .gap(px(2.))
            .rounded(px(4.))
            .hover(move |s| s.bg(hover))
            .child(top);
        let Some(url) = url else {
            return row.into_any_element();
        };
        let (open, copy, menu_url, menu_copy) =
            (url.clone(), url.clone(), url.clone(), url.clone());
        row.child(
            h_flex()
                .gap(px(6.))
                .child(
                    div()
                        .id(SharedString::from(format!("{id}-url")))
                        .min_w_0()
                        .truncate()
                        .text_size(px(11.))
                        .text_color(p.accent)
                        .cursor_pointer()
                        .child(url.trim_end_matches('/').to_string())
                        .on_click(move |_, _, cx| cx.open_url(&open)),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .id(SharedString::from(format!("{id}-copy")))
                        .debug_selector({
                            let at = format!("{id}-copy");
                            move || at
                        })
                        .cursor_pointer()
                        .child(icon("copy"))
                        .tooltip(|window, cx| {
                            gpui_component::tooltip::Tooltip::new("Copy URL").build(window, cx)
                        })
                        .on_click(move |_, _, cx| crate::treeops::copy(copy.clone(), cx)),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("{id}-open")))
                        .cursor_pointer()
                        .child(icon("external"))
                        .tooltip(|window, cx| {
                            gpui_component::tooltip::Tooltip::new("Open in Browser")
                                .build(window, cx)
                        })
                        .on_click({
                            let url = url.clone();
                            move |_, _, cx| cx.open_url(&url)
                        }),
                ),
        )
        .context_menu(move |menu, _, cx| {
            crate::ui::modal(cx);
            let (open, copy) = (menu_url.clone(), menu_copy.clone());
            let port = label.clone();
            menu.item(
                PopupMenuItem::new("Open in Browser").on_click(move |_, _, cx| cx.open_url(&open)),
            )
            .item(
                PopupMenuItem::new("Copy URL")
                    .on_click(move |_, _, cx| crate::treeops::copy(copy.clone(), cx)),
            )
            .item(
                PopupMenuItem::new("Copy Port")
                    .on_click(move |_, _, cx| crate::treeops::copy(port.clone(), cx)),
            )
        })
        .into_any_element()
    }
}

fn heading(text: &str, p: &Palette) -> gpui::Div {
    div()
        .px(px(6.))
        .pt(px(10.))
        .pb(px(2.))
        .text_size(px(10.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(p.muted)
        .child(text.to_uppercase())
}

fn note(text: impl Into<SharedString>, p: &Palette) -> gpui::Div {
    div()
        .px(px(6.))
        .py(px(4.))
        .text_size(px(11.5))
        .text_color(p.muted)
        .child(text.into())
}

impl Render for PortsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let about = self.about(cx);
        let mut body = v_flex().gap(px(1.));

        if let Some((env, _, _)) = &about {
            let public = env.ports_public;
            let enabled = !self.setting;
            body = body.child(
                h_flex()
                    .gap(px(4.))
                    .px(px(4.))
                    .child(
                        chip("ports-private", "Private".into(), !public, &p)
                            .debug_selector(|| "ports-private".into())
                            .when(enabled && public, |d| {
                                d.on_click(cx.listener(|v, _, _, cx| v.set_public(false, cx)))
                            }),
                    )
                    .child(
                        chip("ports-public", "Public".into(), public, &p)
                            .debug_selector(|| "ports-public".into())
                            .when(enabled && !public, |d| {
                                d.on_click(cx.listener(|v, _, _, cx| v.set_public(true, cx)))
                            }),
                    ),
            );
            body = body.child(note(
                if public {
                    "Anyone who can reach Hangar, without signing in. A port by its number is always private."
                } else {
                    "People who may use this environment, signed in to Hangar."
                },
                &p,
            ));
        }
        if let Some(e) = &self.error {
            body = body.child(
                div()
                    .px(px(6.))
                    .text_size(px(11.))
                    .text_color(p.bad)
                    .child(e.clone()),
            );
        }

        let state = self.workspace.read(cx).state().clone();
        body = body.child(heading("Listening", &p));
        match &self.ports {
            None => {
                body = body.child(note(
                    match &state {
                        State::Failed(e) => format!("Could not reach the Code service: {e}"),
                        State::Reconnecting(why) => State::reconnecting(why),
                        _ => "Reading…".into(),
                    },
                    &p,
                ))
            }
            Some(ports) => {
                let list = shown(ports);
                if list.is_empty() {
                    body = body.child(note(
                        "Nothing listens on a port. A server started in a terminal shows here.",
                        &p,
                    ));
                }
                for port in list {
                    let url = about.as_ref().and_then(|(env, server, suffix)| {
                        let suffix = suffix.as_ref()?;
                        Some(host_url(
                            server,
                            Some(&port.port.to_string()),
                            &env.short_id,
                            suffix,
                        ))
                    });
                    body = body.child(self.address(
                        format!("port-{}", port.port),
                        port.port.to_string(),
                        port.program(),
                        url,
                        &p,
                    ));
                }
            }
        }

        if let Some((env, server, Some(suffix))) = &about {
            body = body.child(heading("Web servers", &p));
            body = body.child(note("Each reaches port 80; HTTPS its 443.", &p));
            let names = std::iter::once(None).chain(env.web_names.iter().map(Some));
            for name in names {
                let url = host_url(server, name.map(String::as_str), &env.short_id, suffix);
                let label = name.cloned().unwrap_or_else(|| "default".into());
                body = body.child(self.address(format!("web-{label}"), label, None, Some(url), &p));
            }
        } else if about.is_some() {
            body = body.child(note(
                "This server has no public address, so the environment's web servers have none either.",
                &p,
            ));
        }

        div()
            .id("ports")
            .size_full()
            .min_h_0()
            .overflow_y_scroll()
            .text_size(px(12.))
            .child(body)
    }
}
