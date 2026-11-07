//! An environment's tab, laid out as IntelliJ is: the environment's
//! toolbar, tool windows docked from stripes on either side, the editors
//! in the middle, and a status bar. Drawn to the design's environment tab.

use std::collections::HashMap;

use gpui::{
    App, Context, DragMoveEvent, Entity, FocusHandle, Focusable, FontWeight, IntoElement,
    KeyBinding, ParentElement, Render, SharedString, Styled, Window, actions, div, prelude::*, px,
    svg,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{h_flex, v_flex};

use crate::api::{AgentSession, Environment};
use crate::assets;
use crate::layout::{self, Edge, Layout, Sizes};
use crate::model::{self, Card, Tone};
use crate::palette::{self, Palette};
use crate::place::PlaceConfig;
use crate::places::Places;
use crate::ui::{button, dot, primary, spark, tone};

/// A tool window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tool {
    Project,
    Commit,
    Structure,
    Terminal,
    Problems,
    Git,
    Docker,
    Find,
    Log,
    Agents,
    Ports,
}

/// Where a tool window docks: IntelliJ's stripes put the top group on
/// their side, the group below the line beneath it, and the foot along the
/// bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    Left,
    LeftBelow,
    Bottom,
    Right,
    RightBelow,
}

/// The left stripe: its top group, the group below the line, and the foot.
const LEFT: (&[Tool], &[Tool], &[Tool]) = (
    &[Tool::Project, Tool::Commit],
    &[Tool::Structure],
    &[
        Tool::Terminal,
        Tool::Problems,
        Tool::Git,
        Tool::Docker,
        Tool::Find,
        Tool::Log,
    ],
);
const RIGHT: (&[Tool], &[Tool], &[Tool]) = (&[Tool::Agents], &[Tool::Ports], &[]);

impl Tool {
    pub fn name(self) -> &'static str {
        match self {
            Tool::Project => "Project",
            Tool::Commit => "Commit",
            Tool::Structure => "Structure",
            Tool::Terminal => "Terminal",
            Tool::Problems => "Problems",
            Tool::Git => "Git",
            Tool::Docker => "Docker",
            Tool::Find => "Find",
            Tool::Log => "Log",
            Tool::Agents => "Agents",
            Tool::Ports => "Ports",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Tool::Find => "search",
            t => match t.name() {
                "Project" => "project",
                "Commit" => "commit",
                "Structure" => "structure",
                "Terminal" => "terminal",
                "Problems" => "problems",
                "Git" => "git",
                "Docker" => "docker",
                "Log" => "log",
                "Agents" => "agents",
                _ => "ports",
            },
        }
    }

    pub fn slot(self) -> Slot {
        match self {
            Tool::Project | Tool::Commit => Slot::Left,
            Tool::Structure => Slot::LeftBelow,
            Tool::Agents => Slot::Right,
            Tool::Ports => Slot::RightBelow,
            _ => Slot::Bottom,
        }
    }

    /// What it will hold, while the app cannot reach the environment's own
    /// services.
    fn waiting(self) -> &'static str {
        match self {
            Tool::Project => "The environment's projects and their files.",
            Tool::Commit => "What has changed, to commit and push.",
            Tool::Structure => "The open file's outline.",
            Tool::Terminal => "The environment's terminal sessions, Claude's among them.",
            Tool::Problems => "What the language servers report.",
            Tool::Git => "Branches, the commit graph and each commit's changes.",
            Tool::Docker => "The environment's containers, their logs and images.",
            Tool::Find => "Matches across the projects.",
            Tool::Log => "The environment's console and its services' logs.",
            Tool::Ports => "The ports the environment listens on, each with its address.",
            Tool::Agents => "",
        }
    }
}

actions!(
    env_tab,
    [
        ToggleProject,
        ToggleCommit,
        ToggleStructure,
        ToggleTerminal,
        ToggleProblems,
        ToggleGit,
        ToggleDocker,
        HideAllToolWindows,
        HideActiveToolWindow
    ]
);

