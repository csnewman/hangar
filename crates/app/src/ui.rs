//! The design's small pieces, shared by the window's views: status dots,
//! buttons, chips.

use gpui::prelude::FluentBuilder;
use gpui::{
    Div, Hsla, InteractiveElement, IntoElement, ParentElement, SharedString, Stateful, Styled, div,
    px,
};

use crate::model::{Mark, Tone};
use crate::palette::Palette;

pub fn tone(t: Tone, p: &Palette) -> Hsla {
    match t {
        Tone::Wait => p.wait,
        Tone::Good => p.good,
        Tone::Bad => p.bad,
        Tone::Busy => p.busy,
        Tone::Idle | Tone::Ring => p.idle,
    }
}

/// A status dot, hollow for Ring.
pub fn dot(t: Tone, p: &Palette) -> Div {
    let d = div().size(px(8.)).rounded_full().flex_none();
    match t {
        Tone::Ring => d.border(px(1.5)).border_color(p.idle),
        t => d.bg(tone(t, p)),
    }
}

/// An environment's dot: its machine's ring around what is in it, with a
/// gap between so the two read apart.
pub fn mark(m: Mark, p: &Palette) -> Div {
    div()
        .size(px(10.))
        .rounded_full()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .border(px(1.5))
        .border_color(tone(m.ring, p))
        .when_some(m.fill, |d, f| {
            d.child(div().size(px(5.)).rounded_full().bg(tone(f, p)))
        })
}

/// A button as the design draws one.
pub fn button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    p: &Palette,
) -> Stateful<Div> {
    styled_button(id, label, p.surface, p.border_strong, p.text, p.surface_2)
}

/// The one button that says what to do next.
pub fn primary(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    p: &Palette,
) -> Stateful<Div> {
    styled_button(
        id,
        label,
        p.accent,
        p.accent,
        p.on_accent,
        p.accent.opacity(0.88),
    )
}

/// A button that cannot be pressed now: greyed, and without a hover.
pub fn disabled(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    p: &Palette,
) -> Stateful<Div> {
    styled_button(id, label, p.surface_2, p.border, p.muted, p.surface_2).cursor_default()
}

pub fn styled_button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    bg: Hsla,
    border: Hsla,
    fg: Hsla,
    hover: Hsla,
) -> Stateful<Div> {
    div()
        .id(id.into())
        .flex()
        .items_center()
        .gap_1p5()
        .px(px(10.))
        .py(px(3.))
        .rounded(px(6.))
        .border_1()
        .border_color(border)
        .bg(bg)
        .text_color(fg)
        .text_size(px(12.))
        .whitespace_nowrap()
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .child(label.into())
}

/// A small outlined label: a card's place.
pub fn chip(text: impl Into<SharedString>, p: &Palette) -> Div {
    div()
        .flex_none()
        .px(px(5.))
        .rounded(px(4.))
        .border_1()
        .border_color(p.border)
        .text_size(px(10.))
        .text_color(p.muted)
        .child(text.into())
}

/// The mark beside a task the agent set.
pub fn spark(p: &Palette) -> Div {
    div().flex_none().text_color(p.accent).child("✦")
}

/// The app's context menus while they are open, held weakly, so a menu
/// that goes without closing cannot leave the window held still.
#[derive(Default)]
pub struct OpenMenus(Vec<gpui::WeakEntity<gpui_component::menu::PopupMenu>>);

impl gpui::Global for OpenMenus {}

/// Just below gpui-component's menus (its POPUP_PRIORITY, 100, which it
/// does not export).
const BELOW_MENUS: usize = 99;

/// Makes a context menu modal, as a native one is, called from the menu's
/// builder: while it is open, the window behind it takes no hover and no
/// clicks (shield).
pub fn modal(cx: &mut gpui::Context<gpui_component::menu::PopupMenu>) {
    let me = cx.entity().downgrade();
    cx.default_global::<OpenMenus>().0.push(me);
    cx.subscribe_self(|_, _: &gpui::DismissEvent, cx| {
        let me = cx.entity().downgrade();
        cx.default_global::<OpenMenus>()
            .0
            .retain(|m| m != &me && m.upgrade().is_some());
        cx.refresh_windows();
    })
    .detach();
}

/// Whether one of the app's context menus is open.
pub fn menu_open(cx: &gpui::App) -> bool {
    cx.try_global::<OpenMenus>()
        .is_some_and(|m| m.0.iter().any(|w| w.upgrade().is_some()))
}

/// What lies between an open menu and the rest of the window: it takes the
/// pointer, so nothing behind the menu reacts to it, and a click there only
/// closes the menu.
pub fn shield(cx: &gpui::App) -> Option<gpui::AnyElement> {
    menu_open(cx).then(|| {
        gpui::deferred(
            div()
                .id("menu-shield")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .occlude(),
        )
        .with_priority(BELOW_MENUS)
        .into_any_element()
    })
}

/// Marks a menu's first item, for Enter, as IntelliJ's pop-up lists open:
/// the menu marks a row only by its keys, which reach it once it is drawn,
/// so this is called as the frame drawing it is made.
pub fn mark_first(window: &mut gpui::Window, cx: &mut gpui::App) {
    window.defer(cx, |window, cx| {
        if let Ok(down) = cx.build_action("ui::SelectDown", None) {
            window.dispatch_action(down, cx);
        }
    });
}
