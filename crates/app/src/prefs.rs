//! Settings (⌘,), as IntelliJ's: one window over the app, a searchable tree
//! on its left and the chosen page on its right, Cancel, Apply and OK
//! below. The app's own settings come first, then a node for each place:
//! the user's account there, and its pages in the browser. Nothing changes
//! until Apply or OK.

use std::collections::HashSet;

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement,
    ParentElement, Render, SharedString, Styled, Window, div, prelude::*, px, relative,
};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Sizable, h_flex, v_flex};

use crate::account::AccountView;
use crate::palette::{self, Palette};
use crate::place::Connection;
use crate::places::Places;
use crate::settings::{Event, FONT_SIZES, Settings, ThemeChoice, Values, Ways};
use crate::thishangar::ThisHangarView;
use crate::ui::{button, disabled, primary};

/// A page of the settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Page {
    Appearance,
    Keymap,
    Editor,
    Terminal,
    Notifications,
    Places,
    /// A Hangar the app made on a Proxmox: where it runs, its release.
    Hangar(String),
    /// The user's account at a place.
    Account(String),
    /// A place's own pages, in the browser.
    Browser(String),
}

impl Page {
    pub const APP: [Page; 6] = [
        Page::Appearance,
        Page::Keymap,
        Page::Editor,
        Page::Terminal,
        Page::Notifications,
        Page::Places,
    ];

    pub fn title(&self) -> &'static str {
        match self {
            Page::Appearance => "Appearance",
            Page::Keymap => "Keymap",
            Page::Editor => "Editor",
            Page::Terminal => "Terminal",
            Page::Notifications => "Notifications",
            Page::Places => "Places",
            Page::Hangar(_) => "This Hangar",
            Page::Account(_) => "Account",
            Page::Browser(_) => "In the Browser",
        }
    }

    /// What a search finds the page by, besides its title: the settings
    /// on it.
    fn words(&self) -> &'static [&'static str] {
        match self {
            Page::Appearance => &["theme", "light", "dark", "system", "colours", "colors"],
            Page::Keymap => &[
                "keys",
                "shortcuts",
                "bindings",
                "shift",
                "search everywhere",
            ],
            Page::Editor => &[
                "font",
                "size",
                "soft-wrap",
                "wrap",
                "select opened file",
                "hidden files",
                "ignored files",
                "project",
                "pointer",
                "typing",
            ],
            Page::Terminal => &[
                "font",
                "family",
                "size",
                "line height",
                "shell",
                "theme",
                "colours",
                "colors",
                "contrast",
                "faint",
                "bold",
                "cursor",
                "blink",
                "option",
                "alt",
                "meta",
                "copy",
                "select",
                "right-click",
                "paste",
                "scroll",
                "bell",
                "dock",
                "sound",
                "progress",
                "clipboard",
                "scrollback",
            ],
            Page::Notifications => &[
                "sound", "badge", "dock", "system", "agent", "asks", "finishes", "fails", "ready",
                "sign in", "image", "quiet",
            ],
            Page::Places => &[
                "server",
                "rename",
                "remove",
                "order",
                "sign in",
                "token",
                "add a place",
            ],
            Page::Hangar(_) => &[
                "proxmox",
                "vm",
                "upgrade",
                "release",
                "remove",
                "delete",
                "certificate",
            ],
            Page::Account(_) => &[
                "sign-in keys",
                "ssh",
                "keys",
                "access tokens",
                "token",
                "revoke",
                "user",
            ],
            Page::Browser(_) => &[
                "templates",
                "packs",
                "files",
                "teams",
                "images",
                "admin",
                "users",
                "workers",
                "audit",
                "web",
            ],
        }
    }

    /// Whether the page is what a search for q finds.
    fn matches(&self, q: &str) -> bool {
        let q = q.trim().to_lowercase();
        q.is_empty()
            || self.title().to_lowercase().contains(&q)
            || self.words().iter().any(|w| w.contains(&q))
    }
}

pub enum SettingsEvent {
    /// The window is done with: OK, Cancel or Escape.
    Close,
    /// The user asks to sign a place in again.
    SignIn(String),
    /// The user asks to add a place.
    AddPlace,
}

/// A place as the Places page has it: renamed, moved or removed only once
/// applied.
struct PlaceRow {
    id: String,
    url: String,
    name: Entity<InputState>,
    removed: bool,
}

