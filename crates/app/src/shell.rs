//! The window: tabs in the title bar (Home, then each environment opened),
//! the sidebar of every place's environments, and the tab's page. Drawn to
//! the design's Home mock.

use std::collections::HashSet;
use std::time::Duration;

use gpui::Focusable;

use gpui::{
    AnyElement, ClickEvent, Context, DragMoveEvent, Entity, FontWeight, IntoElement, ParentElement,
    Pixels, Render, SharedString, Styled, Window, div, prelude::*, px, relative, svg,
};
use gpui_component::input::{Input, InputState};
use gpui_component::{TitleBar, h_flex, v_flex};

use crate::assets;
use crate::doubleshift::DoubleShift;
use crate::envtab::{EnvTab, Tool};
use crate::layout::{self, Edge, Layout, Sizes};
use crate::model::{self, Action, Card, LineKind};
use crate::notices::Notices;
use crate::palette::{self, Palette};
use crate::places::Places;
use crate::search::{Chose, SearchView, Target};
use crate::settings::Settings;
use crate::ui::{button, chip, dot, mark, primary, spark};

/// How often the window is drawn again with nothing changed, so ages
/// ("3m") keep up.
const TICK: Duration = Duration::from_secs(30);

/// The narrowest a card on Home may be.
const CARD_MIN: f32 = 230.;

/// An environment opened in a tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    pub place: String,
    pub env: String,
}

/// How Home lays its cards out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeMode {
    /// One grid, what needs the user first.
    Attention,
    /// A grid for each place.
    Place,
}

pub struct Shell {
    places: Entity<Places>,
    tabs: Vec<Opened>,
    /// Each tab's view, in the order of tabs.
    views: Vec<Entity<EnvTab>>,
    /// The tab shown: None is Home.
    active: Option<usize>,
    home_mode: HomeMode,
    /// The places whose environments the sidebar hides.
    collapsed: HashSet<String>,
    /// The form adding a server, while it is open.
    adding: Option<AddServer>,
    /// The sessions said to be seen, so each is said once.
    told_seen: Vec<(String, String, chrono::DateTime<chrono::Utc>)>,
    /// Search Everywhere, while it is open.
    search: Option<(Entity<SearchView>, gpui::Subscription)>,
    /// Shift pressed twice, which also opens it.
    double_shift: DoubleShift,
    /// The window's focus when nothing in it has its own: the shell's keys
    /// (⌘K, Shift twice) reach it from wherever focus is, and gpui sends
    /// them nowhere else when nothing is focused.
    focus: gpui::FocusHandle,
    /// What has come to need the user, in every place.
    notices: Notices,
    /// Whether the bell's list is open.
    notices_open: bool,
}

gpui::actions!(shell, [SearchEverywhere, GoToSymbol]);

/// Binds the shell's keys: ⌘K (Ctrl+K elsewhere) for Search Everywhere,
/// and IntelliJ's ⌥⌘O (Ctrl+Alt+Shift+N elsewhere) for Go to Symbol.
pub fn bind_keys(cx: &mut gpui::App) {
    let (key, symbol) = if cfg!(target_os = "macos") {
        ("cmd-k", "alt-cmd-o")
    } else {
        ("ctrl-k", "ctrl-alt-shift-n")
    };
    cx.bind_keys([
        gpui::KeyBinding::new(key, SearchEverywhere, Some("Shell")),
        gpui::KeyBinding::new(symbol, GoToSymbol, Some("Shell")),
    ]);
}

struct AddServer {
    url: Entity<InputState>,
    token: Entity<InputState>,
    error: Option<String>,
}