/// Binds the tool windows' keys: IntelliJ's, Command on macOS and Alt
/// elsewhere.
pub fn bind_keys(cx: &mut App) {
    let ctx = Some("EnvTab");
    #[cfg(target_os = "macos")]
    let keys = [
        "cmd-1",
        "cmd-0",
        "cmd-7",
        "alt-f12",
        "cmd-6",
        "cmd-9",
        "cmd-8",
        "cmd-shift-f12",
    ];
    #[cfg(not(target_os = "macos"))]
    let keys = [
        "alt-1",
        "alt-0",
        "alt-7",
        "alt-f12",
        "alt-6",
        "alt-9",
        "alt-8",
        "ctrl-shift-f12",
    ];
    cx.bind_keys([
        KeyBinding::new(keys[0], ToggleProject, ctx),
        KeyBinding::new(keys[1], ToggleCommit, ctx),
        KeyBinding::new(keys[2], ToggleStructure, ctx),
        KeyBinding::new(keys[3], ToggleTerminal, ctx),
        KeyBinding::new(keys[4], ToggleProblems, ctx),
        KeyBinding::new(keys[5], ToggleGit, ctx),
        KeyBinding::new(keys[6], ToggleDocker, ctx),
        KeyBinding::new(keys[7], HideAllToolWindows, ctx),
        KeyBinding::new("shift-escape", HideActiveToolWindow, ctx),
    ]);
}

fn shortcut(t: Tool) -> Option<String> {
    let m = if cfg!(target_os = "macos") {
        "⌘"
    } else {
        "Alt+"
    };
    Some(match t {
        Tool::Project => format!("{m}1"),
        Tool::Commit => format!("{m}0"),
        Tool::Structure => format!("{m}7"),
        Tool::Terminal => if cfg!(target_os = "macos") {
            "⌥F12"
        } else {
            "Alt+F12"
        }
        .to_string(),
        Tool::Problems => format!("{m}6"),
        Tool::Git => format!("{m}9"),
        Tool::Docker => format!("{m}8"),
        _ => return None,
    })
}

pub struct EnvTab {
    places: Entity<Places>,
    place: String,
    env: String,
    focus: FocusHandle,
    /// The tool window open in each slot.
    open: HashMap<Slot, Tool>,
    /// The task, while it is being edited.
    task_edit: Option<Entity<InputState>>,
    /// The tool windows Hide All put away, to bring back.
    hidden: Option<HashMap<Slot, Tool>>,
    /// The tool window last opened, which Hide Active hides.
    last: Option<Tool>,
}

impl EnvTab {
    pub fn new(places: Entity<Places>, place: &str, env: &str, cx: &mut Context<Self>) -> Self {
        cx.observe(&places, |_, _, cx| cx.notify()).detach();
        let open = HashMap::from([
            (Slot::Left, Tool::Project),
            (Slot::Bottom, Tool::Terminal),
            (Slot::Right, Tool::Agents),
        ]);
        EnvTab {
            places,
            place: place.into(),
            env: env.into(),
            focus: cx.focus_handle(),
            open,
            task_edit: None,
            hidden: None,
            last: None,
        }
    }

    pub fn is_open(&self, tool: Tool) -> bool {
        self.open.get(&tool.slot()) == Some(&tool)
    }

    /// Opens a tool window in its slot, or closes it if it is open there.
    pub fn toggle(&mut self, tool: Tool, cx: &mut Context<Self>) {
        let slot = tool.slot();
        if self.open.get(&slot) == Some(&tool) {
            self.open.remove(&slot);
        } else {
            self.open.insert(slot, tool);
            self.last = Some(tool);
            self.hidden = None;
        }
        cx.notify();
    }

    /// Puts every tool window away, leaving the editors the room, or brings
    /// back what it put away.
    pub fn hide_all(&mut self, cx: &mut Context<Self>) {
        if self.open.is_empty() {
            if let Some(h) = self.hidden.take() {
                self.open = h;
            }
        } else {
            self.hidden = Some(std::mem::take(&mut self.open));
        }
        cx.notify();
    }

    /// Hides the tool window last opened, if it is still open.
    pub fn hide_active(&mut self, cx: &mut Context<Self>) {
        if let Some(t) = self.last.take()
            && self.is_open(t)
        {
            self.open.remove(&t.slot());
            cx.notify();
        }
    }

    fn edit_task(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = text.to_string();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("What is this environment for?")
                .default_value(text)
        });
        cx.subscribe_in(&input, window, |this, _, e: &InputEvent, _, cx| {
            if let InputEvent::PressEnter { .. } = e {
                this.save_task(cx);
            }
        })
        .detach();
        input.update(cx, |i, cx| i.focus(window, cx));
        self.task_edit = Some(input);
        cx.notify();
    }

    fn save_task(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.task_edit.take() else {
            return;
        };
        let text = input.read(cx).value().to_string();
        let (place, env) = (self.place.clone(), self.env.clone());
        self.places
            .update(cx, |p, cx| p.set_task(&place, &env, &text, cx));
        cx.notify();
    }
}

