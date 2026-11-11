//! The Log tool window: one of the logs an environment's worker keeps of it
//! from its last start (its console, what the worker said about it, its
//! monitor's, its GPU's, or all of them as one), its end first and then
//! what is written, read every two seconds. The worker keeps them whatever
//! the environment is doing, so a start that failed can be looked into.

use std::time::Duration;

use gpui::{
    Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled, WeakEntity, Window,
    div, prelude::*, px,
};
use gpui_component::{h_flex, v_flex};

use crate::api::{Client, LogPart};
use crate::gitlog::chip;
use crate::logview::LogPane;
use crate::palette;

/// An environment's logs, in the order they are offered: the name the API
/// knows each by, its label, and what it holds.
pub const LOGS: [(&str, &str, &str); 5] = [
    (
        "combined",
        "Combined",
        "Every log below as one, each line marked with the log it is from, in the order the worker saw them written.",
    ),
    (
        "console",
        "Console",
        "The machine's serial console: the kernel, Hangar's init assembling the root, and systemd.",
    ),
    (
        "worker",
        "Worker",
        "What its worker logged about it: each step of starting, and any error.",
    ),
    (
        "monitor",
        "Monitor",
        "The virtual machine monitor's own log.",
    ),
    (
        "gpu",
        "GPU",
        "The virtual GPU's backend, for an environment with one.",
    ),
];

/// How often a log is read on from where it was.
const POLL: Duration = Duration::from_secs(2);

pub struct LogsView {
    client: Client,
    env: String,
    chosen: &'static str,
    pane: Entity<LogPane>,
    /// Where to read on from, once the end has been read.
    next: Option<i64>,
    /// Whether the log's oldest lines are gone.
    earlier: bool,
    error: Option<String>,
    reading: bool,
    paused: bool,
    /// Counts the logs chosen, so a read of one chosen before is dropped.
    asked: u64,
}

impl LogsView {
    pub fn new(client: Client, env: &str, cx: &mut Context<Self>) -> Self {
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
        let mut v = LogsView {
            client,
            env: env.to_string(),
            chosen: LOGS[0].0,
            pane: cx.new(|cx| LogPane::new("env-log", "Reading…", cx)),
            next: None,
            earlier: false,
            error: None,
            reading: false,
            paused: false,
            asked: 0,
        };
        v.read(cx);
        v
    }

    pub fn chosen(&self) -> &'static str {
        self.chosen
    }

    /// Whether the log's oldest lines are gone.
    pub fn earlier(&self) -> bool {
        self.earlier
    }

    pub fn pane(&self) -> &Entity<LogPane> {
        &self.pane
    }

    /// Shows another log, from its end.
    pub fn choose(&mut self, log: &str, cx: &mut Context<Self>) {
        let Some(&(name, _, _)) = LOGS.iter().find(|(n, _, _)| *n == log) else {
            return;
        };
        if name == self.chosen {
            return;
        }
        self.chosen = name;
        self.asked += 1;
        self.next = None;
        self.earlier = false;
        self.error = None;
        self.reading = false;
        self.pane.update(cx, |p, cx| {
            p.clear(cx);
            p.set_empty("Reading…", cx);
        });
        self.read(cx);
        cx.notify();
    }

    /// Stops reading while the window is hidden, and reads on once shown.
    pub fn set_paused(&mut self, paused: bool, cx: &mut Context<Self>) {
        if self.paused != paused {
            self.paused = paused;
            if !paused {
                self.read(cx);
            }
        }
    }

    pub fn paused(&self) -> bool {
        self.paused
    }

    /// Reads what follows what has been read, or the end.
    pub fn read(&mut self, cx: &mut Context<Self>) {
        if self.reading {
            return;
        }
        self.reading = true;
        let (client, env, log, next, asked) = (
            self.client.clone(),
            self.env.clone(),
            self.chosen,
            self.next,
            self.asked,
        );
        let task = cx.background_spawn(async move { client.environment_log(&env, log, next) });
        cx.spawn(async move |this, cx| {
            let got = task.await;
            let _ = this.update(cx, |v, cx| {
                if v.asked != asked {
                    return;
                }
                v.reading = false;
                match got {
                    Ok(part) => v.took(part, cx),
                    Err(e) => v.error = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn took(&mut self, part: LogPart, cx: &mut Context<Self>) {
        self.error = None;
        match self.next {
            None => self.earlier = part.start > 0,
            // The log started over, or its oldest went, since the last
            // read: what is shown does not run on into this.
            Some(n) if part.start != n => {
                self.earlier = part.start > 0;
                self.pane.update(cx, |p, cx| p.clear(cx));
            }
            Some(_) => {}
        }
        self.next = Some(part.next);
        let empty = if part.missing {
            "There is no such log here yet."
        } else {
            "Nothing logged yet."
        };
        self.pane.update(cx, |p, cx| {
            p.set_empty(empty, cx);
            if !part.text.is_empty() {
                p.push(&part.text, cx);
            }
        });
    }
}

impl Render for LogsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let mut bar = h_flex().h(px(26.)).flex_none().px(px(8.)).gap(px(4.));
        for (name, label, about) in LOGS {
            let at = format!("log-{name}");
            bar = bar.child(
                chip(
                    SharedString::from(at.clone()),
                    label.into(),
                    self.chosen == name,
                    &p,
                )
                .debug_selector(move || at.clone())
                .tooltip(move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(about).build(window, cx)
                })
                .on_click(cx.listener(move |v, _, _, cx| v.choose(name, cx))),
            );
        }
        if self.earlier {
            bar = bar.child(
                div()
                    .ml(px(8.))
                    .text_size(px(11.))
                    .text_color(p.muted)
                    .child("Its oldest lines are gone."),
            );
        }
        if let Some(e) = &self.error {
            bar = bar.child(
                div()
                    .ml(px(8.))
                    .min_w_0()
                    .truncate()
                    .text_size(px(11.))
                    .text_color(p.bad)
                    .child(e.clone()),
            );
        }
        v_flex()
            .size_full()
            .min_h_0()
            .text_size(px(12.))
            .child(bar)
            .child(div().flex_1().min_h_0().child(self.pane.clone()))
    }
}
