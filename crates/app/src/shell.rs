//! The window: tabs in the title bar (Home, then each environment opened),
//! the sidebar of every place's environments, and the tab's page.

use std::time::Duration;

use gpui::{
    App, ClickEvent, Context, Entity, Hsla, IntoElement, ParentElement, Render, SharedString,
    Styled, Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme, Sizable, TitleBar, h_flex, v_flex};

use crate::model::{self, Action, Card, LineKind, Tone};
use crate::place::Connection;
use crate::places::Places;

/// How often the window is drawn again with nothing changed, so ages
/// ("3m") keep up.
const TICK: Duration = Duration::from_secs(30);

/// An environment opened in a tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    pub place: String,
    pub env: String,
}

pub struct Shell {
    places: Entity<Places>,
    tabs: Vec<Opened>,
    /// The tab shown: None is Home.
    active: Option<usize>,
    /// The form adding a server, while it is open.
    adding: Option<AddServer>,
    /// The sessions said to be seen, so each is said once.
    told_seen: Vec<(String, String, chrono::DateTime<chrono::Utc>)>,
}

struct AddServer {
    url: Entity<InputState>,
    token: Entity<InputState>,
    error: Option<String>,
}

impl Shell {
    pub fn new(places: Entity<Places>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe_in(&places, window, |this, _, window, cx| {
            this.tell_seen(window, cx);
            cx.notify();
        })
        .detach();
        cx.observe_window_activation(window, |this, window, cx| this.tell_seen(window, cx))
            .detach();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
        })
        .detach();
        let empty = places.read(cx).list().is_empty();
        let mut shell = Shell {
            places,
            tabs: vec![],
            active: None,
            adding: None,
            told_seen: vec![],
        };
        if empty {
            shell.open_add(window, cx);
        }
        shell
    }

    /// The tabs open, for tests.
    pub fn tabs(&self) -> (&[Opened], Option<usize>) {
        (&self.tabs, self.active)
    }

    /// Opens an environment's tab, or shows it if it is open.
    pub fn open(&mut self, place: &str, env: &str, window: &mut Window, cx: &mut Context<Self>) {
        let o = Opened {
            place: place.into(),
            env: env.into(),
        };
        let i = match self.tabs.iter().position(|t| *t == o) {
            Some(i) => i,
            None => {
                self.tabs.push(o);
                self.tabs.len() - 1
            }
        };
        self.active = Some(i);
        self.tell_seen(window, cx);
        cx.notify();
    }

    pub fn show_home(&mut self, cx: &mut Context<Self>) {
        self.active = None;
        cx.notify();
    }

    fn close(&mut self, i: usize, cx: &mut Context<Self>) {
        self.tabs.remove(i);
        self.active = match self.active {
            Some(a) if a == i => None,
            Some(a) if a > i => Some(a - 1),
            a => a,
        };
        cx.notify();
    }

    fn open_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let url = cx.new(|cx| InputState::new(window, cx).placeholder("hangar.example.com"));
        let token = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("hgr_…")
                .masked(true)
        });
        self.adding = Some(AddServer {
            url,
            token,
            error: None,
        });
        cx.notify();
    }

    fn submit_add(&mut self, cx: &mut Context<Self>) {
        let Some(add) = &mut self.adding else { return };
        let url = add.url.read(cx).value().to_string();
        let token = add.token.read(cx).value().to_string();
        match self
            .places
            .update(cx, |p, cx| p.add_server(&url, &token, cx))
        {
            Ok(_) => self.adding = None,
            Err(e) => add.error = Some(e),
        }
        cx.notify();
    }

    /// Says the sessions in front of the user are seen: the lead session of
    /// the environment shown, while the window is focused.
    fn tell_seen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !window.is_window_active() {
            return;
        }
        let Some(o) = self.active.and_then(|i| self.tabs.get(i)).cloned() else {
            return;
        };
        let places = self.places.read(cx);
        let Some(e) = places
            .get(&o.place)
            .and_then(|(_, s)| s.environment(&o.env))
        else {
            return;
        };
        let due: Vec<_> = e
            .agents
            .iter()
            .filter(|a| a.needs_you)
            .map(|a| (o.env.clone(), a.id.clone(), a.since))
            .filter(|k| !self.told_seen.contains(k))
            .collect();
        for k in due {
            self.places
                .update(cx, |p, cx| p.seen(&o.place, &k.0, &k.1, cx));
            self.told_seen.push(k);
        }
    }

    fn action(&mut self, card: &Card, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            Action::Answer | Action::Review | Action::Details => {
                self.open(&card.place, &card.env, window, cx)
            }
            a => self
                .places
                .update(cx, |p, cx| p.act(&card.place, &card.env, a, cx)),
        }
    }
}