impl Focusable for EnvTab {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for EnvTab {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let places = self.places.read(cx);
        let Some((config, state)) = places.get(&self.place) else {
            return div().p_4().child("This place is gone.").into_any_element();
        };
        let Some(env) = state.environment(&self.env).cloned() else {
            return div()
                .p_4()
                .text_color(p.muted)
                .child("This environment has been deleted, or is out of reach.")
                .into_any_element();
        };
        let config = config.clone();
        let card = model::card(&config, &env, chrono::Utc::now());

        let left = self.open.get(&Slot::Left).copied();
        let left_below = self.open.get(&Slot::LeftBelow).copied();
        let right = self.open.get(&Slot::Right).copied();
        let right_below = self.open.get(&Slot::RightBelow).copied();
        let bottom = self.open.get(&Slot::Bottom).copied();
        let waiting = env.agents.iter().filter(|a| a.needs_you).count();
        let sizes = Layout::sizes(cx);

        v_flex()
            .size_full()
            .key_context("EnvTab")
            .on_action(cx.listener(|t, _: &HideAllToolWindows, _, cx| t.hide_all(cx)))
            .on_action(cx.listener(|t, _: &HideActiveToolWindow, _, cx| t.hide_active(cx)))
            .on_drag_move(cx.listener(|_, e: &DragMoveEvent<Edge>, _, cx| {
                let edge = *e.drag(cx);
                let inset = match edge {
                    Edge::Left | Edge::Right => 38.,
                    Edge::Bottom => 24.,
                    Edge::Sidebar => return,
                };
                let size = Sizes::dragged(edge, e.event.position, e.bounds, inset);
                Layout::update(cx, |s| s.set(edge, size));
            }))
            .track_focus(&self.focus)
            .on_action(cx.listener(|t, _: &ToggleProject, _, cx| t.toggle(Tool::Project, cx)))
            .on_action(cx.listener(|t, _: &ToggleCommit, _, cx| t.toggle(Tool::Commit, cx)))
            .on_action(cx.listener(|t, _: &ToggleStructure, _, cx| t.toggle(Tool::Structure, cx)))
            .on_action(cx.listener(|t, _: &ToggleTerminal, _, cx| t.toggle(Tool::Terminal, cx)))
            .on_action(cx.listener(|t, _: &ToggleProblems, _, cx| t.toggle(Tool::Problems, cx)))
            .on_action(cx.listener(|t, _: &ToggleGit, _, cx| t.toggle(Tool::Git, cx)))
            .on_action(cx.listener(|t, _: &ToggleDocker, _, cx| t.toggle(Tool::Docker, cx)))
            .child(self.toolbar(&config, &env, &card, &p, cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .child(self.stripe(LEFT, true, waiting, &p, cx))
                    .child(
                        // As IntelliJ: the side windows end above the bottom
                        // one, which takes the width between the stripes.
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                h_flex()
                                    .flex_1()
                                    .min_h_0()
                                    .items_stretch()
                                    .when(left.is_some() || left_below.is_some(), |d| {
                                        d.child(
                                            v_flex()
                                                .relative()
                                                .w(sizes.get(Edge::Left))
                                                .flex_none()
                                                .child(layout::handle("left-edge", Edge::Left))
                                                .bg(p.surface)
                                                .when_some(left, |d, t| {
                                                    d.child(
                                                        self.tool_window(t, &env, &card, &p, cx),
                                                    )
                                                })
                                                .when_some(left_below, |d, t| {
                                                    d.child(
                                                        div()
                                                            .when(left.is_some(), |d| {
                                                                d.border_t_1()
                                                                    .border_color(p.border)
                                                            })
                                                            .flex_1()
                                                            .child(self.tool_window(
                                                                t, &env, &card, &p, cx,
                                                            )),
                                                    )
                                                }),
                                        )
                                    })
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .bg(p.surface)
                                            .child(self.editors(&p)),
                                    )
                                    .when(right.is_some() || right_below.is_some(), |d| {
                                        d.child(
                                            v_flex()
                                                .relative()
                                                .w(sizes.get(Edge::Right))
                                                .flex_none()
                                                .child(layout::handle("right-edge", Edge::Right))
                                                .bg(p.surface)
                                                .when_some(right, |d, t| {
                                                    d.child(
                                                        self.tool_window(t, &env, &card, &p, cx),
                                                    )
                                                })
                                                .when_some(right_below, |d, t| {
                                                    d.child(
                                                        div()
                                                            .when(right.is_some(), |d| {
                                                                d.border_t_1()
                                                                    .border_color(p.border)
                                                            })
                                                            .child(self.tool_window(
                                                                t, &env, &card, &p, cx,
                                                            )),
                                                    )
                                                }),
                                        )
                                    }),
                            )
                            .when_some(bottom, |d, t| {
                                d.child(self.bottom(t, sizes.get(Edge::Bottom), &p))
                            }),
                    )
                    .child(self.stripe(RIGHT, false, waiting, &p, cx)),
            )
            .child(self.status_bar(&config, &env, &p))
            .into_any_element()
    }
}

