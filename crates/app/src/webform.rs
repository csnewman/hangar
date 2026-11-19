//! A Hangar's host name and the certificate it serves there, as the person
//! gives them when making it on a Proxmox (Add a place › Proxmox) or later
//! (This Hangar): the name, how its other hosts are named after it, and
//! the certificate (PEM, with its chain) and key files, checked as they
//! are chosen (`hangar::Web::check`). With a host name a Hangar runs a
//! registry, which needs a host name of its own.

use gpui::{
    App, Context, Entity, EventEmitter, IntoElement, ParentElement, Render, Styled, Subscription,
    Window, div, prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{h_flex, v_flex};
use hangar_proxmox::hangar::Web;

use crate::addplace::field;
use crate::palette;
use crate::ui::{button, primary};

/// The largest certificate or key file read: a chain is a few kilobytes.
const MAX_PEM: u64 = 256 << 10;

/// A certificate's or key's file: its name, as shown, and its content.
type Chosen = Option<(String, String)>;

pub struct WebForm {
    host: Entity<InputState>,
    /// The prefix style (registry-<host>), else the subdomain style.
    prefix: bool,
    cert: Chosen,
    key: Chosen,
    /// Why a file chosen could not be read.
    unread: Option<String>,
    _subs: Vec<Subscription>,
}

/// What the form says to the view holding it.
pub enum WebFormEvent {
    Changed,
}

impl EventEmitter<WebFormEvent> for WebForm {}

#[derive(Clone, Copy)]
enum Which {
    Cert,
    Key,
}

impl WebForm {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> WebForm {
        let host = cx.new(|cx| InputState::new(window, cx).placeholder("hangar.example.com"));
        let subs = vec![cx.subscribe(&host, |_, _, e: &InputEvent, cx| {
            if let InputEvent::Change = e {
                cx.emit(WebFormEvent::Changed);
                cx.notify();
            }
        })];
        WebForm {
            host,
            prefix: true,
            cert: None,
            key: None,
            unread: None,
            _subs: subs,
        }
    }

    /// Fills in the host name and style given before; the files are
    /// chosen again.
    pub fn set(&mut self, host: &str, style: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.host
            .update(cx, |s, cx| s.set_value(host.to_string(), window, cx));
        self.prefix = style != "subdomain";
        cx.notify();
    }

    /// What was given: none while no host name is typed; else the host
    /// name and certificate, or why they cannot be served.
    pub fn web(&self, cx: &App) -> Option<Result<Web, String>> {
        let host = self.host.read(cx).value().trim().to_ascii_lowercase();
        if host.is_empty() {
            return None;
        }
        let (Some(cert), Some(key)) = (&self.cert, &self.key) else {
            return Some(Err("Choose the certificate and its key.".into()));
        };
        let w = Web {
            host,
            style: if self.prefix { "prefix" } else { "subdomain" }.into(),
            cert: cert.1.clone(),
            key: key.1.clone(),
        };
        Some(w.check().map(|_| w))
    }

    fn choose(&mut self, which: Which, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let read = cx
                .background_spawn(async move {
                    let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
                    if size > MAX_PEM {
                        return Err(format!("{} is too large for a PEM file.", path.display()));
                    }
                    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
                    let name = path.file_name().map_or_else(
                        || path.display().to_string(),
                        |n| n.to_string_lossy().into(),
                    );
                    Ok((name, text))
                })
                .await;
            let _ = this.update(cx, |f, cx| {
                match read {
                    Ok(c) => {
                        f.unread = None;
                        match which {
                            Which::Cert => f.cert = Some(c),
                            Which::Key => f.key = Some(c),
                        }
                    }
                    Err(e) => f.unread = Some(e),
                }
                cx.emit(WebFormEvent::Changed);
                cx.notify();
            });
        })
        .detach();
    }

    fn style(&mut self, prefix: bool, cx: &mut Context<Self>) {
        self.prefix = prefix;
        cx.emit(WebFormEvent::Changed);
        cx.notify();
    }
}

impl Render for WebForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let host = self.host.read(cx).value().trim().to_ascii_lowercase();
        let shown = if host.is_empty() {
            "hangar.example.com".to_string()
        } else {
            host.clone()
        };
        let domain = shown.split_once('.').map_or(shown.as_str(), |(_, d)| d);
        let pick = |id: &'static str, label: String, on: bool| {
            if on {
                primary(id, label, &p)
            } else {
                button(id, label, &p)
            }
        };
        let file = |id: &'static str, chosen: &Chosen, what: &str, which: Which| {
            h_flex()
                .gap(px(8.))
                .child(
                    button(id, "Choose…", &p)
                        .on_click(cx.listener(move |f, _, _, cx| f.choose(which, cx))),
                )
                .child(match chosen {
                    Some((name, _)) => div().child(name.clone()),
                    None => div().text_color(p.muted).child(what.to_string()),
                })
        };
        let status = match (&self.unread, self.web(cx)) {
            (Some(e), _) => div().text_color(p.bad).child(e.clone()),
            (None, None) => div().text_color(p.muted).child(
                "Optional. Without a host name the Hangar is reached at its address and has no registry.",
            ),
            (None, Some(Ok(w))) => div().text_color(p.good).child(format!(
                "Covers {} and {}. Point them at the Hangar's address in your DNS.",
                w.host,
                w.registry()
            )),
            (None, Some(Err(e))) => div().text_color(p.bad).child(e),
        };
        v_flex()
            .gap(px(8.))
            .child(field("Host name", &p).child(Input::new(&self.host)))
            .child(
                field("Other host names", &p).child(
                    h_flex()
                        .gap(px(6.))
                        .flex_wrap()
                        .child(
                            pick("web-prefix", format!("registry-{shown}"), self.prefix)
                                .on_click(cx.listener(|f, _, _, cx| f.style(true, cx))),
                        )
                        .child(
                            pick("web-subdomain", format!("registry.{shown}"), !self.prefix)
                                .on_click(cx.listener(|f, _, _, cx| f.style(false, cx))),
                        ),
                ),
            )
            .child(
                div()
                    .text_size(px(11.5))
                    .text_color(p.muted)
                    .child(if self.prefix {
                        format!("Beside the host name, under {domain}: a certificate for *.{domain} covers them all.")
                    } else {
                        format!("Under the host name: the certificate covers {shown} and *.{shown}.")
                    }),
            )
            .child(field("Certificate", &p).child(file(
                "web-cert",
                &self.cert,
                "PEM, with its chain",
                Which::Cert,
            )))
            .child(field("Key", &p).child(file("web-key", &self.key, "PEM", Which::Key)))
            .child(div().text_size(px(11.5)).child(status))
    }
}
