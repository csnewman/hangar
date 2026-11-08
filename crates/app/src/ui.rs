//! The design's small pieces, shared by the window's views: status dots,
//! buttons, chips.

use gpui::{Div, Hsla, InteractiveElement, ParentElement, SharedString, Stateful, Styled, div, px};

use crate::model::Tone;
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