impl EnvTab {
    fn toolbar(
        &self,
        config: &PlaceConfig,
        env: &Environment,
        card: &Card,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let pill = |p: &Palette| {
            h_flex()
                .gap(px(5.))
                .px(px(8.))
                .py(px(1.))
                .rounded(px(10.))
                .border_1()
                .border_color(p.border)
                .text_size(px(10.5))
                .text_color(p.muted)
        };
        let where_ = match env.phase.as_str() {
            "running" => format!("Running on {}", config.name),
            "suspended" => format!("Suspended on {}", config.name),
            "stopped" => format!("Stopped on {}", config.name),
            "failed" => format!("Failed on {}", config.name),
            phase => format!("{} on {}", capital(phase), config.name),
        };
        let phase_tone = match env.phase.as_str() {
            "running" => Tone::Good,
            "suspended" => Tone::Ring,
            "stopped" => Tone::Idle,
            "failed" => Tone::Bad,
            _ => Tone::Busy,
        };
        let (place, id) = (self.place.clone(), self.env.clone());
        let task_text = env
            .task
            .as_ref()
            .map(|t| t.text.clone())
            .unwrap_or_default();
        let lifecycle = match env.phase.as_str() {
            "running" => Some(("Suspend", model::Action::Suspend)),
            "suspended" => Some(("Resume", model::Action::Resume)),
            "stopped" | "failed" => Some(("Start", model::Action::Start)),
            _ => None,
        };
        let web = format!("{}/environments/{}", config.url, env.id);

        let title =
            match &self.task_edit {
                Some(input) => h_flex()
                    .gap(px(6.))
                    .w(px(420.))
                    .child(div().flex_1().child(Input::new(input)))
                    .child(
                        primary("task-save", "Save", p)
                            .on_click(cx.listener(|t, _, _, cx| t.save_task(cx))),
                    )
                    .child(button("task-cancel", "Cancel", p).on_click(cx.listener(
                        |t, _, _, cx| {
                            t.task_edit = None;
                            cx.notify();
                        },
                    )))
                    .when(env.task.is_some(), |d| {
                        let (place, id) = (place.clone(), id.clone());
                        d.child(button("task-done", "Mark done", p).on_click(cx.listener(
                            move |t, _, _, cx| {
                                t.task_edit = None;
                                t.places
                                    .update(cx, |pl, cx| pl.set_task(&place, &id, "", cx));
                            },
                        )))
                    }),
                None => h_flex()
                    .gap(px(6.))
                    .min_w_0()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(14.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(card.title.clone()),
                    )
                    .when(card.agent_task, |d| d.child(spark(p)))
                    .when(card.title != card.name, |d| {
                        d.child(
                            div()
                                .text_size(px(11.5))
                                .text_color(p.muted)
                                .child(card.name.clone()),
                        )
                    })
                    .child(
                        div()
                            .id("task-edit")
                            .text_color(p.muted)
                            .cursor_pointer()
                            .child("✎")
                            .on_click(cx.listener(move |t, _, window, cx| {
                                t.edit_task(&task_text, window, cx)
                            })),
                    ),
            };

        h_flex()
            .gap(px(10.))
            .px(px(12.))
            .py(px(8.))
            .bg(p.surface)
            .border_b_1()
            .border_color(p.border)
            .child(title)
            .child(pill(p).child(dot(phase_tone, p)).child(where_))
            .child(div().flex_1())
            .when_some(lifecycle, |d, (label, action)| {
                let (place, id) = (place.clone(), id.clone());
                d.child(
                    button("lifecycle", label, p).on_click(cx.listener(move |t, _, _, cx| {
                        t.places
                            .update(cx, |pl, cx| pl.act(&place, &id, action, cx));
                    })),
                )
            })
            .child(button("more", "Open in browser", p).on_click(move |_, _, cx| cx.open_url(&web)))
    }