pub struct SettingsView {
    places: Entity<Places>,
    draft: Values,
    rows: Vec<PlaceRow>,
    page: Page,
    search: Entity<InputState>,
    keys: Entity<InputState>,
    /// The tree's groups folded: "app", or a place's ID.
    folded: HashSet<String>,
    focus: FocusHandle,
    /// Each place's account page, made when first shown.
    accounts: Vec<(String, Entity<AccountView>)>,
    /// Each made Hangar's page, made when first shown.
    hangars: Vec<(String, Entity<ThisHangarView>)>,
    /// The monospaced font families installed, found when the Terminal page
    /// is first shown.
    fonts: Option<Vec<String>>,
    _subs: Vec<gpui::Subscription>,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

gpui::actions!(settings_window, [Cancel]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([gpui::KeyBinding::new(
        "escape",
        Cancel,
        Some("SettingsWindow"),
    )]);
}

impl SettingsView {
    pub fn new(
        places: Entity<Places>,
        page: Page,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search settings"));
        let keys =
            cx.new(|cx| InputState::new(window, cx).placeholder("Find an action or its keys"));
        let subs = vec![
            cx.subscribe(&search, |_, _, e: &InputEvent, cx| {
                if matches!(e, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.subscribe(&keys, |_, _, e: &InputEvent, cx| {
                if matches!(e, InputEvent::Change) {
                    cx.notify();
                }
            }),
        ];
        let mut view = SettingsView {
            places,
            draft: Settings::get(cx),
            rows: vec![],
            page,
            search,
            keys,
            folded: HashSet::new(),
            focus: cx.focus_handle(),
            accounts: vec![],
            hangars: vec![],
            fonts: None,
            _subs: subs,
        };
        view.rows = view.place_rows(window, cx);
        view
    }

    fn place_rows(&self, window: &mut Window, cx: &mut Context<Self>) -> Vec<PlaceRow> {
        let list: Vec<(String, String, String)> = self
            .places
            .read(cx)
            .list()
            .iter()
            .map(|(c, _)| (c.id.clone(), c.url.clone(), c.name.clone()))
            .collect();
        list.into_iter()
            .map(|(id, url, name)| {
                let input = cx.new(|cx| {
                    let mut s = InputState::new(window, cx);
                    s.set_value(name, window, cx);
                    s
                });
                PlaceRow {
                    id,
                    url,
                    name: input,
                    removed: false,
                }
            })
            .collect()
    }

    pub fn page(&self) -> &Page {
        &self.page
    }

    /// Changes the settings as they stand in the window, to be kept on
    /// Apply or OK.
    pub fn edit(&mut self, f: impl FnOnce(&mut Values), cx: &mut Context<Self>) {
        f(&mut self.draft);
        cx.notify();
    }

    /// Marks a place to be removed on Apply or OK.
    pub fn remove_place(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(r) = self.rows.iter_mut().find(|r| r.id == id) {
            r.removed = true;
        }
        cx.notify();
    }

    pub fn show(&mut self, page: Page, cx: &mut Context<Self>) {
        self.page = page;
        cx.notify();
    }

    /// The places' list as the Places page has it: each kept place's ID
    /// and name, in order.
    fn arranged(&self, cx: &App) -> Vec<(String, String)> {
        self.rows
            .iter()
            .filter(|r| !r.removed)
            .map(|r| (r.id.clone(), r.name.read(cx).value().trim().to_string()))
            .collect()
    }

    /// Whether anything is changed and not yet applied.
    pub fn changed(&self, cx: &App) -> bool {
        if self.draft != Settings::get(cx) {
            return true;
        }
        let now: Vec<(String, String)> = self
            .places
            .read(cx)
            .list()
            .iter()
            .map(|(c, _)| (c.id.clone(), c.name.clone()))
            .collect();
        let arranged = self.arranged(cx);
        arranged.len() != now.len()
            || arranged
                .iter()
                .zip(&now)
                .any(|(a, b)| a.0 != b.0 || (!a.1.is_empty() && a.1 != b.1))
    }

    pub fn apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self.draft.clone();
        Settings::update(cx, |v| *v = draft);
        let arranged = self.arranged(cx);
        self.places.update(cx, |p, cx| p.arrange(&arranged, cx));
        self.rows = self.place_rows(window, cx);
        if let Page::Account(id) | Page::Browser(id) | Page::Hangar(id) = &self.page
            && self.places.read(cx).get(id).is_none()
        {
            self.page = Page::Places;
        }
        cx.notify();
    }

    fn ok(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.apply(window, cx);
        cx.emit(SettingsEvent::Close);
    }

    fn hangar(&mut self, place: &str, cx: &mut Context<Self>) -> Entity<ThisHangarView> {
        if let Some((_, v)) = self.hangars.iter().find(|(p, _)| p == place) {
            return v.clone();
        }
        let places = self.places.clone();
        let view = cx.new(|cx| ThisHangarView::new(places, place, cx));
        self.hangars.push((place.to_string(), view.clone()));
        view
    }

    fn account(
        &mut self,
        place: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<AccountView> {
        if let Some((_, v)) = self.accounts.iter().find(|(p, _)| p == place) {
            return v.clone();
        }
        let client = self.places.read(cx).client(place);
        let view = cx.new(|cx| AccountView::new(client, window, cx));
        self.accounts.push((place.to_string(), view.clone()));
        view
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let changed = self.changed(cx);
        let size = window.viewport_size();
        let w = (size.width - px(40.)).min(px(960.));
        let h = (size.height - px(70.)).min(px(660.));
        let crumb = match &self.page {
            Page::Account(id) | Page::Browser(id) | Page::Hangar(id) => {
                let name = self
                    .places
                    .read(cx)
                    .get(id)
                    .map(|(c, _)| c.name.clone())
                    .unwrap_or_default();
                format!("{name} › {}", self.page.title())
            }
            page => format!("App › {}", page.title()),
        };
        let body = self.page_body(&p, window, cx);
        v_flex()
            .id("settings-window")
            .debug_selector(|| "settings-window".into())
            .track_focus(&self.focus)
            .key_context("SettingsWindow")
            .on_action(cx.listener(|_, _: &Cancel, _, cx| cx.emit(SettingsEvent::Close)))
            .w(w)
            .h(h)
            .bg(p.surface)
            .border_1()
            .border_color(p.border_strong)
            .rounded(px(10.))
            .shadow_lg()
            .overflow_hidden()
            .text_size(px(12.5))
            .line_height(relative(1.45))
            .child(
                div()
                    .px(px(14.))
                    .py(px(9.))
                    .border_b_1()
                    .border_color(p.border)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Settings"),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .child(self.tree(&p, cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .px(px(18.))
                                    .pt(px(12.))
                                    .pb(px(6.))
                                    .text_color(p.muted)
                                    .child(crumb),
                            )
                            .child(
                                div()
                                    .id("settings-page")
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .px(px(18.))
                                    .pb(px(16.))
                                    .child(body),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .px(px(14.))
                    .py(px(10.))
                    .gap(px(8.))
                    .border_t_1()
                    .border_color(p.border)
                    .child(div().flex_1())
                    .child(
                        button("settings-cancel", "Cancel", &p)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::Close))),
                    )
                    .child(if changed {
                        button("settings-apply", "Apply", &p)
                            .on_click(cx.listener(|this, _, window, cx| this.apply(window, cx)))
                    } else {
                        disabled("settings-apply", "Apply", &p)
                    })
                    .child(
                        primary("settings-ok", "OK", &p)
                            .on_click(cx.listener(|this, _, window, cx| this.ok(window, cx))),
                    ),
            )
    }
}

impl SettingsView {
    fn tree(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let q = self.search.read(cx).value().to_string();
        let searching = !q.trim().is_empty();
        let places: Vec<(String, String, bool, bool)> = self
            .places
            .read(cx)
            .list()
            .iter()
            .map(|(c, s)| {
                (
                    c.id.clone(),
                    c.name.clone(),
                    matches!(s.connection, Connection::SignIn),
                    c.proxmox.is_some(),
                )
            })
            .collect();
        let mut groups: Vec<(String, String, Vec<Page>, Option<&'static str>)> =
            vec![("app".into(), "App".into(), Page::APP.to_vec(), None)];
        for (id, name, signed_out, made) in places {
            let mut pages = vec![Page::Account(id.clone()), Page::Browser(id.clone())];
            if made {
                pages.insert(0, Page::Hangar(id.clone()));
            }
            groups.push((id, name, pages, signed_out.then_some("Sign in…")));
        }
        let mut list = v_flex().gap(px(1.));
        for (key, title, pages, note) in groups {
            let pages: Vec<Page> = pages.into_iter().filter(|pg| pg.matches(&q)).collect();
            if searching && pages.is_empty() {
                continue;
            }
            let folded = !searching && self.folded.contains(&key);
            let fold_key = key.clone();
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("settings-group-{key}")))
                    .gap(px(4.))
                    .px(px(6.))
                    .py(px(2.))
                    .mt(px(4.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .text_color(p.muted)
                    .font_weight(FontWeight::SEMIBOLD)
                    .hover(|s| s.bg(p.surface_2))
                    .child(if folded { "▸" } else { "▾" })
                    .child(div().flex_1().truncate().child(title))
                    .when_some(note, |d, n| {
                        d.child(
                            div()
                                .text_color(p.wait)
                                .font_weight(FontWeight::NORMAL)
                                .child(n),
                        )
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.folded.remove(&fold_key) {
                            this.folded.insert(fold_key.clone());
                        }
                        cx.notify();
                    })),
            );
            if folded {
                continue;
            }
            for page in pages {
                let on = page == self.page;
                let id = match &page {
                    Page::Account(pl) => format!("settings-node-account-{pl}"),
                    Page::Hangar(pl) => format!("settings-node-hangar-{pl}"),
                    Page::Browser(pl) => format!("settings-node-browser-{pl}"),
                    other => format!("settings-node-{}", other.title().to_lowercase()),
                };
                let go = page.clone();
                list = list.child(
                    div()
                        .id(SharedString::from(id))
                        .pl(px(22.))
                        .pr(px(6.))
                        .py(px(2.))
                        .rounded(px(4.))
                        .cursor_pointer()
                        .when(on, |d| d.bg(p.selection))
                        .when(!on, |d| d.hover(|s| s.bg(p.surface_2)))
                        .child(page.title())
                        .on_click(cx.listener(move |this, _, _, cx| this.show(go.clone(), cx))),
                );
            }
        }
        v_flex()
            .w(px(230.))
            .flex_none()
            .h_full()
            .border_r_1()
            .border_color(p.border)
            .bg(p.sidebar)
            .p(px(8.))
            .gap(px(6.))
            .child(Input::new(&self.search).small())
            .child(
                div()
                    .id("settings-tree")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(list),
            )
    }