fn tone(t: Tone, cx: &App) -> Hsla {
    let th = cx.theme();
    match t {
        Tone::Ask => th.blue,
        Tone::Done => th.green,
        Tone::Failed => th.red,
        Tone::Busy => th.yellow,
        Tone::Quiet => th.muted_foreground,
    }
}

fn dot(color: Hsla) -> impl IntoElement {
    div().size(px(8.)).rounded_full().bg(color).flex_none()
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = chrono::Utc::now();
        let places = self.places.read(cx);
        let groups = model::sidebar(places.list(), now);
        let cards = model::home(places.list(), now);
        let error = places.error.clone();
        let theme = cx.theme().clone();

        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .text_sm()
            .child(self.title_bar(&cards, cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.sidebar(groups, cx))
                    .child(div().flex_1().min_w_0().h_full().overflow_hidden().child(
                        match (&self.adding, self.active) {
                            (Some(_), _) => self.add_page(window, cx).into_any_element(),
                            (None, None) => self.home(cards, cx).into_any_element(),
                            (None, Some(i)) => self.env_page(i, &cards, cx).into_any_element(),
                        },
                    )),
            )
            .when_some(error, |this, e| {
                this.child(
                    h_flex()
                        .id("error")
                        .px_3()
                        .py_1()
                        .gap_2()
                        .bg(theme.danger)
                        .text_color(theme.danger_foreground)
                        .child(e)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.places.update(cx, |p, _| p.error = None);
                            cx.notify();
                        })),
                )
            })
    }
}