    fn stripe(
        &self,
        groups: (&[Tool], &[Tool], &[Tool]),
        left: bool,
        waiting: usize,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (top, mid, foot) = groups;
        let icon = |t: Tool, cx: &mut Context<Self>| {
            let on = self.is_open(t);
            let badge = (t == Tool::Agents && waiting > 0).then(|| waiting.to_string());
            let hover = p.surface_2;
            div()
                .id(SharedString::from(format!("stripe-{}", t.name())))
                .relative()
                .size(px(28.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .cursor_pointer()
                .when(on, |d| d.bg(p.accent_soft))
                .when(!on, move |d| d.hover(move |s| s.bg(hover)))
                .child(
                    svg()
                        .path(assets::icon(t.icon()))
                        .size(px(16.))
                        .text_color(if on { p.accent } else { p.muted }),
                )
                .when_some(badge, |d, b| {
                    d.child(
                        div()
                            .absolute()
                            .right(px(-3.))
                            .top(px(-3.))
                            .px(px(4.))
                            .rounded(px(7.))
                            .bg(p.accent)
                            .text_color(p.on_accent)
                            .text_size(px(9.))
                            .line_height(px(14.))
                            .child(b),
                    )
                })
                .tooltip(move |window, cx| {
                    let label = match shortcut(t) {
                        Some(k) => format!("{} {k}", t.name()),
                        None => t.name().to_string(),
                    };
                    gpui_component::tooltip::Tooltip::new(label).build(window, cx)
                })
                .on_click(cx.listener(move |this, _, _, cx| this.toggle(t, cx)))
        };
        let mut s = v_flex()
            .w(px(38.))
            .flex_none()
            .items_center()
            .gap(px(3.))
            .py(px(6.))
            .bg(p.sidebar)
            .border_color(p.border)
            .when(left, |d| d.border_r_1())
            .when(!left, |d| d.border_l_1());
        for t in top {
            s = s.child(icon(*t, cx));
        }
        if !mid.is_empty() {
            s = s.child(div().w(px(20.)).h(px(1.)).my(px(4.)).bg(p.border_strong));
            for t in mid {
                s = s.child(icon(*t, cx));
            }
        }
        s = s.child(div().flex_1());
        for t in foot {
            s = s.child(icon(*t, cx));
        }
        s
    }

    fn tool_window(
        &self,
        t: Tool,
        env: &Environment,
        card: &Card,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let head = h_flex()
            .gap(px(8.))
            .mb(px(8.))
            .font_weight(FontWeight::SEMIBOLD)
            .child(t.name())
            .child(div().flex_1())
            .child(
                div()
                    .id(SharedString::from(format!("hide-{}", t.name())))
                    .text_color(p.muted)
                    .font_weight(FontWeight::NORMAL)
                    .cursor_pointer()
                    .child("–")
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle(t, cx))),
            );
        let body = match t {
            Tool::Agents => self.agents(env, card, p).into_any_element(),
            t => div()
                .text_size(px(11.5))
                .text_color(p.muted)
                .child(t.waiting())
                .into_any_element(),
        };
        v_flex()
            .p(px(8.))
            .min_h_0()
            .text_size(px(12.))
            .child(head)
            .child(body)
    }

