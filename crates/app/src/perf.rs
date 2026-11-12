//! The Performance tool window, as the web's: what the environment uses
//! (its CPU, memory, disk and network, each with its last two minutes
//! drawn), its processes, by what they use or as the tree of which started
//! which, each stopped (TERM) or killed from its menu, and its activity:
//! what has been done to it, newest first, each a sentence.

use std::collections::{HashMap, VecDeque};
use std::ops::Range;
use std::time::Duration;

use chrono::{DateTime, Utc};
use gpui::{
    Bounds, Context, FocusHandle, Focusable, FontWeight, Hsla, IntoElement, ParentElement, Pixels,
    Render, Styled, WeakEntity, Window, canvas, div, point, prelude::*, px, uniform_list,
};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::{h_flex, v_flex};

use crate::api::{ActivityEvent, Client, Process, ProcessList, Stats};
use crate::docker::bytes;
use crate::gitlog::chip;
use crate::palette::{self, Palette};

/// How often what it uses and its processes are read while shown.
const POLL: Duration = Duration::from_secs(2);
/// How many readings the charts keep: two minutes of them.
const KEPT: usize = 60;
/// How many events of its activity are read at a time.
const PAGE: usize = 50;

/// How the processes are ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Cpu,
    Memory,
    Name,
    Pid,
}

/// The window's two views.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Processes,
    Activity,
}

/// The processes in the order shown, each with how deep it is in the tree
/// (0 when listed flat): by the order asked, busiest first for CPU and
/// memory; as a tree, each under the process that started it, siblings in
/// that order.
pub fn ordered(list: &[Process], sort: Sort, tree: bool) -> Vec<(usize, usize)> {
    let key = |a: &Process, b: &Process| match sort {
        Sort::Cpu => b
            .cpu_percent
            .total_cmp(&a.cpu_percent)
            .then(a.pid.cmp(&b.pid)),
        Sort::Memory => b.rss_bytes.cmp(&a.rss_bytes).then(a.pid.cmp(&b.pid)),
        Sort::Name => a
            .name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.pid.cmp(&b.pid)),
        Sort::Pid => a.pid.cmp(&b.pid),
    };
    let mut idx: Vec<usize> = (0..list.len()).collect();
    idx.sort_by(|&a, &b| key(&list[a], &list[b]));
    if !tree {
        return idx.into_iter().map(|i| (i, 0)).collect();
    }
    let pids: std::collections::HashSet<i64> = list.iter().map(|p| p.pid).collect();
    let mut children: HashMap<i64, Vec<usize>> = HashMap::new();
    let mut roots = Vec::new();
    for i in idx {
        let parent = list[i].ppid;
        if parent != list[i].pid && pids.contains(&parent) {
            children.entry(parent).or_default().push(i);
        } else {
            roots.push(i);
        }
    }
    let mut out = Vec::new();
    let mut stack: Vec<(usize, usize)> = roots.into_iter().rev().map(|i| (i, 0)).collect();
    while let Some((i, depth)) = stack.pop() {
        out.push((i, depth));
        if let Some(kids) = children.get(&list[i].pid) {
            stack.extend(kids.iter().rev().map(|&k| (k, depth + 1)));
        }
    }
    out
}

/// Events that say someone looked, rather than that anything changed, as
/// the web's environment page leaves them out; a phase change but a
/// failure is the worker's account of a start.
pub fn notable(e: &ActivityEvent) -> bool {
    const QUIET: [&str; 9] = [
        "environment.open_code",
        "environment.open_editor",
        "environment.open_desktop",
        "environment.open_ports",
        "environment.terminal_attach",
        "environment.terminal_close",
        "environment.ssh_connect",
        "environment.ssh_disconnect",
        "environment.place",
    ];
    !QUIET.contains(&e.action.as_str())
        && !(e.action == "environment.phase" && e.details["to"] != "failed")
}

