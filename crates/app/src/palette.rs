//! The app's colours, light and dark: the design's tokens
//! (docs/native-app.md's UX plan), also handed to gpui-component's theme
//! so its inputs and buttons match.

use gpui::{App, Hsla, Window, rgb, rgba};
use gpui_component::{ActiveTheme, Theme};

#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub bg: Hsla,
    pub surface: Hsla,
    pub surface_2: Hsla,
    pub sidebar: Hsla,
    pub sidebar_active: Hsla,
    /// The row selected in a tool window's list or tree.
    pub selection: Hsla,
    pub border: Hsla,
    pub border_strong: Hsla,
    pub text: Hsla,
    pub muted: Hsla,
    pub accent: Hsla,
    pub accent_soft: Hsla,
    pub on_accent: Hsla,
    /// Status: done or online.
    pub good: Hsla,
    /// Status: something running.
    pub busy: Hsla,
    /// Status: wants the user.
    pub wait: Hsla,
    pub idle: Hsla,
    pub bad: Hsla,
    /// Someone else's hand in a file: an agent's or another tab's edits.
    pub peer: Hsla,
    /// A folder's icon in the file trees.
    pub folder: Hsla,
    /// Behind a row that needs the user.
    pub warn_bg: Hsla,
    pub warn_border: Hsla,
    pub term_bg: Hsla,
    pub term_fg: Hsla,
    /// A terminal's own background, and its tab's.
    pub term_tab: Hsla,
}

fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

pub fn light() -> Palette {
    Palette {
        bg: c(0xf1f4f4),
        surface: c(0xffffff),
        surface_2: c(0xe8eeee),
        sidebar: c(0xe9eeee),
        sidebar_active: c(0xd3e7e5),
        selection: c(0xc5e2df),
        border: c(0xd5dfdf),
        border_strong: c(0xb9c8c8),
        text: c(0x102021),
        muted: c(0x5a6f70),
        accent: c(0x0b7a76),
        accent_soft: c(0xdcefed),
        on_accent: c(0xffffff),
        good: c(0x2b8a4b),
        busy: c(0xb9770c),
        wait: c(0x5357d1),
        idle: c(0x93a3a4),
        bad: c(0xc6383a),
        peer: c(0xd9622b),
        folder: c(0xc09553),
        warn_bg: c(0xeeeffc),
        warn_border: c(0xc4c6f3),
        term_bg: c(0xeef2f2),
        term_fg: c(0x102021),
        term_tab: c(0xffffff),
    }
}

pub fn dark() -> Palette {
    Palette {
        bg: c(0x0c1415),
        surface: c(0x121c1d),
        surface_2: c(0x182426),
        sidebar: c(0x0f1819),
        sidebar_active: c(0x173235),
        selection: c(0x214a4d),
        border: c(0x213133),
        border_strong: c(0x2e4144),
        text: c(0xe1ebea),
        muted: c(0x869d9e),
        accent: c(0x3cc3ba),
        accent_soft: c(0x123334),
        on_accent: c(0x062221),
        good: c(0x4fcb7c),
        busy: c(0xeeb04a),
        wait: c(0x8e92f6),
        idle: c(0x5f7476),
        bad: c(0xf2716f),
        peer: c(0xf08a55),
        folder: c(0xc9a064),
        warn_bg: c(0x1a1c3a),
        warn_border: c(0x34387a),
        term_bg: c(0x081011),
        term_fg: c(0xcfdddc),
        term_tab: c(0x132022),
    }
}

/// The palette for the theme's mode.
pub fn of(cx: &App) -> Palette {
    if cx.theme().is_dark() {
        dark()
    } else {
        light()
    }
}

/// Follows the system's appearance, with the palette's colours in
/// gpui-component's theme.
pub fn sync(window: Option<&mut Window>, cx: &mut App) {
    if let Some(Forced(mode)) = cx.try_global::<Forced>() {
        Theme::change(*mode, window, cx);
    } else {
        Theme::sync_system_appearance(window, cx);
    }
    apply(cx);
}

/// A mode kept whatever the system's appearance.
struct Forced(gpui_component::ThemeMode);

impl gpui::Global for Forced {}

/// Keeps the app light or dark, whatever the system's appearance.
pub fn force(mode: gpui_component::ThemeMode, cx: &mut App) {
    cx.set_global(Forced(mode));
    Theme::change(mode, None, cx);
    apply(cx);
}

/// Puts the palette's colours into gpui-component's theme, for the mode it
/// is in.
pub fn apply(cx: &mut App) {
    let p = of(cx);
    Theme::update(cx, |t| {
        t.font_size = gpui::px(13.);
        t.radius = gpui::px(6.);
        let k = &mut t.colors;
        k.background = p.surface;
        k.foreground = p.text;
        k.border = p.border;
        k.input = p.border_strong;
        k.ring = p.accent;
        k.caret = p.accent;
        k.selection = with_alpha(p.accent, 0.25);
        k.muted = p.surface_2;
        k.muted_foreground = p.muted;
        k.accent = p.accent_soft;
        k.accent_foreground = p.accent;
        k.primary = p.accent;
        k.primary_hover = p.accent.opacity(0.9);
        k.primary_active = p.accent.opacity(0.8);
        k.primary_foreground = p.on_accent;
        k.secondary = p.surface;
        k.secondary_hover = p.surface_2;
        k.secondary_active = p.sidebar_active;
        k.secondary_foreground = p.text;
        k.popover = p.surface;
        k.popover_foreground = p.text;
        k.sidebar = p.sidebar;
        k.sidebar_border = p.border;
        k.sidebar_accent = p.sidebar_active;
        k.title_bar = p.sidebar;
        k.title_bar_border = p.border;
        k.danger = p.bad;
        k.success = p.good;
        k.warning = p.busy;
        k.info = p.wait;
        k.overlay = rgba(0x06141452).into();
    });
}

fn with_alpha(c: Hsla, a: f32) -> Hsla {
    Hsla { a, ..c }
}