    fn agents(&self, env: &Environment, card: &Card, p: &Palette) -> impl IntoElement {
        let now = chrono::Utc::now();
        let mut list = v_flex().gap(px(4.));
        let mut sessions: Vec<&AgentSession> =
            env.agents.iter().filter(|a| a.state != "ended").collect();
        sessions.sort_by_key(|a| !a.needs_you);
        if sessions.is_empty() {
            return list.child(
                div()
                    .text_size(px(11.5))
                    .text_color(p.muted)
                    .child("No coding agent is running here. Start Claude Code in a terminal and it shows here."),
            );
        }
        for a in sessions {
            let asking = a.state == "asking";
            let lead = card.session.as_deref() == Some(a.id.as_str());
            let state_tone = match a.state.as_str() {
                "asking" => Tone::Wait,
                "finished" if a.needs_you => Tone::Good,
                "working" => Tone::Busy,
                _ => Tone::Idle,
            };
            list = list.child(
                v_flex()
                    .gap(px(4.))
                    .px(px(6.))
                    .py(px(7.))
                    .rounded(px(6.))
                    .text_size(px(11.5))
                    .when(lead && !asking, |d| d.bg(p.accent_soft))
                    .when(asking, |d| {
                        d.bg(p.warn_bg).border_1().border_color(p.warn_border)
                    })
                    .when(!lead && !asking, |d| {
                        d.border_b_1().border_color(p.border).rounded_none()
                    })
                    .child(
                        h_flex()
                            .gap(px(6.))
                            .child(dot(state_tone, p))
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(a.agent.clone()),
                            )
                            .child(
                                div()
                                    .ml_auto()
                                    .text_size(px(10.5))
                                    .text_color(p.muted)
                                    .child(format!("{} · {}", a.state, model::age(a.since, now))),
                            ),
                    )
                    .when_some(env.task.as_ref(), |d, t| {
                        d.child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(t.text.clone()),
                        )
                    })
                    .when(asking, |d| {
                        d.when_some(a.step.clone(), |d, s| {
                            d.child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(format!("{s}?")),
                            )
                        })
                        .when_some(a.message.clone(), |d, m| {
                            d.child(div().text_color(p.muted).child(m))
                        })
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(p.muted)
                                .child("Answer it in its terminal session."),
                        )
                    })
                    .when(!asking, |d| {
                        d.when_some(a.step.clone(), |d, s| {
                            d.child(div().text_color(p.muted).child(s))
                        })
                    }),
            );
        }
        list
    }

    fn editors(&self, p: &Palette) -> impl IntoElement {
        let mut keys = v_flex().gap(px(6.)).items_center();
        for t in [Tool::Project, Tool::Terminal, Tool::Commit, Tool::Git] {
            if let Some(k) = shortcut(t) {
                keys = keys.child(
                    h_flex()
                        .gap(px(10.))
                        .child(div().w(px(110.)).text_right().child(t.name()))
                        .child(div().w(px(70.)).text_color(p.accent).child(k)),
                );
            }
        }
        v_flex()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .gap(px(14.))
            .text_color(p.muted)
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("No file open"),
            )
            .child(keys)
    }

    fn bottom(&self, t: Tool, height: gpui::Pixels, p: &Palette) -> impl IntoElement {
        let dark = t == Tool::Terminal;
        v_flex()
            .relative()
            .h(height)
            .flex_none()
            .bg(if dark { p.term_bg } else { p.surface })
            .child(layout::handle("bottom-edge", Edge::Bottom))
            .child(
                h_flex()
                    .gap(px(6.))
                    .px(px(10.))
                    .py(px(5.))
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if dark { p.term_fg } else { p.text })
                    .when(!dark, |d| d.border_b_1().border_color(p.border))
                    .child(t.name()),
            )
            .child(
                div()
                    .px(px(10.))
                    .py(px(6.))
                    .text_size(px(11.))
                    .text_color(if dark { p.idle } else { p.muted })
                    .child(t.waiting()),
            )
    }

    fn status_bar(&self, config: &PlaceConfig, env: &Environment, p: &Palette) -> impl IntoElement {
        let size = match (env.cpus, env.memory_mib) {
            (Some(c), Some(m)) => format!("{c} vCPUs · {} GB", (m as f64 / 1024.).round()),
            _ => String::new(),
        };
        h_flex()
            .h(px(24.))
            .flex_none()
            .gap(px(14.))
            .px(px(12.))
            .bg(p.sidebar)
            .border_t_1()
            .border_color(p.border)
            .text_size(px(11.))
            .text_color(p.muted)
            .whitespace_nowrap()
            .child(format!("{} › {}", config.name, env.name))
            .child(div().flex_1())
            .when_some(env.worker.clone(), |d, w| d.child(format!("on {w}")))
            .child(size)
            .child(div().size(px(6.)).rounded_full().bg(tone(
                if env.phase == "running" {
                    Tone::Good
                } else {
                    Tone::Idle
                },
                p,
            )))
    }
}

fn capital(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}