impl Shell {
    pub fn new(places: Entity<Places>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // A context menu opening or closing puts the shield up or down.
        cx.observe_global::<crate::ui::OpenMenus>(|_, cx| cx.notify())
            .detach();
        cx.observe_in(&places, window, |this, _, window, cx| {
            this.tell_seen(window, cx);
            this.observe_notices(window, cx);
            cx.notify();
        })
        .detach();
        // A system notification clicked opens its environment.
        let (me, handle) = (cx.entity().downgrade(), window.window_handle());
        cx.on_system_notification_response(move |r, cx| {
            let Some((place, env)) = r.tag.split_once('\u{1f}') else {
                return;
            };
            let (place, env) = (place.to_string(), env.to_string());
            let me = me.clone();
            let _ = handle.update(cx, |_, window, cx| {
                window.activate_window();
                let _ = me.update(cx, |s, cx| s.open(&place, &env, window, cx));
            });
        });
        cx.observe_window_activation(window, |this, window, cx| this.tell_seen(window, cx))
            .detach();
        cx.observe_window_appearance(window, |_, window, cx| {
            palette::sync(Some(window), cx);
        })
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
            views: vec![],
            active: None,
            home_mode: HomeMode::Attention,
            collapsed: HashSet::new(),
            adding: None,
            told_seen: vec![],
            search: None,
            double_shift: DoubleShift::default(),
            focus: cx.focus_handle(),
            notices: Notices::default(),
            notices_open: false,
        };
        shell.observe_notices(window, cx);
        window.focus(&shell.focus, cx);
        if empty {
            shell.open_add(window, cx);
        }
        shell
    }

    /// The tabs open, and which is shown (None is Home).
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
                let places = self.places.clone();
                self.views
                    .push(cx.new(|cx| EnvTab::new(places, &o.place, &o.env, cx)));
                self.tabs.push(o);
                self.tabs.len() - 1
            }
        };
        self.show(i, window, cx);
    }

    /// Shows an open tab, its view taking the keyboard.
    fn show(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.adding = None;
        self.active = Some(i);
        if let Some(v) = self.views.get(i) {
            window.focus(&v.focus_handle(cx), cx);
        }
        self.tell_seen(window, cx);
        cx.notify();
    }

    /// The view of the tab shown, if an environment's is.
    pub fn active_view(&self) -> Option<Entity<EnvTab>> {
        self.active.and_then(|i| self.views.get(i)).cloned()
    }

    pub fn show_home(&mut self, cx: &mut Context<Self>) {
        self.adding = None;
        self.active = None;
        cx.notify();
    }

    pub fn set_home_mode(&mut self, mode: HomeMode, cx: &mut Context<Self>) {
        self.home_mode = mode;
        cx.notify();
    }

    fn close(&mut self, i: usize, cx: &mut Context<Self>) {
        self.tabs.remove(i);
        self.views.remove(i);
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

    /// Says the sessions in front of the user are seen: those of the
    /// environment shown, while the window is focused.
    fn tell_seen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !window.is_window_active() || self.adding.is_some() {
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

    /// Opens Search Everywhere over the window, or closes it.
    pub fn search_everywhere(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_search(None, window, cx);
    }

    /// Opens the search on the open environment's symbols alone, as
    /// IntelliJ's Go to Symbol, or closes it.
    pub fn go_to_symbol(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_search(Some("Symbols"), window, cx);
    }

    fn open_search(
        &mut self,
        only: Option<&'static str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.search.is_some() {
            self.close_search(window, cx);
            return;
        }
        let languages = self.active.and_then(|i| self.views[i].read(cx).languages());
        let active = self
            .active
            .map(|i| (self.tabs[i].place.clone(), self.tabs[i].env.clone()));
        let workspace = self
            .active
            .and_then(|i| self.views[i].read(cx).current_workspace());
        let places = self.places.clone();
        let view =
            cx.new(|cx| SearchView::new(places, active, workspace, languages, only, window, cx));
        let sub = cx.subscribe_in(&view, window, |s, _, e: &Chose, window, cx| {
            s.close_search(window, cx);
            if let Some(t) = &e.0 {
                s.go(t.clone(), window, cx);
            }
            cx.notify();
        });
        window.focus(&view.focus_handle(cx), cx);
        self.search = Some((view, sub));
        cx.notify();
    }

    /// Notes what has come to need the user since the places were last
    /// seen, and tells the system of it while the window is behind others.
    fn observe_notices(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let news = self
            .notices
            .observe(self.places.read(cx).list(), chrono::Utc::now());
        if window.is_window_active() {
            return;
        }
        for n in news {
            let tag = match &n.env {
                Some(env) => format!("{}\u{1f}{env}", n.place),
                None => n.place.clone(),
            };
            let body = if n.detail.is_empty() {
                n.what.clone()
            } else {
                format!("{} · {}", n.what, n.detail)
            };
            cx.show_system_notification(gpui::SystemNotification {
                tag: tag.into(),
                title: n.title.clone().into(),
                body: body.into(),
                actions: vec![],
            });
        }
    }

    pub fn notices(&self) -> &Notices {
        &self.notices
    }

    /// Opens the bell's list, which marks what is in it read, or closes it.
    pub fn toggle_notices(&mut self, cx: &mut Context<Self>) {
        self.notices_open = !self.notices_open;
        if self.notices_open {
            self.notices.read_all();
        }
        cx.notify();
    }

    /// Closes Search Everywhere, the focus it had going back to the shell.
    fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// The Search Everywhere view, while it is open.
    pub fn search(&self) -> Option<Entity<SearchView>> {
        self.search.as_ref().map(|(v, _)| v.clone())
    }

    /// Goes where a search result leads.
    fn go(&mut self, t: Target, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.active.map(|i| self.views[i].clone());
        match t {
            Target::Env { place, env } => self.open(&place, &env, window, cx),
            Target::File { root, path } => {
                if let Some(tab) = tab {
                    tab.update(cx, |t, cx| t.open_file(&root, &path, window, cx));
                }
            }
            Target::At {
                root,
                path,
                line,
                column,
            } => {
                if let Some(tab) = tab {
                    tab.update(cx, |t, cx| {
                        t.open_file_at(&root, &path, line, column, window, cx)
                    });
                }
            }
            Target::Tool(Tool::Find) => {
                if let Some(tab) = tab {
                    tab.update(cx, |t, cx| t.find_in_files(window, cx));
                }
            }
            Target::Tool(tool) => {
                if let Some(tab) = tab {
                    tab.update(cx, |t, cx| {
                        if !t.is_open(tool) {
                            t.toggle(tool, cx);
                        }
                    });
                }
            }
            Target::Act { place, env, action } => self
                .places
                .update(cx, |p, cx| p.act(&place, &env, action, cx)),
            Target::SuspendIdle => self.suspend_idle(cx),
            Target::AddPlace => self.open_add(window, cx),
            Target::DoubleShift => Settings::update(cx, |v| v.double_shift = !v.double_shift),
        }
    }

    fn suspend_idle(&mut self, cx: &mut Context<Self>) {
        let cards = model::home(self.places.read(cx).list(), chrono::Utc::now());
        for (place, env) in model::idle(&cards) {
            self.places
                .update(cx, |p, cx| p.act(&place, &env, Action::Suspend, cx));
        }
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let now = chrono::Utc::now();
        let places = self.places.read(cx);
        let groups = model::sidebar(places.list(), now);
        let cards = model::home(places.list(), now);
        let by_place = model::home_by_place(places.list(), now);
        let error = places.error.clone();
        let width = window.viewport_size().width;

        let page = match (&self.adding, self.active) {
            (Some(_), _) => self.add_page(&p, cx).into_any_element(),
            (None, None) => self
                .home(&cards, by_place, width, &p, cx)
                .into_any_element(),
            (None, Some(i)) => self.views[i].clone().into_any_element(),
        };

        let search = self.search.as_ref().map(|(v, _)| v.clone());
        v_flex()
            .size_full()
            .relative()
            .track_focus(&self.focus)
            .key_context("Shell")
            .on_action(
                cx.listener(|s, _: &SearchEverywhere, window, cx| s.search_everywhere(window, cx)),
            )
            .on_action(cx.listener(|s, _: &GoToSymbol, window, cx| s.go_to_symbol(window, cx)))
            .on_modifiers_changed(
                cx.listener(|s, e: &gpui::ModifiersChangedEvent, window, cx| {
                    let pressed = s
                        .double_shift
                        .modifiers(&e.modifiers, std::time::Instant::now());
                    if pressed && Settings::get(cx).double_shift {
                        s.search_everywhere(window, cx);
                    }
                }),
            )
            .capture_key_down(cx.listener(|s, _: &gpui::KeyDownEvent, _, _| s.double_shift.key()))
            .on_drag_move(cx.listener(|_, e: &DragMoveEvent<Edge>, _, cx| {
                if *e.drag(cx) == Edge::Sidebar {
                    let size = Sizes::dragged(Edge::Sidebar, e.event.position, e.bounds, 0.);
                    Layout::update(cx, |s| s.set(Edge::Sidebar, size));
                }
            }))
            .bg(p.bg)
            .text_color(p.text)
            .text_size(px(12.5))
            .line_height(relative(1.45))
            .child(self.title_bar(&cards, &p, cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .child(self.sidebar(groups, &p, cx))
                    .child(div().flex_1().min_w_0().h_full().child(page)),
            )
            .when_some(error, |this, e| {
                this.child(
                    h_flex()
                        .id("error")
                        .px_3()
                        .py_1()
                        .gap_2()
                        .bg(p.bad)
                        .text_color(p.on_accent)
                        .cursor_pointer()
                        .child(div().flex_1().child(e))
                        .child("Dismiss")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.places.update(cx, |p, _| p.error = None);
                            cx.notify();
                        })),
                )
            })
            .when(self.notices_open, |this| {
                this.child(
                    div()
                        .id("notices-scrim")
                        .absolute()
                        .inset_0()
                        .on_click(cx.listener(|s, _, _, cx| s.toggle_notices(cx))),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(38.))
                        .right(px(10.))
                        .child(self.notices_list(&p, cx)),
                )
            })
            .children(crate::ui::shield(cx))
            .when_some(search, |this, view| {
                this.child(
                    div()
                        .id("search-scrim")
                        .absolute()
                        .inset_0()
                        .bg(gpui::black().opacity(0.18))
                        .on_click(cx.listener(|s, _, window, cx| s.close_search(window, cx))),
                )
                .child(
                    h_flex()
                        .absolute()
                        .top(px(44.))
                        .left_0()
                        .right_0()
                        .justify_center()
                        .child(view),
                )
            })
    }
}