/// An event in words, as the web says it: what was done, and what more
/// there is to say.
pub fn describe(e: &ActivityEvent) -> (String, Option<String>) {
    let d = &e.details;
    let text = |k: &str| d[k].as_str().filter(|s| !s.is_empty()).map(str::to_string);
    let what = match e.action.as_str() {
        "environment.create" => {
            let detail = match (d["cpus"].as_i64(), d["memory_mib"].as_i64()) {
                (Some(c), Some(m)) => Some(format!("{c} vCPUs · {}", memory(m))),
                _ => None,
            };
            return ("created it".into(), detail);
        }
        "environment.start" if d["from"] == "suspended" => "resumed it",
        "environment.start" => "started it",
        "environment.stop" => "stopped it",
        "environment.suspend" => "suspended it",
        "environment.delete" | "environment.deleted" => "deleted it",
        "environment.update_settings" => {
            let mut changed = vec![];
            for (key, label) in [
                ("cpus", "vCPUs"),
                ("memory_mib", "memory"),
                ("display", "display"),
            ] {
                let c = &d[key];
                if c.is_object() && c["from"] != c["to"] {
                    let show = |v: &serde_json::Value| match (key, v) {
                        ("memory_mib", v) => v.as_i64().map(memory).unwrap_or_default(),
                        ("display", v) if v == "none" => "headless".into(),
                        (_, serde_json::Value::String(s)) => s.clone(),
                        (_, v) => v.to_string(),
                    };
                    changed.push(format!("{label} {} → {}", show(&c["from"]), show(&c["to"])));
                }
            }
            let what = if d["running"] == true {
                "resized its memory as it ran"
            } else {
                "changed its settings"
            };
            return (
                what.into(),
                (!changed.is_empty()).then(|| changed.join(" · ")),
            );
        }
        "environment.reset_to_template" => "reset it to its template",
        "environment.ports_public" if d["public"] == true => "made its web servers public",
        "environment.ports_public" => "made its web servers private",
        "environment.image_upgrade" => "chose to upgrade its image at its next start",
        "environment.image_rollback" => "chose to roll its image back at its next start",
        "environment.image_keep" => "kept its upgraded image",
        "environment.image_change_cancel" => "cancelled the change to its image",
        "environment.choose_copy" => "chose which copy of a pack it uses",
        "environment.resolve_conflict" => "settled a file that became shared",
        "environment.unplaceable" => return ("could not place it yet".into(), text("reason")),
        "environment.phase" => return ("reported it failed".into(), text("reason")),
        "environment.signal_process" => {
            let signal = text("signal").unwrap_or_else(|| "a signal".into());
            let pid = d["pid"].as_i64().map(|p| p.to_string()).unwrap_or_default();
            return (
                format!("sent {signal} to process {pid}").trim().to_string(),
                text("error"),
            );
        }
        "environment.ssh_refused" => "was refused SSH",
        other => {
            let verb = other.split_once('.').map_or(other, |(_, v)| v);
            return (verb.replace('_', " "), None);
        }
    };
    (what.into(), None)
}

/// MiB as the web shows memory: GB from 1024 on.
pub fn memory(mib: i64) -> String {
    if mib >= 1024 {
        let g = mib as f64 / 1024.;
        if g.fract() == 0. {
            format!("{g:.0} GB")
        } else {
            format!("{g:.1} GB")
        }
    } else {
        format!("{mib} MB")
    }
}

/// A rate in bytes a second.
fn rate(bps: f64) -> String {
    format!("{}/s", bytes(bps))
}

pub struct PerformanceView {
    client: Client,
    env: String,
    /// The readings, oldest first, the last what it uses at the moment.
    history: VecDeque<Stats>,
    list: Option<Result<ProcessList, String>>,
    sort: Sort,
    tree: bool,
    selected: Option<i64>,
    pane: Pane,
    /// Its activity, as read so far, whether there is more, and why it
    /// could not be read.
    activity: Vec<ActivityEvent>,
    more: bool,
    activity_error: Option<String>,
    /// Who is signed in, to say "you" for.
    me: Option<String>,
    /// Whether the window is hidden, which stops the reading.
    paused: bool,
    busy: Option<String>,
    focus: FocusHandle,
}

