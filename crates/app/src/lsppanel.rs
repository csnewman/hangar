//! The language server's chip in the status bar and its panel, as the web
//! editor's: for a server running, what it is, how long it has run, what
//! uses it, its log and the messages passed, and restarting it; for none,
//! why, how to install one, and naming another in the settings.

use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use gpui::{
    App, Context, Entity, EventEmitter, IntoElement, ParentElement, Render, SharedString, Styled,
    Subscription, Task, Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::popover::Popover;
use gpui_component::{Sizable, h_flex, v_flex};
use serde_json::{Value, json};

use crate::languages::{Languages, ServerState};
use crate::palette::{self, Palette};
use crate::workspace::Workspace;

/// How often an open panel reads the server again: its log and messages
/// move.
const EVERY: Duration = Duration::from_secs(2);

/// The most of a server's log the panel shows, its last lines.
const LOG_LINES: usize = 400;

/// What the panel asks of the environment's tab.
#[derive(Debug, Clone, PartialEq)]
pub enum Ask {
    /// Runs a command in a new terminal session.
    Run(String),
    /// Opens a file in an editor.
    Open(String),
}

/// The status bar's chip: the file's language server, starting, running
/// or missing; clicked, its panel.
pub struct ServerChip {
    languages: Entity<Languages>,
    workspace: Entity<Workspace>,
    file: Option<String>,
    panel: Option<Entity<LspPanel>>,
    _sub: Subscription,
}

impl EventEmitter<Ask> for ServerChip {}

impl ServerChip {
    pub fn new(
        languages: Entity<Languages>,
        workspace: Entity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        let sub = cx.observe(&languages, |_, _, cx| cx.notify());
        ServerChip {
            languages,
            workspace,
            file: None,
            panel: None,
            _sub: sub,
        }
    }

    /// The file the editors show.
    pub fn set_file(&mut self, abs: Option<String>, cx: &mut Context<Self>) {
        if self.file != abs {
            self.file = abs;
            self.panel = None;
            cx.notify();
        }
    }

    fn set_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.panel = None;
        if open
            && let Some(key) = self
                .file
                .as_ref()
                .and_then(|f| self.languages.read(cx).server_of(f, cx))
        {
            let me = cx.entity().downgrade();
            let (languages, workspace) = (self.languages.clone(), self.workspace.clone());
            self.panel = Some(cx.new(|cx| LspPanel::new(me, languages, workspace, key, cx)));
        }
        cx.notify();
    }

    fn ask(&mut self, ask: Ask, cx: &mut Context<Self>) {
        self.panel = None;
        cx.emit(ask);
        cx.notify();
    }
}

impl Render for ServerChip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let Some(file) = self.file.clone() else {
            return div().into_any_element();
        };
        let langs = self.languages.read(cx);
        let key = langs.server_of(&file, cx);
        let state = key.as_ref().and_then(|k| langs.state(k));
        let (text, tip, missing) = match (&key, &state) {
            (Some(k), Some(ServerState::Ready)) => (
                langs.server_name(k),
                format!(
                    "{} knows this file's code. Click for its details.",
                    langs.server_name(k)
                ),
                false,
            ),
            (Some(k), Some(ServerState::Starting)) => {
                (format!("{} starting…", k.1), String::new(), false)
            }
            (Some(k), Some(ServerState::Unavailable(why))) => (
                format!("No {} server", k.1),
                format!("{why} Click for what to do."),
                true,
            ),
            _ => (
                crate::docedit::language(&file).to_string(),
                String::new(),
                false,
            ),
        };
        let clickable = matches!(
            state,
            Some(ServerState::Ready | ServerState::Unavailable(_))
        );
        if !clickable {
            return div().child(text).into_any_element();
        }
        let me = cx.entity().downgrade();
        let panel = self.panel.clone();
        Popover::new("lsp-chip")
            .anchor(gpui::Anchor::BottomRight)
            .open(panel.is_some())
            .on_open_change(move |open, _, cx| {
                let _ = me.update(cx, |c, cx| c.set_open(*open, cx));
            })
            .trigger(
                Button::new("lsp-chip-button")
                    .ghost()
                    .xsmall()
                    .debug_selector(|| "lsp-chip".into())
                    .label(text)
                    .text_size(px(11.))
                    .text_color(if missing { p.bad } else { p.muted })
                    .tooltip(tip),
            )
            .content(move |_, _, _| match &panel {
                Some(p) => p.clone().into_any_element(),
                None => div().into_any_element(),
            })
            .into_any_element()
    }
}

/// The panel: what lsp.info says, read again while it is open.
pub struct LspPanel {
    chip: gpui::WeakEntity<ServerChip>,
    languages: Entity<Languages>,
    workspace: Entity<Workspace>,
    key: (String, String),
    info: Option<Value>,
    error: Option<String>,
    log: bool,
    _poll: Task<()>,
}