impl Shell {
    fn title_bar(&self, cards: &[Card], p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let tab = |id: SharedString, on: bool| {
            let hover = p.surface_2;
            h_flex()
                .id(id)
                .h(px(28.))
                .px(px(12.))
                .gap(px(6.))
                .rounded_t(px(6.))
                .whitespace_nowrap()
                .cursor_pointer()
                .when(on, |d| {
                    d.bg(p.surface)
                        .text_color(p.text)
                        .border_t_1()
                        .border_l_1()
                        .border_r_1()
                        .border_color(p.border)
                        .mb(px(-1.))
                })
                .when(!on, |d| d.text_color(p.muted).hover(move |s| s.bg(hover)))
        };
        let mut tabs = h_flex().h_full().items_end().gap(px(2.)).min_w_0().child(
            tab(
                "tab-home".into(),
                self.active.is_none() && self.adding.is_none(),
            )
            .child("Home")
            .on_click(cx.listener(|this, _, _, cx| this.show_home(cx))),
        );
        for (i, o) in self.tabs.iter().enumerate() {
            let card = cards.iter().find(|c| c.place == o.place && c.env == o.env);
            let title = card.map_or_else(|| "Gone".to_string(), |c| c.title.clone());
            let on = self.active == Some(i) && self.adding.is_none();
            tabs = tabs.child(
                tab(format!("tab-{i}").into(), on)
                    .when_some(card, |d, c| d.child(mark(c.mark, p)))
                    .child(div().max_w(px(170.)).truncate().child(title))
                    .when_some(card, |d, c| {
                        d.child(
                            div()
                                .text_size(px(10.5))
                                .text_color(p.muted)
                                .child(c.place_name.clone()),
                        )
                    })
                    .child(
                        div()
                            .id(("close", i))
                            .ml_1()
                            .text_color(p.muted)
                            .child("×")
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                this.close(i, cx);
                            })),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| this.show(i, window, cx))),
            );
        }
        let waiting = self.notices.unread();
        TitleBar::new().bg(p.sidebar).border_color(p.border).child(
            h_flex()
                .size_full()
                .gap(px(8.))
                .pr(px(10.))
                .child(tabs)
                .child(div().flex_1())
                .child(
                    h_flex()
                        .id("search")
                        .gap(px(8.))
                        .min_w(px(220.))
                        .px(px(10.))
                        .py(px(3.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(p.border)
                        .bg(p.surface)
                        .text_color(p.muted)
                        .cursor_pointer()
                        .hover(|s| s.border_color(p.border_strong))
                        .child("⌘K")
                        .child("Go to environment, file or command")
                        .on_click(cx.listener(|s, _, window, cx| s.search_everywhere(window, cx))),
                )
                .child(
                    div()
                        .id("bell")
                        .relative()
                        .ml(px(6.))
                        .text_color(p.muted)
                        .cursor_pointer()
                        .child(
                            svg()
                                .path(assets::icon("bell"))
                                .size(px(17.))
                                .text_color(p.muted),
                        )
                        .when(waiting > 0, |d| {
                            d.child(
                                div()
                                    .absolute()
                                    .right(px(-7.))
                                    .top(px(-5.))
                                    .px(px(4.))
                                    .rounded(px(7.))
                                    .bg(p.wait)
                                    .text_color(gpui::white())
                                    .text_size(px(9.))
                                    .line_height(px(14.))
                                    .child(waiting.to_string()),
                            )
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_notices(cx))),
                ),
        )
    }

    /// The bell's list, as the design draws it.
    fn notices_list(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let now = chrono::Utc::now();
        let mut list = v_flex().px(px(4.)).pb(px(6.));
        if self.notices.list().is_empty() {
            list = list.child(
                div()
                    .px(px(10.))
                    .py(px(10.))
                    .text_color(p.muted)
                    .child("Nothing needs you. Agents asking or finishing, and environments failing, show here."),
            );
        }
        for (i, n) in self.notices.list().iter().enumerate() {
            let target = n.env.clone().map(|e| (n.place.clone(), e));
            list = list.child(
                h_flex()
                    .id(("notice", i))
                    .items_start()
                    .gap(px(8.))
                    .px(px(10.))
                    .py(px(7.))
                    .rounded(px(6.))
                    .when(target.is_some(), |d| {
                        d.cursor_pointer().hover(|s| s.bg(p.surface_2))
                    })
                    .child(div().pt(px(5.)).child(dot(n.tone, p)))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                h_flex()
                                    .gap(px(4.))
                                    .min_w_0()
                                    .child(
                                        div()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .truncate()
                                            .child(n.title.clone()),
                                    )
                                    .child(div().flex_none().child(format!("· {}", n.what))),
                            )
                            .when(!n.detail.is_empty(), |d| {
                                d.child(
                                    div()
                                        .text_size(px(11.5))
                                        .text_color(p.muted)
                                        .truncate()
                                        .child(n.detail.clone()),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(11.))
                            .text_color(p.muted)
                            .child(model::age(n.at, now)),
                    )
                    .on_click(cx.listener(move |s, _, window, cx| {
                        if let Some((place, env)) = &target {
                            s.notices_open = false;
                            s.open(place, env, window, cx);
                        }
                    })),
            );
        }
        v_flex()
            .w(px(380.))
            .rounded(px(10.))
            .border_1()
            .border_color(p.border_strong)
            .bg(p.surface)
            .shadow_lg()
            .overflow_hidden()
            .text_size(px(12.5))
            // Clicks in the list stay in it, not reaching the shade behind.
            .occlude()
            .child(
                h_flex()
                    .px(px(14.))
                    .pt(px(10.))
                    .pb(px(8.))
                    .border_b_1()
                    .border_color(p.border)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.muted)
                    .child("Notifications")
                    .child(div().flex_1())
                    .child(div().font_weight(FontWeight::NORMAL).child("every place")),
            )
            .child(crate::project::scroll("notices", list).max_h(px(420.)))
    }

    fn sidebar(
        &self,
        groups: Vec<model::Group>,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.active.and_then(|i| self.tabs.get(i)).cloned();
        let mut list = v_flex().gap(px(6.));
        for g in groups {
            let (ctone, state, warn) = model::connection(&g.connection);
            let open = !self.collapsed.contains(&g.place);
            let id = g.place.clone();
            let mut group = v_flex().gap(px(1.)).child(
                h_flex()
                    .id(SharedString::from(format!("place-{}", g.place)))
                    .gap(px(6.))
                    .px(px(6.))
                    .pt(px(6.))
                    .pb(px(3.))
                    .text_size(px(11.5))
                    .whitespace_nowrap()
                    .cursor_pointer()
                    .child(
                        svg()
                            .path(assets::icon(if open {
                                "chevron-down"
                            } else {
                                "chevron-right"
                            }))
                            .size(px(10.))
                            .flex_none()
                            .text_color(p.muted),
                    )
                    .child(dot(ctone, p))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(g.name.clone()),
                    )
                    .child(div().text_size(px(10.)).text_color(p.muted).child(g.kind))
                    .when_some(state, |d, s| {
                        d.child(
                            div()
                                .ml_auto()
                                .text_size(px(10.5))
                                .text_color(if warn { p.busy } else { p.muted })
                                .child(s),
                        )
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.collapsed.remove(&id) {
                            this.collapsed.insert(id.clone());
                        }
                        cx.notify();
                    })),
            );
            if open {
                if g.rows.is_empty() {
                    group = group.child(
                        div()
                            .pl(px(18.))
                            .py(px(3.))
                            .text_size(px(11.))
                            .text_color(p.muted)
                            .child("No environments"),
                    );
                }
                for c in g.rows {
                    let on = active
                        .as_ref()
                        .is_some_and(|o| o.place == c.place && o.env == c.env);
                    group = group.child(self.row(c, on, p, cx));
                }
            }
            list = list.child(group);
        }
        let new_url = self
            .places
            .read(cx)
            .list()
            .iter()
            .find(|(_, s)| s.connection == crate::place::Connection::Online)
            .map(|(c, _)| c.url.clone());
        v_flex()
            .relative()
            .w(Layout::sizes(cx).get(Edge::Sidebar))
            .child(layout::handle("sidebar-edge", Edge::Sidebar))
            .flex_none()
            .h_full()
            .bg(p.sidebar)
            .px(px(8.))
            .py(px(10.))
            .gap(px(2.))
            .child(h_flex().px(px(4.)).pt(px(2.)).pb(px(6.)).child(
                primary("new-env", "+ New environment", p).on_click(cx.listener(
                    move |_, _, _, cx| {
                        // Until the app has its own sheet, the server's
                        // web UI makes environments.
                        if let Some(url) = &new_url {
                            cx.open_url(url);
                        }
                    },
                )),
            ))
            .child(
                crate::project::scroll("sidebar-list", list)
                    .flex_1()
                    .min_h_0(),
            )
            .child(
                div()
                    .id("add-place")
                    .px(px(8.))
                    .py(px(6.))
                    .text_size(px(11.5))
                    .text_color(p.muted)
                    .cursor_pointer()
                    .hover(|s| s.text_color(p.text))
                    .child("+ Add a place")
                    .on_click(cx.listener(|this, _, window, cx| this.open_add(window, cx))),
            )
    }

    fn row(&self, c: Card, on: bool, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let (place, env) = (c.place.clone(), c.env.clone());
        let need = c.rank.needs_you() && !c.stale;
        let hover = p.sidebar_active;
        let sub = if c.title != c.name {
            format!("{} · {}", c.name, c.summary)
        } else {
            c.summary.clone()
        };
        h_flex()
            .id(SharedString::from(format!("row-{}-{}", c.place, c.env)))
            .items_start()
            .gap(px(8.))
            .pl(px(18.))
            .pr(px(8.))
            .py(px(5.))
            .rounded(px(6.))
            .cursor_pointer()
            .when(need, |d| d.bg(p.warn_bg))
            .when(on, |d| d.bg(p.sidebar_active))
            .when(!on, move |d| d.hover(move |s| s.bg(hover.opacity(0.6))))
            .when(c.stale, |d| d.opacity(0.55))
            .child(div().pt(px(4.)).child(mark(c.mark, p)))
            .child(
                v_flex()
                    .min_w_0()
                    .flex_1()
                    .child(
                        h_flex()
                            .gap_1()
                            .min_w_0()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(c.title.clone()),
                            )
                            .when(c.agent_task, |d| d.child(spark(p))),
                    )
                    .child(
                        div()
                            .mt(px(-3.))
                            .truncate()
                            .text_size(px(11.))
                            .text_color(p.muted)
                            .child(sub),
                    ),
            )
            .on_click(cx.listener(move |this, _, window, cx| this.open(&place, &env, window, cx)))
    }

    fn home(
        &self,
        cards: &[Card],
        by_place: Vec<model::Group>,
        width: Pixels,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let room = f32::from(width) - Layout::sizes(cx).sidebar - 32.;
        let cols = ((room + 10.) / (CARD_MIN + 10.)).floor().clamp(1., 4.) as u16;
        let idle = model::idle(cards).len();
        let seg = |id: &'static str, mode: HomeMode, first: bool| {
            let on = self.home_mode == mode;
            div()
                .id(id)
                .px(px(10.))
                .py(px(2.))
                .cursor_pointer()
                .when(!first, |d| d.border_l_1().border_color(p.border))
                .when(on, |d| {
                    d.bg(p.accent_soft)
                        .text_color(p.accent)
                        .font_weight(FontWeight::SEMIBOLD)
                })
                .when(!on, |d| d.text_color(p.muted))
                .on_click(cx.listener(move |this, _, _, cx| this.set_home_mode(mode, cx)))
        };
        let head = h_flex()
            .gap(px(10.))
            .mb(px(12.))
            .child(
                div()
                    .text_size(px(16.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("All environments"),
            )
            .child(
                h_flex()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(p.border)
                    .overflow_hidden()
                    .child(seg("by-attention", HomeMode::Attention, true).child("By attention"))
                    .child(seg("by-place", HomeMode::Place, false).child("By place")),
            )
            .child(div().flex_1())
            .when(idle > 0, |d| {
                d.child(
                    button("suspend-idle", format!("Suspend idle ({idle})"), p)
                        .on_click(cx.listener(|this, _, _, cx| this.suspend_idle(cx))),
                )
            });
        let grid =
            |cards: Vec<AnyElement>| div().grid().grid_cols(cols).gap(px(10.)).children(cards);
        let mut body = v_flex().gap(px(14.));
        match self.home_mode {
            HomeMode::Attention => {
                body = body.child(grid(
                    cards
                        .iter()
                        .map(|c| self.card(c.clone(), p, cx).into_any_element())
                        .collect(),
                ));
            }
            HomeMode::Place => {
                for g in by_place {
                    let (ctone, state, warn) = model::connection(&g.connection);
                    body = body.child(
                        v_flex()
                            .gap(px(8.))
                            .child(
                                h_flex()
                                    .gap(px(7.))
                                    .child(dot(ctone, p))
                                    .child(
                                        div()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(g.name.clone()),
                                    )
                                    .child(
                                        div().text_size(px(11.)).text_color(p.muted).child(g.kind),
                                    )
                                    .when_some(state, |d, s| {
                                        d.child(
                                            div()
                                                .text_size(px(11.))
                                                .text_color(if warn { p.busy } else { p.muted })
                                                .child(s),
                                        )
                                    }),
                            )
                            .child(grid(
                                g.rows
                                    .into_iter()
                                    .map(|c| self.card(c, p, cx).into_any_element())
                                    .collect(),
                            )),
                    );
                }
            }
        }
        if cards.is_empty() {
            body = body.child(
                div()
                    .text_color(p.muted)
                    .child("No environments yet. Make one with + New environment."),
            );
        }
        v_flex()
            .id("home")
            .size_full()
            .overflow_y_scroll()
            .px(px(16.))
            .py(px(14.))
            .child(head)
            .child(body)
    }

    fn card(&self, c: Card, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let attn = c.rank == model::Rank::Asking && !c.stale;
        let fail = c.rank == model::Rank::Failed && !c.stale;
        let mut body = v_flex()
            .gap(px(2.))
            .px(px(10.))
            .pt(px(2.))
            .pb(px(8.))
            .min_h(px(52.))
            .text_size(px(11.5));
        for l in &c.lines {
            body = body.child(match l.kind {
                LineKind::Question => div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(l.text.clone()),
                LineKind::Plain => div().truncate().child(l.text.clone()),
                LineKind::Muted => div()
                    .truncate()
                    .text_size(px(11.))
                    .text_color(p.muted)
                    .child(l.text.clone()),
            });
        }
        let (place, env) = (c.place.clone(), c.env.clone());
        let card_for_action = c.clone();
        v_flex()
            .id(SharedString::from(format!("card-{}-{}", c.place, c.env)))
            .min_w_0()
            .rounded(px(8.))
            .overflow_hidden()
            .bg(p.surface)
            .border_1()
            .border_color(p.border)
            .cursor_pointer()
            .when(attn, |d| d.border_2().border_color(p.wait))
            .when(fail, |d| d.border_color(p.bad))
            .when(c.stale, |d| d.opacity(0.6))
            .child(
                h_flex()
                    .gap(px(7.))
                    .px(px(10.))
                    .pt(px(8.))
                    .pb(px(4.))
                    .child(mark(c.mark, p))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(c.title.clone()),
                    )
                    .when(c.agent_task, |d| d.child(spark(p))),
            )
            .child(
                h_flex()
                    .gap(px(10.))
                    .px(px(10.))
                    .pb(px(6.))
                    .text_size(px(11.))
                    .text_color(p.muted)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(c.subtitle.clone()),
                    )
                    .child(chip(c.place_name.clone(), p)),
            )
            .child(body)
            .child(
                h_flex()
                    .mt_auto()
                    .gap(px(6.))
                    .px(px(10.))
                    .pt(px(7.))
                    .pb(px(9.))
                    .text_size(px(11.5))
                    .child(mark(c.mark, p))
                    .child(div().flex_1().truncate().child(c.status.clone()))
                    .when_some(c.action, |d, a| {
                        d.child(
                            button(format!("act-{}-{}", c.place, c.env), a.label(), p)
                                .py(px(1.))
                                .px(px(8.))
                                .text_size(px(11.))
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.action(&card_for_action, a, window, cx)
                                })),
                        )
                    }),
            )
            .on_click(cx.listener(move |this, _, window, cx| this.open(&place, &env, window, cx)))
    }

    fn add_page(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(add) = &self.adding else {
            return v_flex();
        };
        let first = self.places.read(cx).list().is_empty();
        v_flex().size_full().items_center().justify_center().child(
            v_flex()
                .w(px(440.))
                .gap(px(12.))
                .p(px(18.))
                .rounded(px(10.))
                .bg(p.surface)
                .border_1()
                .border_color(p.border)
                .child(
                    div()
                        .text_size(px(16.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Add a Hangar server"),
                )
                .child(
                    div()
                        .text_color(p.muted)
                        .child("Its address, and an access token made on its Account page."),
                )
                .child(
                    v_flex()
                        .gap(px(4.))
                        .child(
                            div()
                                .text_size(px(11.5))
                                .text_color(p.muted)
                                .child("Address"),
                        )
                        .child(Input::new(&add.url)),
                )
                .child(
                    v_flex()
                        .gap(px(4.))
                        .child(
                            div()
                                .text_size(px(11.5))
                                .text_color(p.muted)
                                .child("Access token"),
                        )
                        .child(Input::new(&add.token)),
                )
                .when_some(add.error.clone(), |d, e| {
                    d.child(div().text_color(p.bad).child(e))
                })
                .child(
                    h_flex()
                        .gap(px(8.))
                        .justify_end()
                        .when(!first, |d| {
                            d.child(button("add-cancel", "Cancel", p).on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.adding = None;
                                    cx.notify();
                                },
                            )))
                        })
                        .child(
                            primary("add-submit", "Add", p)
                                .on_click(cx.listener(|this, _, _, cx| this.submit_add(cx))),
                        ),
                ),
        )
    }
}