    fn page_body(
        &mut self,
        p: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        match self.page.clone() {
            Page::Appearance => self.appearance(p, cx).into_any_element(),
            Page::Keymap => self.keymap(p, cx).into_any_element(),
            Page::Editor => self.editor(p, cx).into_any_element(),
            Page::Terminal => self.terminal(p, cx).into_any_element(),
            Page::Notifications => self.notifications(p, cx).into_any_element(),
            Page::Places => self.places_page(p, cx).into_any_element(),
            Page::Account(id) => self.account(&id, window, cx).into_any_element(),
            Page::Hangar(id) => self.hangar(&id, cx).into_any_element(),
            Page::Browser(id) => self.browser(&id, p, cx).into_any_element(),
        }
    }

    fn appearance(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let choice = self.draft.theme;
        let seg =
            |id: &'static str, label: &'static str, value: ThemeChoice, cx: &mut Context<Self>| {
                let on = choice == value;
                div()
                    .id(id)
                    .px(px(12.))
                    .py(px(3.))
                    .cursor_pointer()
                    .when(on, |d| d.bg(p.accent).text_color(p.on_accent))
                    .when(!on, |d| d.hover(|s| s.bg(p.surface_2)))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.draft.theme = value;
                        cx.notify();
                    }))
            };
        v_flex()
            .gap(px(10.))
            .child(section("Theme", p))
            .child(
                field("Colours", p).child(
                    h_flex()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(p.border_strong)
                        .overflow_hidden()
                        .child(seg(
                            "theme-system",
                            "As the System",
                            ThemeChoice::System,
                            cx,
                        ))
                        .child(seg("theme-light", "Light", ThemeChoice::Light, cx))
                        .child(seg("theme-dark", "Dark", ThemeChoice::Dark, cx)),
                ),
            )
            .child(note(
                "As the System follows macOS's appearance, changing with it.",
                p,
            ))
    }

    fn editor(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let d = &self.draft;
        v_flex()
            .gap(px(10.))
            .child(section("Font", p))
            .child(field("Size", p).child(self.stepper(
                "editor-font",
                d.editor_font(),
                crate::settings::EDITOR_FONT_SIZE,
                |v, s| v.editor_font_size = s,
                p,
                cx,
            )))
            .child(section("Editing", p))
            .child(self.check("soft-wrap", "Soft-wrap long lines", d.soft_wrap, |v, b| v.soft_wrap = b, cx))
            .child(self.check(
                "hide-pointer",
                "Hide the pointer while typing, until the mouse moves",
                d.hide_pointer,
                |v, b| v.hide_pointer = b,
                cx,
            ))
            .child(section("Project and Filesystem", p))
            .child(self.check(
                "select-opened",
                "Always select the opened file",
                d.select_opened,
                |v, b| v.select_opened = b,
                cx,
            ))
            .child(self.check(
                "show-hidden",
                "Show hidden files (names starting with a dot)",
                d.show_hidden,
                |v, b| v.show_hidden = b,
                cx,
            ))
            .child(self.check(
                "show-ignored",
                "Show files git ignores",
                d.show_ignored,
                |v, b| v.show_ignored = b,
                cx,
            ))
            .child(note(
                "Spelling, language servers and where tool windows dock are kept with each environment, in its Code settings, as the web's are.",
                p,
            ))
    }

    fn terminal(&mut self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::settings::{Access, CursorShape, RightClick, TerminalEmulator};
        let fonts = self
            .fonts
            .get_or_insert_with(|| monospaced_fonts(cx))
            .clone();
        let d = self.draft.clone();
        let t = &d.terminal;
        let family = if t.font_family.is_empty() {
            format!("{} (the default)", crate::termview::mono_family())
        } else {
            t.font_family.clone()
        };
        let mut families = vec![(
            format!("{} (the default)", crate::termview::mono_family()),
            String::new(),
        )];
        families.extend(fonts.into_iter().map(|f| (f.clone(), f)));
        let themes = |chosen: &str| {
            let mut list = vec![("The app's colours".to_string(), String::new())];
            list.extend(
                crate::termthemes::THEMES
                    .iter()
                    .map(|th| (th.name.to_string(), th.name.to_string())),
            );
            let label = if chosen.is_empty() {
                "The app's colours".to_string()
            } else {
                chosen.to_string()
            };
            (label, list)
        };
        let (light_label, light_list) = themes(&t.theme_light);
        let (dark_label, dark_list) = themes(&t.theme_dark);
        let access = [
            ("Allow", Access::Allow),
            ("Ask", Access::Ask),
            ("Deny", Access::Deny),
        ];
        v_flex()
            .gap(px(10.))
            .child(section("Emulator", p))
            .child(field("Emulator", p).child(self.segments(
                "emulator",
                &[
                    ("Ghostty", TerminalEmulator::Ghostty),
                    ("Alacritty", TerminalEmulator::Alacritty),
                ],
                d.terminal_emulator,
                |v, e| v.terminal_emulator = e,
                p,
                cx,
            )))
            .child(note(
                "Ghostty is libghostty-vt, Ghostty's terminal emulation; a session a Ghostty terminal starts is told it runs in Ghostty, so programs such as Claude Code use what Ghostty can do. Terminals opened after the choice use it.",
                p,
            ))
            .child(section("Font", p))
            .child(field("Family", p).child(self.choice(
                "terminal-family",
                family,
                families,
                t.font_family.clone(),
                |v, f| v.terminal.font_family = f,
                cx,
            )))
            .child(field("Size", p).child(self.stepper(
                "terminal-font",
                d.terminal_font(),
                crate::settings::TERMINAL_FONT_SIZE,
                |v, s| v.terminal_font_size = s,
                p,
                cx,
            )))
            .child(field("Line height", p).child(self.amount(
                Amount {
                    id: "terminal-line",
                    value: t.line_height(),
                    default: crate::settings::LINE_HEIGHT,
                    step: 0.05,
                    range: (1., 2.),
                    show: |v| format!("{v:.2}×"),
                },
                |v, h| v.terminal.line_height = h,
                p,
                cx,
            )))
            .child(note(
                "A terminal that changes size tells its session, so programs in it redraw to fit.",
                p,
            ))
            .child(section("Colours", p))
            .child(field("Light theme", p).child(self.choice(
                "terminal-light",
                light_label,
                light_list,
                t.theme_light.clone(),
                |v, n| v.terminal.theme_light = n,
                cx,
            )))
            .child(field("Dark theme", p).child(self.choice(
                "terminal-dark",
                dark_label,
                dark_list,
                t.theme_dark.clone(),
                |v, n| v.terminal.theme_dark = n,
                cx,
            )))
            .child(note(
                "Each with the app's appearance, light or dark: the terminal's background, text and sixteen named colours, as Ghostty's themes of those names have them.",
                p,
            ))
            .child(field("Contrast", p).child(self.amount(
                Amount {
                    id: "terminal-contrast",
                    value: t.minimum_contrast(),
                    default: 1.,
                    step: 0.5,
                    range: (1., 21.),
                    show: |v| {
                        if v <= 1. {
                            "Off".into()
                        } else {
                            format!("{v:.1} : 1")
                        }
                    },
                },
                |v, c| v.terminal.minimum_contrast = c,
                p,
                cx,
            )))
            .child(note(
                "Text with less contrast against its background than this is drawn white or black, whichever stands out more.",
                p,
            ))
            .child(field("Faint text", p).child(self.amount(
                Amount {
                    id: "terminal-faint",
                    value: t.faint_opacity(),
                    default: 0.5,
                    step: 0.05,
                    range: (0., 1.),
                    show: |v| format!("{:.0}%", v * 100.),
                },
                |v, f| v.terminal.faint_opacity = f,
                p,
                cx,
            )))
            .child(self.check(
                "terminal-bold-bright",
                "Draw bold text in the bright colours",
                t.bold_is_bright,
                |v, b| v.terminal.bold_is_bright = b,
                cx,
            ))
            .child(section("Cursor", p))
            .child(field("Shape", p).child(self.segments(
                "terminal-cursor",
                &[
                    ("Block", CursorShape::Block),
                    ("Bar", CursorShape::Bar),
                    ("Underline", CursorShape::Underline),
                ],
                t.cursor,
                |v, c| v.terminal.cursor = c,
                p,
                cx,
            )))
            .child(self.check(
                "terminal-blink",
                "Blink",
                t.cursor_blink,
                |v, b| v.terminal.cursor_blink = b,
                cx,
            ))
            .child(note(
                "Until a program asks for another, as editors and shells may.",
                p,
            ))
            .child(section("Keyboard and mouse", p))
            .when(cfg!(target_os = "macos"), |d| {
                d.child(self.check(
                    "terminal-option-alt",
                    "Use Option as Alt (Meta)",
                    t.option_as_alt,
                    |v, b| v.terminal.option_as_alt = b,
                    cx,
                ))
                .child(note(
                    "Option-B and Option-F then move by words in the shell, as Alt does elsewhere, rather than typing ∫ and ƒ.",
                    p,
                ))
            })
            .child(self.check(
                "terminal-copy-select",
                "Copy text as it is selected",
                t.copy_on_select,
                |v, b| v.terminal.copy_on_select = b,
                cx,
            ))
            .child(field("Right-click", p).child(self.segments(
                "terminal-right",
                &[
                    ("Menu", RightClick::Menu),
                    ("Paste", RightClick::Paste),
                    ("Copy", RightClick::Copy),
                    ("Copy or paste", RightClick::CopyOrPaste),
                    ("Nothing", RightClick::Nothing),
                ],
                t.right_click,
                |v, r| v.terminal.right_click = r,
                p,
                cx,
            )))
            .child(note(
                "Shift-right-click opens the menu whatever is chosen. A program that asks for the mouse is told of the click instead.",
                p,
            ))
            .child(field("Scroll speed", p).child(self.amount(
                Amount {
                    id: "terminal-scroll",
                    value: t.scroll_speed(),
                    default: 1.,
                    step: 0.25,
                    range: (0.25, 10.),
                    show: |v| format!("{v:.2}×"),
                },
                |v, s| v.terminal.scroll_speed = s,
                p,
                cx,
            )))
            .child(section("Bell", p))
            .child(self.check(
                "terminal-bell-mark",
                "Mark the terminal's tab",
                t.bell_mark,
                |v, b| v.terminal.bell_mark = b,
                cx,
            ))
            .child(self.check(
                "terminal-bell-bounce",
                "Bounce the Dock icon while the app is behind others",
                t.bell_bounce,
                |v, b| v.terminal.bell_bounce = b,
                cx,
            ))
            .child(self.check(
                "terminal-bell-sound",
                "Play the system's alert sound",
                t.bell_sound,
                |v, b| v.terminal.bell_sound = b,
                cx,
            ))
            .child(section("Programs", p))
            .child(self.check(
                "terminal-progress",
                "Show the progress programs report, along the terminal's top",
                t.progress,
                |v, b| v.terminal.progress = b,
                cx,
            ))
            .child(field("Copy", p).child(self.segments(
                "terminal-clip-write",
                &access,
                t.clipboard_write,
                |v, a| v.terminal.clipboard_write = a,
                p,
                cx,
            )))
            .child(field("Read", p).child(self.segments(
                "terminal-clip-read",
                &access,
                t.clipboard_read,
                |v, a| v.terminal.clipboard_read = a,
                p,
                cx,
            )))
            .child(note(
                "Whether a program may copy to the clipboard, and read what is on it (OSC 52), or asks you first. A read hands the program whatever was last copied, anywhere.",
                p,
            ))
            .child(field("Scrollback", p).child(self.amount(
                Amount {
                    id: "terminal-scrollback",
                    value: t.scrollback_mb.clamp(1, 1000) as f32,
                    default: 50.,
                    step: 10.,
                    range: (10., 1000.),
                    show: |v| format!("{v:.0} MB"),
                },
                |v, mb| v.terminal.scrollback_mb = mb as u32,
                p,
                cx,
            )))
            .child(note(
                "Kept for each terminal, by Ghostty's emulation; alacritty's keeps 10,000 lines.",
                p,
            ))
    }

    fn keymap(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let q = self.keys.read(cx).value().trim().to_lowercase();
        let rows: Vec<Binding> = bindings(cx)
            .into_iter()
            .filter(|b| {
                q.is_empty()
                    || b.name.to_lowercase().contains(&q)
                    || b.context.to_lowercase().contains(&q)
                    || b.keys.iter().any(|k| k.to_lowercase().contains(&q))
            })
            .collect();
        let count = rows.len();
        v_flex()
            .gap(px(10.))
            .child(self.check(
                "double-shift",
                "Shift pressed twice opens Search Everywhere",
                self.draft.double_shift,
                |v, b| v.double_shift = b,
                cx,
            ))
            .child(section("Actions", p))
            .child(Input::new(&self.keys).small())
            .child(
                v_flex()
                    .border_1()
                    .border_color(p.border)
                    .rounded(px(6.))
                    .children(rows.into_iter().enumerate().map(|(i, b)| {
                        h_flex()
                            .px(px(8.))
                            .py(px(3.))
                            .gap(px(8.))
                            .when(i > 0, |d| d.border_t_1().border_color(p.border))
                            .child(div().flex_1().min_w_0().truncate().child(b.name))
                            .child(
                                div()
                                    .w(px(110.))
                                    .flex_none()
                                    .truncate()
                                    .text_color(p.muted)
                                    .child(b.context),
                            )
                            .child(
                                h_flex()
                                    .w(px(150.))
                                    .flex_none()
                                    .justify_end()
                                    .gap(px(4.))
                                    .children(b.keys.into_iter().map(|k| {
                                        div()
                                            .px(px(5.))
                                            .rounded(px(4.))
                                            .bg(p.surface_2)
                                            .border_1()
                                            .border_color(p.border)
                                            .text_size(px(11.5))
                                            .child(k)
                                    })),
                            )
                    })),
            )
            .child(note(
                &format!("{count} actions. Keys are IntelliJ's wherever IntelliJ has the action."),
                p,
            ))
    }

    fn notifications(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let n = &self.draft.notify;
        let col = |label: &'static str| {
            div()
                .w(px(84.))
                .flex_none()
                .text_center()
                .text_color(p.muted)
                .child(label)
        };
        let mut table = v_flex()
            .border_1()
            .border_color(p.border)
            .rounded(px(6.))
            .child(
                h_flex()
                    .px(px(8.))
                    .py(px(4.))
                    .bg(p.surface_2)
                    .child(div().flex_1().text_color(p.muted).child("When"))
                    .child(col("In the app"))
                    .child(col("System"))
                    .child(col("Dock badge"))
                    .child(col("Sound")),
            );
        for e in Event::ALL {
            let ways = n.ways(e);
            let cell = |slot: u8, on: bool, cx: &mut Context<Self>| {
                h_flex().w(px(84.)).flex_none().justify_center().child(
                    Checkbox::new(SharedString::from(format!("notify-{e:?}-{slot}")))
                        .checked(on)
                        .on_click(cx.listener(move |this, b: &bool, _, cx| {
                            let w: &mut Ways = this.draft.notify.ways_mut(e);
                            match slot {
                                0 => w.app = *b,
                                1 => w.system = *b,
                                2 => w.badge = *b,
                                _ => w.sound = *b,
                            }
                            cx.notify();
                        })),
                )
            };
            table = table.child(
                h_flex()
                    .px(px(8.))
                    .py(px(4.))
                    .border_t_1()
                    .border_color(p.border)
                    .child(div().flex_1().child(e.label()))
                    .child(cell(0, ways.app, cx))
                    .child(cell(1, ways.system, cx))
                    .child(cell(2, ways.badge, cx))
                    .child(cell(3, ways.sound, cx)),
            );
        }
        let places: Vec<(String, String)> = self
            .places
            .read(cx)
            .list()
            .iter()
            .map(|(c, _)| (c.id.clone(), c.name.clone()))
            .collect();
        let mut from = h_flex().flex_wrap().gap(px(14.));
        for (id, name) in places {
            let on = n.from(&id);
            let key = id.clone();
            from = from.child(
                Checkbox::new(SharedString::from(format!("notify-from-{id}")))
                    .label(name)
                    .checked(on)
                    .on_click(cx.listener(move |this, b: &bool, _, cx| {
                        let quiet = &mut this.draft.notify.quiet_places;
                        quiet.retain(|p| p != &key);
                        if !*b {
                            quiet.push(key.clone());
                        }
                        cx.notify();
                    })),
            );
        }
        v_flex()
            .gap(px(10.))
            .child(table)
            .child(section("From", p))
            .child(from)
            .child(note(
                "A place not ticked shows in the bell's list only: no system notification, badge or sound.",
                p,
            ))
            .child(section("Quiet", p))
            .child(self.check(
                "while-looking",
                "No system notification for the environment in front of you",
                n.while_looking,
                |v, b| v.notify.while_looking = b,
                cx,
            ))
            .child(note(
                "The system's own notifications follow Focus and Do Not Disturb, as macOS has them.",
                p,
            ))
    }

    fn places_page(&mut self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let states: Vec<(String, Connection)> = self
            .places
            .read(cx)
            .list()
            .iter()
            .map(|(c, s)| (c.id.clone(), s.connection.clone()))
            .collect();
        let last = self.rows.len().saturating_sub(1);
        let mut list = v_flex().gap(px(6.));
        for (i, row) in self.rows.iter().enumerate() {
            let state = states
                .iter()
                .find(|(id, _)| id == &row.id)
                .map(|(_, c)| c.clone())
                .unwrap_or_default();
            let (status, tone) = connection_label(&state, p);
            let id = row.id.clone();
            list = list.child(
                v_flex()
                    .p(px(8.))
                    .gap(px(6.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(p.border)
                    .when(row.removed, |d| d.opacity(0.55))
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .child(div().w(px(220.)).child(Input::new(&row.name).small()))
                            .child(crate::ui::chip("Server", p))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(p.muted)
                                    .child(row.url.clone()),
                            )
                            .child(div().text_color(tone).child(status)),
                    )
                    .child(
                        h_flex()
                            .gap(px(6.))
                            .child(if i > 0 && !row.removed {
                                button(SharedString::from(format!("place-up-{id}")), "Move Up", p)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.rows.swap(i, i - 1);
                                        cx.notify();
                                    }))
                            } else {
                                disabled(SharedString::from(format!("place-up-{id}")), "Move Up", p)
                            })
                            .child(if i < last && !row.removed {
                                button(
                                    SharedString::from(format!("place-down-{id}")),
                                    "Move Down",
                                    p,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.rows.swap(i, i + 1);
                                        cx.notify();
                                    },
                                ))
                            } else {
                                disabled(
                                    SharedString::from(format!("place-down-{id}")),
                                    "Move Down",
                                    p,
                                )
                            })
                            .child({
                                let id = id.clone();
                                button(
                                    SharedString::from(format!("place-signin-{id}")),
                                    "Sign In Again…",
                                    p,
                                )
                                .on_click(cx.listener(
                                    move |_, _, _, cx| cx.emit(SettingsEvent::SignIn(id.clone())),
                                ))
                            })
                            .child(div().flex_1())
                            .child(if row.removed {
                                h_flex()
                                    .gap(px(8.))
                                    .child(div().text_color(p.muted).child("Removed when applied"))
                                    .child(
                                        button(
                                            SharedString::from(format!("place-keep-{id}")),
                                            "Keep",
                                            p,
                                        )
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.rows[i].removed = false;
                                                cx.notify();
                                            }),
                                        ),
                                    )
                                    .into_any_element()
                            } else {
                                button(
                                    SharedString::from(format!("place-remove-{id}")),
                                    "Remove",
                                    p,
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.rows[i].removed = true;
                                    cx.notify();
                                }))
                                .into_any_element()
                            }),
                    ),
            );
        }
        v_flex()
            .gap(px(10.))
            .child(list)
            .child(
                h_flex().child(
                    button("place-add", "Add a Place…", p)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::AddPlace))),
                ),
            )
            .child(note(
                "The list is in this order, in the sidebar and on Home. Removing a place forgets it here, its token with it; its environments carry on there.",
                p,
            ))
    }

    fn browser(&self, id: &str, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let places = self.places.read(cx);
        let Some((config, state)) = places.get(id) else {
            return v_flex();
        };
        let admin = state.me.as_ref().is_some_and(|m| m.admin);
        let base = config.url.trim_end_matches('/').to_string();
        let mut pages: Vec<(&str, &str, &str)> = vec![
            (
                "Account",
                "/account",
                "Your password, sign-in keys, SSH keys and access tokens",
            ),
            ("Templates", "/templates", "What environments are made from"),
            (
                "Files and packs",
                "/files",
                "Your packs, and the copies of each",
            ),
            ("Teams", "/teams", "The teams you are in"),
            ("Images", "/images", "Images in this place's registry"),
            ("Settings", "/settings", "Your preferences on this server"),
        ];
        if admin {
            pages.extend([
                ("Users", "/admin/users", "Everyone who can sign in"),
                (
                    "Workers",
                    "/admin/workers",
                    "The machines environments run on",
                ),
                ("Audit log", "/admin/audit", "What was done, by whom"),
            ]);
        }
        let mut list = v_flex().gap(px(2.));
        for (title, path, what) in pages {
            let url = format!("{base}{path}");
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("browser-{path}")))
                    .gap(px(10.))
                    .px(px(8.))
                    .py(px(5.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(|s| s.bg(p.surface_2))
                    .child(
                        div()
                            .w(px(130.))
                            .text_color(p.accent)
                            .child(format!("{title} ↗")),
                    )
                    .child(div().flex_1().text_color(p.muted).child(what))
                    .on_click(move |_, _, cx| cx.open_url(&url)),
            );
        }
        v_flex()
            .gap(px(10.))
            .child(note(
                "Administering a place stays in its own web pages, which always match its server's version.",
                p,
            ))
            .child(list)
    }

    fn check(
        &self,
        id: &'static str,
        label: &'static str,
        on: bool,
        set: fn(&mut Values, bool),
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        Checkbox::new(SharedString::from(format!("setting-{id}")))
            .label(label)
            .checked(on)
            .on_click(cx.listener(move |this, b: &bool, _, cx| {
                let b = *b;
                this.edit(|v| set(v, b), cx)
            }))
    }

    fn stepper(
        &self,
        id: &'static str,
        value: f32,
        default: f32,
        set: fn(&mut Values, f32),
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.amount(
            Amount {
                id,
                value,
                default,
                step: 0.5,
                range: (*FONT_SIZES.start(), *FONT_SIZES.end()),
                show: |v| format!("{v:.1} pt"),
            },
            set,
            p,
            cx,
        )
    }

    /// A number set a step at a time, with a way back to its default.
    fn amount(
        &self,
        a: Amount,
        set: fn(&mut Values, f32),
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Amount {
            id,
            value,
            default,
            step,
            range,
            show,
        } = a;
        let by = move |sign: f32| {
            move |this: &mut SettingsView,
                  _: &gpui::ClickEvent,
                  _: &mut Window,
                  cx: &mut Context<SettingsView>| {
                // Whole steps, so they add up without drifting.
                let next = (((value + sign * step) / step).round() * step).clamp(range.0, range.1);
                set(&mut this.draft, next);
                cx.notify();
            }
        };
        h_flex()
            .gap(px(6.))
            .child(
                button(SharedString::from(format!("{id}-less")), "−", p)
                    .on_click(cx.listener(by(-1.))),
            )
            .child(div().min_w(px(44.)).text_center().child(show(value)))
            .child(
                button(SharedString::from(format!("{id}-more")), "+", p)
                    .on_click(cx.listener(by(1.))),
            )
            .when((value - default).abs() > step / 100., |d| {
                d.child(
                    div()
                        .id(SharedString::from(format!("{id}-reset")))
                        .ml(px(6.))
                        .text_color(p.accent)
                        .cursor_pointer()
                        .child("Reset")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            set(&mut this.draft, default);
                            cx.notify();
                        })),
                )
            })
    }

    /// One of a few choices, side by side, the one chosen in the accent.
    fn segments<T: Copy + PartialEq + 'static>(
        &self,
        id: &'static str,
        options: &[(&'static str, T)],
        chosen: T,
        set: fn(&mut Values, T),
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .rounded(px(6.))
            .border_1()
            .border_color(p.border_strong)
            .overflow_hidden()
            .children(options.iter().map(|&(label, value)| {
                let on = value == chosen;
                let hover = p.surface_2;
                div()
                    .id(SharedString::from(format!(
                        "{id}-{}",
                        label.to_lowercase().replace(' ', "-")
                    )))
                    .px(px(12.))
                    .py(px(3.))
                    .cursor_pointer()
                    .when(on, |d| d.bg(p.accent).text_color(p.on_accent))
                    .when(!on, move |d| d.hover(move |s| s.bg(hover)))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        set(&mut this.draft, value);
                        cx.notify();
                    }))
            }))
    }

    /// One of a list, chosen from a menu: each a label and its value.
    fn choice(
        &self,
        id: &'static str,
        label: String,
        options: Vec<(String, String)>,
        chosen: String,
        set: fn(&mut Values, String),
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        use gpui_component::button::Button;
        use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
        let me = cx.entity().downgrade();
        Button::new(SharedString::from(format!("setting-{id}")))
            .small()
            .outline()
            .label(format!("{label} ▾"))
            .dropdown_menu(move |mut menu, _, cx| {
                crate::ui::modal(cx);
                menu = menu.max_h(px(360.)).scrollable(true);
                for (name, value) in options.clone() {
                    let me = me.clone();
                    let on = value == chosen;
                    menu = menu.item(PopupMenuItem::new(name).checked(on).on_click(
                        move |_, _, cx| {
                            let value = value.clone();
                            let _ = me.update(cx, |v, cx| {
                                set(&mut v.draft, value);
                                cx.notify();
                            });
                        },
                    ));
                }
                menu
            })
    }
}