impl LspPanel {
    fn new(
        chip: gpui::WeakEntity<ServerChip>,
        languages: Entity<Languages>,
        workspace: Entity<Workspace>,
        key: (String, String),
        cx: &mut Context<Self>,
    ) -> Self {
        let poll = cx.spawn(async move |this, cx| {
            loop {
                let Ok(Some(call)) = this.update(cx, |p, cx| p.languages.read(cx).info(&p.key, cx))
                else {
                    return;
                };
                let got = call.await;
                if this
                    .update(cx, |p, cx| {
                        match got {
                            Ok(v) => p.info = Some(v),
                            Err(e) => p.error = Some(e),
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
                cx.background_executor().timer(EVERY).await;
            }
        });
        LspPanel {
            chip,
            languages,
            workspace,
            key,
            info: None,
            error: None,
            log: false,
            _poll: poll,
        }
    }

    fn ask(&self, ask: Ask, cx: &mut App) {
        let _ = self.chip.update(cx, |c, cx| c.ask(ask, cx));
    }

    /// Opens the settings file, written first if there is none, for naming
    /// a server in its languageServers.
    fn choose(&mut self, cx: &mut Context<Self>) {
        let ws = self.workspace.read(cx);
        let (Some(path), Some(code)) =
            (ws.settings_path().map(str::to_string), ws.code(&self.key.0))
        else {
            self.error = Some("The environment did not say where the settings are.".into());
            cx.notify();
            return;
        };
        cx.spawn(async move |this, cx| {
            let saved = match code.call("settings.get", json!({})).await {
                Ok(s) => code.call("settings.set", json!({ "settings": s })).await,
                Err(e) => Err(e),
            };
            let _ = this.update(cx, |p, cx| match saved {
                Ok(_) => p.ask(Ask::Open(path), cx),
                Err(e) => {
                    p.error = Some(e);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn running(&self, p: &Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        let info = self.info.clone().unwrap_or_default();
        let s = |k: &str| info[k].as_str().unwrap_or_default().to_string();
        let command = std::iter::once(s("path"))
            .chain(
                info["args"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|a| a.as_str().map(str::to_string)),
            )
            .collect::<Vec<_>>()
            .join(" ");
        let since = millis(&info["started"])
            .map(|t| crate::model::ago(t, Utc::now()))
            .unwrap_or_default();
        let pid = info["pid"].as_u64().map(|n| format!(" · pid {n}"));
        let (tabs, docs) = (
            info["tabs"].as_u64().unwrap_or(0),
            info["documents"].as_u64().unwrap_or(0),
        );
        let plural = |n: u64, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
        let fact = |label: &'static str, value: gpui::AnyElement| {
            h_flex()
                .gap(px(10.))
                .items_start()
                .child(
                    div()
                        .w(px(74.))
                        .flex_none()
                        .text_color(p.muted)
                        .child(label),
                )
                .child(div().flex_1().min_w_0().child(value))
        };
        let mono = crate::diff::mono();
        let copied = command.clone();
        let facts = v_flex()
            .gap(px(4.))
            .child(fact(
                "Running",
                h_flex()
                    .child(format!("since {since}"))
                    .when_some(pid, |d, pid| d.child(div().text_color(p.muted).child(pid)))
                    .into_any_element(),
            ))
            .child(fact(
                "Command",
                h_flex()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .font_family(mono)
                            .text_size(px(11.))
                            .child(command),
                    )
                    .child(
                        Button::new("lsp-copy-command")
                            .ghost()
                            .xsmall()
                            .icon(gpui_component::Icon::empty().path(crate::assets::icon("copy")))
                            .tooltip("Copy the command")
                            .on_click(move |_, _, cx| crate::treeops::copy(copied.clone(), cx)),
                    )
                    .into_any_element(),
            ))
            .child(fact(
                "Folder",
                div()
                    .font_family(mono)
                    .text_size(px(11.))
                    .child(s("root"))
                    .into_any_element(),
            ))
            .child(fact(
                "Used by",
                div()
                    .child(format!(
                        "{} connected, {} open",
                        plural(tabs, "editor"),
                        plural(docs, "file")
                    ))
                    .into_any_element(),
            ));
        let log = self.log;
        let tabs = h_flex()
            .gap(px(4.))
            .child(
                crate::gitlog::chip("lsp-messages", "Messages".into(), !log, p).on_click(
                    cx.listener(|v, _, _, cx| {
                        v.log = false;
                        cx.notify();
                    }),
                ),
            )
            .child(
                crate::gitlog::chip("lsp-log", "Log".into(), log, p).on_click(cx.listener(
                    |v, _, _, cx| {
                        v.log = true;
                        cx.notify();
                    },
                )),
            )
            .child(div().flex_1())
            .when(log, |d| {
                let all = s("log");
                d.child(
                    Button::new("lsp-copy-log")
                        .ghost()
                        .xsmall()
                        .label("Copy")
                        .on_click(move |_, _, cx| crate::treeops::copy(all.clone(), cx)),
                )
            });
        let lines: Vec<gpui::AnyElement> = if log {
            let text = s("log");
            let all: Vec<&str> = text.trim().lines().collect();
            if all.is_empty() {
                vec![
                    div()
                        .text_color(p.muted)
                        .child("The server has written nothing to its log.")
                        .into_any_element(),
                ]
            } else {
                all[all.len().saturating_sub(LOG_LINES)..]
                    .iter()
                    .map(|l| div().child(l.to_string()).into_any_element())
                    .collect()
            }
        } else {
            let trace: Vec<Value> = info["trace"].as_array().cloned().unwrap_or_default();
            if trace.is_empty() {
                vec![
                    div()
                        .text_color(p.muted)
                        .child("No messages yet.")
                        .into_any_element(),
                ]
            } else {
                trace.iter().rev().map(|m| traced(m, p)).collect()
            }
        };
        v_flex()
            .gap(px(8.))
            .child(facts)
            .child(tabs)
            .child(
                div()
                    .id("lsp-lines")
                    .h(px(200.))
                    .overflow_y_scroll()
                    .p(px(6.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(p.border)
                    .bg(p.surface_2)
                    .font_family(mono)
                    .text_size(px(11.))
                    .children(lines),
            )
            .into_any_element()
    }

    fn missing(
        &self,
        why: Option<String>,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let info = self.info.clone().unwrap_or_default();
        // The service's own words first: the state's add how to install,
        // which the rows below show.
        let why = info["missing"]
            .as_str()
            .map(|m| format!("{m}."))
            .or(why)
            .unwrap_or_else(|| format!("The {} server is not running.", self.key.1));
        let installs: Vec<(String, Option<String>)> = info["install"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|i| {
                (
                    i["command"].as_str().unwrap_or_default().to_string(),
                    i["install"].as_str().map(str::to_string),
                )
            })
            .collect();
        let installed = (info["running"] == json!(false) && info.get("install").is_none())
            .then(|| (info["command"].as_str(), info["path"].as_str()))
            .and_then(|(c, at)| Some(format!("{} is installed at {}.", c?, at?)));
        let mut why = why;
        if let Some(first) = why.get(..1) {
            why = first.to_uppercase() + &why[1..];
        }
        // Installed and not running: it stopped, rather than never started.
        let stopped = installed.is_some();
        let mono = crate::diff::mono();
        let rows = installs.into_iter().enumerate().map(|(i, (command, how))| {
            h_flex()
                .gap(px(8.))
                .child(
                    div()
                        .w(px(180.))
                        .flex_none()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(command),
                )
                .child(match how {
                    Some(how) => {
                        let (run, copied) = (how.clone(), how.clone());
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(4.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .font_family(mono)
                                    .text_size(px(11.))
                                    .child(how),
                            )
                            .child(
                                Button::new(("lsp-run", i))
                                    .ghost()
                                    .xsmall()
                                    .icon(
                                        gpui_component::Icon::empty()
                                            .path(crate::assets::icon("terminal")),
                                    )
                                    .tooltip("Run it in a new terminal session")
                                    .on_click(cx.listener(move |v, _, _, cx| {
                                        v.ask(Ask::Run(run.clone()), cx)
                                    })),
                            )
                            .child(
                                Button::new(("lsp-copy", i))
                                    .ghost()
                                    .xsmall()
                                    .icon(
                                        gpui_component::Icon::empty()
                                            .path(crate::assets::icon("copy")),
                                    )
                                    .tooltip("Copy")
                                    .on_click(move |_, _, cx| {
                                        crate::treeops::copy(copied.clone(), cx)
                                    }),
                            )
                            .into_any_element()
                    }
                    None => div()
                        .text_color(p.muted)
                        .child("see its documentation")
                        .into_any_element(),
                })
        });
        let has_rows = info["install"].as_array().is_some_and(|a| !a.is_empty());
        let key = self.key.clone();
        v_flex()
            .gap(px(8.))
            .child(div().child(why))
            .when(has_rows, |d| {
                d.child(
                    div()
                        .text_color(p.muted)
                        .child("Install one, then try again:"),
                )
                .children(rows)
            })
            .when_some(installed, |d, t| {
                d.child(div().text_color(p.muted).child(t))
            })
            .child(
                h_flex()
                    .gap(px(6.))
                    .child(
                        Button::new("lsp-retry")
                            .primary()
                            .small()
                            .label(if stopped { "Start again" } else { "Try again" })
                            .on_click(cx.listener(move |v, _, _, cx| {
                                v.languages.update(cx, |l, cx| l.retry(&key, cx));
                                let _ = v.chip.update(cx, |c, cx| c.set_open(false, cx));
                            })),
                    )
                    .child(
                        Button::new("lsp-choose")
                            .small()
                            .label("Choose a server…")
                            .tooltip(format!(
                                "Name a server for {} in the settings' languageServers",
                                self.key.1
                            ))
                            .on_click(cx.listener(|v, _, _, cx| v.choose(cx))),
                    ),
            )
            .into_any_element()
    }
}

/// A message passed, newest first: when, which way, its method or the id
/// it answers, how long it took, and its error.
fn traced(m: &Value, p: &Palette) -> gpui::AnyElement {
    let at = millis(&m["t"])
        .map(|t| t.with_timezone(&Local).format("%H:%M:%S").to_string())
        .unwrap_or_default();
    let what = match (m["method"].as_str(), m["id"].as_str()) {
        (Some(method), _) => method.to_string(),
        (None, Some(id)) => format!("answer {id}"),
        (None, None) => format!("answer {}", m["id"]),
    };
    let error = m["error"].as_str().map(str::to_string);
    h_flex()
        .gap(px(6.))
        .child(div().text_color(p.muted).child(at))
        .child(if m["dir"] == "to" { "→" } else { "←" })
        .child(
            div()
                .when(error.is_some(), |d| d.text_color(p.bad))
                .child(what),
        )
        .when(m["kind"] == "response" && m["method"].is_string(), |d| {
            d.child(div().text_color(p.muted).child("answered"))
        })
        .when_some(m["ms"].as_u64(), |d, ms| {
            d.child(div().text_color(p.muted).child(format!("{ms} ms")))
        })
        .when_some(error, |d, e| {
            d.child(div().min_w_0().truncate().text_color(p.bad).child(e))
        })
        .into_any_element()
}

/// A time the service gives as milliseconds since the epoch, which it
/// sends as a float.
fn millis(v: &Value) -> Option<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp_millis(v.as_f64()? as i64)
}

/// A server's version for the panel's head. gopls gives its whole build as
/// JSON; its module's version is what a person means.
pub fn short_version(v: &str) -> String {
    if v.starts_with('{')
        && let Ok(b) = serde_json::from_str::<Value>(v)
        && let Some(found) = b["Main"]["Version"]
            .as_str()
            .or_else(|| b["Version"].as_str())
    {
        return found.to_string();
    }
    if v.chars().count() > 40 {
        format!("{}…", v.chars().take(40).collect::<String>())
    } else {
        v.to_string()
    }
}

impl Render for LspPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let state = self.languages.read(cx).state(&self.key);
        let running = self.info.as_ref().is_some_and(|i| i["running"] == true);
        let name = self
            .info
            .as_ref()
            .and_then(|i| i["serverInfo"]["name"].as_str().map(str::to_string))
            .unwrap_or_else(|| self.languages.read(cx).server_name(&self.key));
        let version = self
            .info
            .as_ref()
            .and_then(|i| i["serverInfo"]["version"].as_str().map(short_version));
        let ready = state == Some(ServerState::Ready);
        let title: SharedString = if running || ready {
            name.into()
        } else {
            format!("No {} language server", self.key.1).into()
        };
        let key = self.key.clone();
        let head = h_flex()
            .gap(px(6.))
            .child(
                div()
                    .size(px(7.))
                    .rounded_full()
                    .bg(if running { p.good } else { p.idle }),
            )
            .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
            .when_some(version.filter(|_| running), |d, v| {
                d.child(div().text_color(p.muted).child(v))
            })
            .when(running, |d| {
                d.child(div().text_color(p.muted).child(format!("· {}", self.key.1)))
            })
            .child(div().flex_1())
            .when(running, |d| {
                d.child(
                    Button::new("lsp-restart")
                        .small()
                        .icon(gpui_component::Icon::empty().path(crate::assets::icon("refresh")))
                        .label("Restart")
                        .on_click(cx.listener(move |v, _, _, cx| {
                            v.languages.update(cx, |l, cx| l.restart(&key, cx));
                            let _ = v.chip.update(cx, |c, cx| c.set_open(false, cx));
                        })),
                )
            });
        let why = match state {
            Some(ServerState::Unavailable(why)) => Some(why),
            _ => None,
        };
        let body = if running {
            self.running(&p, cx)
        } else if self.info.is_some() || why.is_some() {
            self.missing(why, &p, cx)
        } else {
            div()
                .text_color(p.muted)
                .child("Asking the environment…")
                .into_any_element()
        };
        v_flex()
            .w(px(520.))
            .gap(px(10.))
            .p(px(4.))
            .text_size(px(12.))
            .child(head)
            .when_some(self.error.clone(), |d, e| {
                d.child(div().text_color(p.bad).child(e))
            })
            .child(body)
    }
}