impl Shell {
    fn title_bar(&self, cards: &[Card], cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let tab = |id: SharedString, active: bool| {
            h_flex()
                .id(id)
                .h(px(26.))
                .px_3()
                .gap_2()
                .rounded_md()
                .max_w(px(220.))
                .when(active, |d| {
                    d.bg(theme.tab_active)
                        .text_color(theme.tab_active_foreground)
                })
                .when(!active, |d| {
                    d.text_color(theme.muted_foreground)
                        .hover(|d| d.bg(theme.muted))
                })
        };
        let mut bar = h_flex().gap_1().child(
            tab("tab-home".into(), self.active.is_none())
                .child("Home")
                .on_click(cx.listener(|this, _, _, cx| this.show_home(cx))),
        );
        for (i, o) in self.tabs.iter().enumerate() {
            let card = cards.iter().find(|c| c.place == o.place && c.env == o.env);
            let title = card.map_or_else(|| "Gone".to_string(), |c| c.title.clone());
            let color = card.map_or(theme.muted_foreground, |c| tone(c.tone, cx));
            bar = bar.child(
                tab(format!("tab-{i}").into(), self.active == Some(i))
                    .child(dot(color))
                    .child(div().truncate().child(title))
                    .child(
                        div()
                            .id(("close", i))
                            .text_color(theme.muted_foreground)
                            .child("×")
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                this.close(i, cx);
                            })),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.active = Some(i);
                        this.tell_seen(window, cx);
                        cx.notify();
                    })),
            );
        }
        TitleBar::new().child(bar)
    }

    fn sidebar(&self, groups: Vec<model::Group>, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let active = self.active.and_then(|i| self.tabs.get(i)).cloned();
        let mut list = v_flex()
            .id("sidebar-list")
            .flex_1()
            .overflow_y_scroll()
            .gap_3()
            .py_2();
        for g in groups {
            let state = match &g.connection {
                Connection::Online => None,
                Connection::Connecting => Some("Connecting…".to_string()),
                Connection::Offline { .. } => Some("Offline".to_string()),
                Connection::SignIn => Some("Sign in".to_string()),
                Connection::Unsupported { .. } => Some("Needs an update".to_string()),
            };
            let mut group = v_flex().gap_px().child(
                h_flex()
                    .px_3()
                    .gap_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(g.name.clone()),
                    )
                    .when_some(state, |d, s| d.child(s)),
            );
            if g.rows.is_empty() {
                group = group.child(
                    div()
                        .px_3()
                        .py_1()
                        .text_color(theme.muted_foreground)
                        .child("No environments"),
                );
            }
            for c in g.rows {
                let selected = active
                    .as_ref()
                    .is_some_and(|o| o.place == c.place && o.env == c.env);
                let (place, env) = (c.place.clone(), c.env.clone());
                group = group.child(
                    h_flex()
                        .id(SharedString::from(format!("row-{}-{}", c.place, c.env)))
                        .mx_1()
                        .px_2()
                        .py_1()
                        .gap_2()
                        .rounded_md()
                        .items_start()
                        .when(selected, |d| d.bg(theme.sidebar_accent))
                        .hover(|d| d.bg(theme.sidebar_accent))
                        .child(div().pt(px(5.)).child(dot(tone(c.tone, cx))))
                        .child(
                            v_flex()
                                .min_w_0()
                                .flex_1()
                                .child(
                                    h_flex()
                                        .gap_1()
                                        .child(div().truncate().child(c.title.clone()))
                                        .when(c.agent_task, |d| {
                                            d.child(div().text_color(theme.blue).child("✦"))
                                        }),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(format!("{} · {}", c.subtitle, c.status)),
                                ),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.adding = None;
                            this.open(&place, &env, window, cx)
                        })),
                );
            }
            list = list.child(group);
        }
        v_flex()
            .w(px(264.))
            .h_full()
            .flex_none()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(list)
            .child(
                div()
                    .p_2()
                    .border_t_1()
                    .border_color(theme.sidebar_border)
                    .child(
                        Button::new("add-server")
                            .ghost()
                            .small()
                            .label("Add a server…")
                            .on_click(cx.listener(|this, _, window, cx| this.open_add(window, cx))),
                    ),
            )
    }

    fn home(&self, cards: Vec<Card>, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let mut grid = h_flex().flex_wrap().gap_3().items_stretch();
        for c in cards {
            grid = grid.child(self.card(c, cx));
        }
        v_flex()
            .id("home")
            .size_full()
            .overflow_y_scroll()
            .p_4()
            .gap_3()
            .child(
                div()
                    .text_base()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("All environments"),
            )
            .child(grid)
            .when(
                self.places
                    .read(cx)
                    .list()
                    .iter()
                    .all(|(_, s)| s.environments.is_empty()),
                |d| {
                    d.child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child("No environments yet."),
                    )
                },
            )
    }

    fn card(&self, c: Card, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let color = tone(c.tone, cx);
        let border = if c.rank.needs_you() {
            color
        } else {
            theme.border
        };
        let mut body = v_flex().gap_1().flex_1().pt_1();
        for l in &c.lines {
            body = body.child(match l.kind {
                LineKind::Question => div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(l.text.clone()),
                LineKind::Plain => div().truncate().child(l.text.clone()),
                LineKind::Muted => div()
                    .truncate()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(l.text.clone()),
            });
        }
        let (place, env) = (c.place.clone(), c.env.clone());
        let action = c.action;
        let card_for_action = c.clone();
        v_flex()
            .id(SharedString::from(format!("card-{}-{}", c.place, c.env)))
            .w(px(272.))
            .min_h(px(156.))
            .p_3()
            .gap_1()
            .rounded_lg()
            .border_1()
            .border_color(border)
            .bg(theme.background)
            .child(
                h_flex()
                    .gap_2()
                    .child(dot(color))
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(c.title.clone()),
                    )
                    .when(c.agent_task, |d| {
                        d.child(div().text_color(theme.blue).child("✦"))
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(div().flex_1().truncate().child(c.subtitle.clone()))
                    .child(
                        div()
                            .px_1()
                            .rounded_sm()
                            .border_1()
                            .border_color(theme.border)
                            .child(c.place_name.clone()),
                    ),
            )
            .child(body)
            .child(
                h_flex()
                    .gap_2()
                    .child(dot(color))
                    .child(div().flex_1().text_xs().child(c.status.clone()))
                    .when_some(action, |d, a| {
                        d.child(
                            Button::new(SharedString::from(format!("act-{}-{}", c.place, c.env)))
                                .small()
                                .outline()
                                .label(a.label())
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.action(&card_for_action, a, window, cx)
                                })),
                        )
                    }),
            )
            .on_click(cx.listener(move |this, _, window, cx| this.open(&place, &env, window, cx)))
    }

    fn env_page(&self, i: usize, cards: &[Card], cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let Some(o) = self.tabs.get(i) else {
            return v_flex();
        };
        let places = self.places.read(cx);
        let card = cards.iter().find(|c| c.place == o.place && c.env == o.env);
        let env = places
            .get(&o.place)
            .and_then(|(_, s)| s.environment(&o.env));
        let (Some(card), Some(env)) = (card, env) else {
            return v_flex()
                .p_4()
                .child("This environment has been deleted, or is out of reach.");
        };
        let mut agents = v_flex().gap_2();
        for a in &env.agents {
            agents = agents.child(
                v_flex()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(if a.needs_you {
                        theme.blue
                    } else {
                        theme.border
                    })
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(a.agent.clone()),
                            )
                            .child(
                                div()
                                    .text_color(theme.muted_foreground)
                                    .child(a.state.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(model::age(a.since, chrono::Utc::now())),
                            ),
                    )
                    .when_some(a.message.clone(), |d, m| d.child(m))
                    .when_some(a.step.clone(), |d, s| {
                        d.child(div().text_color(theme.muted_foreground).child(s))
                    }),
            );
        }
        v_flex()
            .p_4()
            .gap_3()
            .child(
                h_flex().gap_2().child(dot(tone(card.tone, cx))).child(
                    div()
                        .text_base()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(card.title.clone()),
                ),
            )
            .child(div().text_color(theme.muted_foreground).child(format!(
                "{} · {} · {}",
                env.name, card.place_name, card.status
            )))
            .child(
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Agents"),
            )
            .child(if env.agents.is_empty() {
                agents.child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child("No agent sessions."),
                )
            } else {
                agents
            })
    }

    fn add_page(&self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let Some(add) = &self.adding else {
            return v_flex();
        };
        let first = self.places.read(cx).list().is_empty();
        v_flex().size_full().items_center().justify_center().child(
            v_flex()
                .w(px(420.))
                .gap_3()
                .p_4()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .child(
                    div()
                        .text_base()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("Add a Hangar server"),
                )
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child("Its address, and an access token made on its Account page."),
                )
                .child(
                    v_flex()
                        .gap_1()
                        .child("Address")
                        .child(Input::new(&add.url)),
                )
                .child(
                    v_flex()
                        .gap_1()
                        .child("Access token")
                        .child(Input::new(&add.token)),
                )
                .when_some(add.error.clone(), |d, e| {
                    d.child(div().text_color(theme.red).child(e))
                })
                .child(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .when(!first, |d| {
                            d.child(Button::new("add-cancel").ghost().label("Cancel").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.adding = None;
                                    cx.notify();
                                }),
                            ))
                        })
                        .child(
                            Button::new("add-submit")
                                .primary()
                                .label("Add")
                                .on_click(cx.listener(|this, _, _, cx| this.submit_add(cx))),
                        ),
                ),
        )
    }
}