/// A number a setting is, and how it steps and shows.
struct Amount {
    id: &'static str,
    value: f32,
    default: f32,
    step: f32,
    range: (f32, f32),
    show: fn(f32) -> String,
}

/// The font families installed whose characters are all one width, which
/// a terminal can be drawn in.
fn monospaced_fonts(cx: &App) -> Vec<String> {
    let ts = cx.text_system();
    ts.all_font_names()
        .into_iter()
        .filter(|name| !name.starts_with('.') && !name.starts_with('#'))
        .filter(|name| {
            let id = ts.resolve_font(&gpui::font(name.clone()));
            // A family not found resolves to another, which is passed over.
            if ts.get_font_for_id(id).is_none_or(|f| &f.family != name) {
                return false;
            }
            let width = |c| ts.advance(id, px(12.), c).map(|s| s.width).ok();
            matches!(
                (width('i'), width('W'), width('.')),
                (Some(a), Some(b), Some(c)) if a > px(0.) && a == b && b == c
            )
        })
        .collect()
}

/// A heading over a page's settings.
fn section(title: &str, p: &Palette) -> impl IntoElement {
    div()
        .pt(px(4.))
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(p.muted)
        .child(title.to_uppercase())
}

/// A setting's label, beside what sets it.
fn field(label: &str, p: &Palette) -> gpui::Div {
    h_flex().gap(px(12.)).child(
        div()
            .w(px(90.))
            .text_color(p.muted)
            .child(label.to_string()),
    )
}