impl PerformanceView {
    pub fn new(client: Client, env: &str, cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            loop {
                let read = this.update(cx, |v, cx| {
                    if !v.paused {
                        v.read(cx);
                    }
                });
                if read.is_err() {
                    return;
                }
                cx.background_executor().timer(POLL).await;
            }
        })
        .detach();
        let me = client.clone();
        let view = PerformanceView {
            client,
            env: env.to_string(),
            history: VecDeque::new(),
            list: None,
            sort: Sort::Cpu,
            tree: false,
            selected: None,
            pane: Pane::Processes,
            activity: vec![],
            more: false,
            activity_error: None,
            me: None,
            paused: false,
            busy: None,
            focus: cx.focus_handle(),
        };
        cx.spawn(async move |this, cx| {
            let me = cx.background_spawn(async move { me.me() }).await;
            if let Ok(me) = me {
                let _ = this.update(cx, |v, cx| {
                    v.me = Some(me.username);
                    cx.notify();
                });
            }
        })
        .detach();
        view
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
        let s = self.history.back()?;
        Some(format!(
            "CPU {:.0}% · {} of {}",
            s.cpu_percent,
            memory(s.memory_used_mib),
            memory(s.memory_total_mib)
        ))
    }

    /// Reads what it uses and its processes, or its activity when that is
    /// shown.
    fn read(&mut self, cx: &mut Context<Self>) {
        let (client, env) = (self.client.clone(), self.env.clone());
        let pane = self.pane;
        let task = cx.background_spawn(async move {
            let stats = client.stats(&env);
            let list = (pane == Pane::Processes).then(|| client.processes(&env));
            (stats, list)
        });
        cx.spawn(async move |this, cx| {
            let (stats, list) = task.await;
            let _ = this.update(cx, |v, cx| {
                if let Ok(Some(s)) = stats {
                    v.history.push_back(s);
                    while v.history.len() > KEPT {
                        v.history.pop_front();
                    }
                }
                if let Some(list) = list {
                    v.list = Some(list.map_err(|e| e.to_string()));
                }
                cx.notify();
            });
        })
        .detach();
        if pane == Pane::Activity && self.activity.is_empty() {
            self.read_activity(None, cx);
        }
    }

    /// Reads a page of its activity: the newest, or what came before.
    fn read_activity(&mut self, before: Option<i64>, cx: &mut Context<Self>) {
        let (client, env) = (self.client.clone(), self.env.clone());
        let task = cx.background_spawn(async move { client.activity(&env, before, PAGE) });
        cx.spawn(async move |this, cx| {
            let page = task.await;
            let _ = this.update(cx, |v, cx| {
                match page {
                    Ok(page) => {
                        v.more = page.len() == PAGE;
                        if before.is_none() {
                            v.activity = page;
                        } else {
                            v.activity.extend(page);
                        }
                        v.activity_error = None;
                        // Most of the log can be looks rather than changes:
                        // read on until there is enough to show, as the
                        // web's page does, a few pages at most.
                        let shown = v.activity.iter().filter(|e| notable(e)).count();
                        if v.more && shown < 10 && v.activity.len() < PAGE * 10 {
                            let last = v.activity.last().map(|e| e.id);
                            v.read_activity(last, cx);
                        }
                    }
                    Err(e) => v.activity_error = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn show(&mut self, pane: Pane, cx: &mut Context<Self>) {
        self.pane = pane;
        if pane == Pane::Activity {
            self.read_activity(None, cx);
        } else {
            self.read(cx);
        }
        cx.notify();
    }

    /// Sends a process a signal, saying in the bar what came of it.
    fn signal(&mut self, pid: i64, signal: &'static str, cx: &mut Context<Self>) {
        let (client, env) = (self.client.clone(), self.env.clone());
        self.busy = Some(format!("Sending {signal} to {pid}…"));
        let task = cx.background_spawn(async move { client.signal_process(&env, pid, signal) });
        cx.spawn(async move |this, cx| {
            let done = task.await;
            let _ = this.update(cx, |v, cx| {
                v.busy = match done {
                    Ok(()) => None,
                    Err(e) => Some(format!("Could not send {signal} to {pid}: {e}")),
                };
                v.read(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Asks before a signal, naming the process.
    fn ask_signal(
        me: WeakEntity<Self>,
        p: &Process,
        signal: &'static str,
        window: &mut Window,
        cx: &mut gpui::App,
    ) {
        let pid = p.pid;
        let (title, about, verb) = match signal {
            "KILL" => (
                format!("Kill {} ({pid})?", p.name),
                "It is stopped at once, with no chance to save its work.",
                "Kill",
            ),
            _ => (
                format!("Stop {} ({pid})?", p.name),
                "It is asked to stop (TERM), as a program asked to quit.",
                "Stop",
            ),
        };
        crate::treeops::confirm(&title, about, verb, window, cx, move |_, cx| {
            let _ = me.update(cx, |v, cx| v.signal(pid, signal, cx));
        });
    }

    /// The tiles along the top: each measure, at the moment and over the
    /// last two minutes.
    fn tiles(&self, p: &Palette) -> gpui::AnyElement {
        let Some(now) = self.history.back().copied() else {
            return div()
                .px(px(10.))
                .py(px(8.))
                .text_color(p.muted)
                .child("Reading what it uses…")
                .into_any_element();
        };
        let series = |f: fn(&Stats) -> f64| -> Vec<f64> { self.history.iter().map(f).collect() };
        let total = now.memory_total_mib.max(1) as f64;
        h_flex()
            .flex_none()
            .gap(px(8.))
            .px(px(8.))
            .py(px(6.))
            .child(tile(
                "CPU",
                format!("{:.0}%", now.cpu_percent),
                series(|s| s.cpu_percent),
                100.,
                p.accent,
                p,
            ))
            .child(tile(
                "Memory",
                format!(
                    "{} of {}",
                    memory(now.memory_used_mib),
                    memory(now.memory_total_mib)
                ),
                series(|s| s.memory_used_mib as f64),
                total,
                p.good,
                p,
            ))
            .child(tile(
                "Disk",
                format!(
                    "{} · ↓{} ↑{}",
                    bytes(now.disk_used_bytes as f64),
                    rate(now.disk_read_bps),
                    rate(now.disk_write_bps)
                ),
                series(|s| s.disk_read_bps + s.disk_write_bps),
                0.,
                p.busy,
                p,
            ))
            .child(tile(
                "Network",
                format!("↓{} ↑{}", rate(now.net_rx_bps), rate(now.net_tx_bps)),
                series(|s| s.net_rx_bps + s.net_tx_bps),
                0.,
                p.wait,
                p,
            ))
            .into_any_element()
    }

    fn header(
        &self,
        label: &'static str,
        sort: Sort,
        width: Option<f32>,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let on = self.sort == sort;
        div()
            .id(label)
            .when_some(width, |d, w| d.w(px(w)).flex_none().text_right())
            .when(width.is_none(), |d| d.flex_1().min_w_0())
            .cursor_pointer()
            .text_color(if on { p.text } else { p.muted })
            .when(on, |d| d.font_weight(FontWeight::SEMIBOLD))
            .child(if on {
                format!("{label} ▾")
            } else {
                label.to_string()
            })
            .on_click(cx.listener(move |v, _, _, cx| {
                v.sort = sort;
                cx.notify();
            }))
            .into_any_element()
    }

    fn processes(&self, p: &Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        let list = match &self.list {
            None => return note("Reading its processes…", p),
            Some(Err(e)) => return note(e.clone(), p),
            Some(Ok(l)) => l,
        };
        let order = ordered(&list.processes, self.sort, self.tree);
        let head = h_flex()
            .flex_none()
            .h(px(22.))
            .px(px(10.))
            .gap(px(10.))
            .border_b_1()
            .border_color(p.border)
            .text_size(px(11.))
            .child(self.header("Name", Sort::Name, None, p, cx))
            .child(self.header("PID", Sort::Pid, Some(56.), p, cx))
            .child(
                div()
                    .w(px(70.))
                    .flex_none()
                    .text_color(p.muted)
                    .child("User"),
            )
            .child(self.header("CPU", Sort::Cpu, Some(56.), p, cx))
            .child(self.header("Memory", Sort::Memory, Some(76.), p, cx))
            .child(
                div()
                    .w(px(56.))
                    .flex_none()
                    .text_right()
                    .text_color(p.muted)
                    .child("Threads"),
            );
        let rows = uniform_list(
            "perf-processes",
            order.len(),
            cx.processor(move |v, range: Range<usize>, _, cx| {
                let p = palette::of(cx);
                let Some(Ok(list)) = &v.list else {
                    return vec![];
                };
                let order = ordered(&list.processes, v.sort, v.tree);
                range
                    .filter_map(|k| order.get(k).copied())
                    .map(|(i, depth)| v.process_row(&list.processes[i], depth, &p, cx))
                    .collect::<Vec<_>>()
            }),
        )
        .flex_1();
        v_flex()
            .flex_1()
            .min_h_0()
            .child(head)
            .child(rows)
            .into_any_element()
    }

    fn process_row(
        &self,
        proc: &Process,
        depth: usize,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let pid = proc.pid;
        let selected = self.selected == Some(pid);
        let me = cx.entity().downgrade();
        let menu_proc = proc.clone();
        let tip = proc.command.clone();
        h_flex()
            .id(("process", pid as u64))
            .w_full()
            .h(px(20.))
            .px(px(10.))
            .gap(px(10.))
            .cursor_pointer()
            .when(selected, |d| d.bg(p.selection))
            .when(!selected, |d| d.hover(|s| s.bg(p.surface_2)))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .gap(px(6.))
                    .pl(px(depth as f32 * 12.))
                    .child(
                        div()
                            .flex_none()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(proc.name.clone()),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(p.muted)
                            .child(proc.command.clone()),
                    ),
            )
            .child(
                div()
                    .w(px(56.))
                    .flex_none()
                    .text_right()
                    .text_color(p.muted)
                    .child(pid.to_string()),
            )
            .child(
                div()
                    .w(px(70.))
                    .flex_none()
                    .truncate()
                    .text_color(p.muted)
                    .child(proc.user.clone()),
            )
            .child(
                div()
                    .w(px(56.))
                    .flex_none()
                    .text_right()
                    .child(format!("{:.1}", proc.cpu_percent)),
            )
            .child(
                div()
                    .w(px(76.))
                    .flex_none()
                    .text_right()
                    .child(bytes(proc.rss_bytes as f64)),
            )
            .child(
                div()
                    .w(px(56.))
                    .flex_none()
                    .text_right()
                    .text_color(p.muted)
                    .child(proc.threads.to_string()),
            )
            .tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
            })
            .on_click(cx.listener(move |v, _, window, cx| {
                window.focus(&v.focus, cx);
                v.selected = Some(pid);
                cx.notify();
            }))
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(move |v, _, _, cx| {
                    v.selected = Some(pid);
                    cx.notify();
                }),
            )
            .context_menu(move |menu, _, cx| {
                crate::ui::modal(cx);
                let p = menu_proc.clone();
                let (stop, kill) = (me.clone(), me.clone());
                let (ps, pk) = (p.clone(), p.clone());
                let (pid, command) = (p.pid.to_string(), p.command.clone());
                menu.item(
                    PopupMenuItem::new("Stop (TERM)…")
                        .disabled(p.protected)
                        .on_click(move |_, window, cx| {
                            Self::ask_signal(stop.clone(), &ps, "TERM", window, cx)
                        }),
                )
                .item(
                    PopupMenuItem::new("Kill (KILL)…")
                        .disabled(p.protected)
                        .on_click(move |_, window, cx| {
                            Self::ask_signal(kill.clone(), &pk, "KILL", window, cx)
                        }),
                )
                .separator()
                .item(
                    PopupMenuItem::new("Copy PID")
                        .on_click(move |_, _, cx| crate::treeops::copy(pid.clone(), cx)),
                )
                .item(
                    PopupMenuItem::new("Copy Command")
                        .on_click(move |_, _, cx| crate::treeops::copy(command.clone(), cx)),
                )
            })
            .into_any_element()
    }

    fn activity_list(&self, p: &Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        if let Some(e) = &self.activity_error {
            return note(format!("Could not read its activity: {e}"), p);
        }
        let shown: Vec<&ActivityEvent> = self.activity.iter().filter(|e| notable(e)).collect();
        let mut list = v_flex()
            .id("perf-activity")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(10.))
            .py(px(6.))
            .gap(px(6.));
        if shown.is_empty() {
            list = list.child(div().text_color(p.muted).child(
                if self.activity.is_empty() && self.more {
                    "Reading…"
                } else {
                    "Nothing has been done to it yet."
                },
            ));
        }
        let now = Utc::now();
        for e in shown {
            let (what, detail) = describe(e);
            let who = if self.me.as_deref() == Some(e.actor.name.as_str()) {
                "You".to_string()
            } else if e.actor.kind == "worker" {
                format!("Worker {}", e.actor.name)
            } else {
                e.actor.name.clone()
            };
            list = list.child(
                v_flex()
                    .gap(px(1.))
                    .child(
                        h_flex()
                            .gap(px(6.))
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(who))
                            .child(div().child(what))
                            .child(div().flex_1())
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(p.muted)
                                    .child(ago(e.at, now)),
                            ),
                    )
                    .when_some(detail, |d, detail| {
                        d.child(div().text_size(px(11.)).text_color(p.muted).child(detail))
                    }),
            );
        }
        if self.more {
            let last = self.activity.last().map(|e| e.id);
            list = list.child(
                div()
                    .id("perf-activity-more")
                    .text_color(p.accent)
                    .cursor_pointer()
                    .child("Show older")
                    .on_click(cx.listener(move |v, _, _, cx| v.read_activity(last, cx))),
            );
        }
        list.into_any_element()
    }
}

/// How long ago, as the app says it elsewhere.
fn ago(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    crate::model::ago(at, now)
}

/// One measure: its name, its reading, and its last readings drawn, scaled
/// to top (or to their own highest, for 0).
fn tile(
    name: &'static str,
    value: String,
    series: Vec<f64>,
    top: f64,
    colour: Hsla,
    p: &Palette,
) -> gpui::AnyElement {
    v_flex()
        .flex_1()
        .min_w(px(120.))
        .p(px(6.))
        .gap(px(2.))
        .rounded(px(6.))
        .border_1()
        .border_color(p.border)
        .child(
            h_flex()
                .gap(px(6.))
                .child(div().text_size(px(11.)).text_color(p.muted).child(name))
                .child(div().flex_1())
                .child(div().text_size(px(11.)).truncate().child(value)),
        )
        .child(
            canvas(
                |_, _, _| {},
                move |b: Bounds<Pixels>, _, window, _| sparkline(&series, top, b, colour, window),
            )
            .h(px(28.))
            .w_full(),
        )
        .into_any_element()
}

/// Draws readings as a line across bounds, the newest at the right.
fn sparkline(series: &[f64], top: f64, b: Bounds<Pixels>, colour: Hsla, window: &mut Window) {
    if series.len() < 2 {
        return;
    }
    let high = if top > 0. {
        top
    } else {
        series.iter().cloned().fold(1., f64::max)
    };
    let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
    let step = w / (KEPT - 1) as f32;
    let start = w - step * (series.len() - 1) as f32;
    let at = |i: usize, v: f64| {
        let y = h - (v / high).clamp(0., 1.) as f32 * (h - 2.) - 1.;
        point(b.origin.x + px(start + step * i as f32), b.origin.y + px(y))
    };
    let mut path = gpui::PathBuilder::stroke(px(1.5));
    path.move_to(at(0, series[0]));
    for (i, v) in series.iter().enumerate().skip(1) {
        path.line_to(at(i, *v));
    }
    if let Ok(path) = path.build() {
        window.paint_path(path, colour);
    }
}

fn note(text: impl Into<gpui::SharedString>, p: &Palette) -> gpui::AnyElement {
    div()
        .px(px(10.))
        .py(px(6.))
        .text_color(p.muted)
        .child(text.into())
        .into_any_element()
}

impl Focusable for PerformanceView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for PerformanceView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let pane = self.pane;
        let tree = self.tree;
        let bar = h_flex()
            .flex_none()
            .gap(px(4.))
            .px(px(8.))
            .h(px(26.))
            .child(
                chip(
                    "perf-processes-tab",
                    "Processes".into(),
                    pane == Pane::Processes,
                    &p,
                )
                .on_click(cx.listener(|v, _, _, cx| v.show(Pane::Processes, cx))),
            )
            .child(
                chip(
                    "perf-activity-tab",
                    "Activity".into(),
                    pane == Pane::Activity,
                    &p,
                )
                .on_click(cx.listener(|v, _, _, cx| v.show(Pane::Activity, cx))),
            )
            .when(pane == Pane::Processes, |d| {
                d.child(div().w(px(1.)).h(px(14.)).mx(px(4.)).bg(p.border))
                    .child(
                        chip("perf-tree", "Tree".into(), tree, &p)
                            .tooltip(|window, cx| {
                                gpui_component::tooltip::Tooltip::new(
                                    "Each process under the one that started it",
                                )
                                .build(window, cx)
                            })
                            .on_click(cx.listener(|v, _, _, cx| {
                                v.tree = !v.tree;
                                cx.notify();
                            })),
                    )
            })
            .child(div().flex_1())
            .when_some(self.busy.clone(), |d, b| {
                d.child(
                    div()
                        .text_size(px(11.))
                        .text_color(p.muted)
                        .truncate()
                        .child(b),
                )
            });
        v_flex()
            .size_full()
            .min_h_0()
            .text_size(px(12.))
            .track_focus(&self.focus)
            .child(self.tiles(&p))
            .child(bar)
            .child(match pane {
                Pane::Processes => self.processes(&p, cx),
                Pane::Activity => self.activity_list(&p, cx),
            })
    }
}