/// A line explaining a page or a setting.
fn note(text: &str, p: &Palette) -> impl IntoElement {
    div()
        .max_w(px(560.))
        .text_size(px(11.5))
        .text_color(p.muted)
        .child(text.to_string())
}

/// How a place's connection is, in a word, and the colour it is said in.
pub fn connection_label(c: &Connection, p: &Palette) -> (String, gpui::Hsla) {
    match c {
        Connection::Connecting => ("Connecting…".into(), p.muted),
        Connection::Online => ("Online".into(), p.good),
        Connection::Offline { .. } => ("Offline".into(), p.bad),
        Connection::SignIn => ("Needs signing in".into(), p.wait),
        Connection::Unsupported { server } => (format!("Server {server} is not supported"), p.bad),
    }
}

/// An action and the keys bound to it, for the Keymap page, with where
/// the keys work.
struct Binding {
    name: String,
    context: String,
    keys: Vec<String>,
}

/// Every action bound to keys, by name. An action bound to the same keys
/// in several places is one row: gpui-component binds Escape to Cancel in
/// each of its pop-ups, say.
fn bindings(cx: &App) -> Vec<Binding> {
    use gpui::AsKeystroke;
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    // (name, context) -> keys, in the order bound.
    let mut by_context: Vec<(String, String, Vec<String>)> = vec![];
    for b in keymap.bindings() {
        let action = b.action().name();
        if action.ends_with("::NoAction") {
            continue;
        }
        let name = humanise(action);
        let context = b
            .predicate()
            .map(|p| p.to_string())
            .unwrap_or_else(|| "Everywhere".into());
        let keys: Vec<String> = b
            .keystrokes()
            .iter()
            .map(|k| gpui_component::kbd::Kbd::format(k.as_keystroke()))
            .collect();
        let keys = keys.join(" ");
        match by_context
            .iter_mut()
            .find(|o| o.0 == name && o.1 == context)
        {
            Some(o) => {
                if !o.2.contains(&keys) {
                    o.2.push(keys);
                }
            }
            None => by_context.push((name, context, vec![keys])),
        }
    }
    let mut out: Vec<(String, Vec<String>, Vec<String>)> = vec![];
    for (name, context, keys) in by_context {
        match out.iter_mut().find(|o| o.0 == name && o.2 == keys) {
            Some(o) => o.1.push(context),
            None => out.push((name, vec![context], keys)),
        }
    }
    let mut out: Vec<Binding> = out
        .into_iter()
        .map(|(name, contexts, keys)| Binding {
            name,
            context: if contexts.len() > 2 {
                format!("{} places", contexts.len())
            } else {
                contexts.join(", ")
            },
            keys,
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name).then(a.context.cmp(&b.context)));
    out
}

/// "envtab::ToggleProject" as "Toggle Project".
pub fn humanise(action: &str) -> String {
    let last = action.rsplit("::").next().unwrap_or(action);
    let mut out = String::new();
    let chars: Vec<char> = last.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
        let prev = i.checked_sub(1).map(|j| chars[j]);
        let boundary = c.is_uppercase()
            && prev.is_some_and(|p| {
                p.is_lowercase() || p.is_ascii_digit() || (p.is_uppercase() && next_lower)
            });
        if boundary {
            out.push(' ');
        }
        out.push(if c == &'_' { ' ' } else { *c });
    }
    out
}
