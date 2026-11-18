//! What a place's pages in Settings are built of: headings, notes,
//! labelled fields, choices from a menu, list items, inputs, and buttons
//! that ask again before deleting, drawn alike on every page.

use std::rc::Rc;

use gpui::{
    AnyElement, App, Context, Div, Entity, FontWeight, IntoElement, ParentElement, SharedString,
    Stateful, Styled, WeakEntity, Window, div, prelude::*, px,
};
use gpui_component::button::Button;
use gpui_component::input::{InputState, TextareaState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{Sizable, h_flex, v_flex};

use crate::palette::Palette;
use crate::ui::{button, disabled};

/// A heading over a part of a page.
pub fn section(title: &str, p: &Palette) -> Div {
    div()
        .mt(px(8.))
        .text_size(px(10.5))
        .text_color(p.muted)
        .child(title.to_uppercase())
}

/// A line explaining a page or a part of it.
pub fn note(text: impl Into<SharedString>, p: &Palette) -> Div {
    div()
        .max_w(px(640.))
        .text_size(px(11.5))
        .text_color(p.muted)
        .child(text.into())
}

/// What went wrong, in the colour of trouble.
pub fn error(text: impl Into<SharedString>, p: &Palette) -> Div {
    div().max_w(px(640.)).text_color(p.bad).child(text.into())
}

/// A labelled field: the label on the left, what is set on the right.
pub fn field(label: &str, p: &Palette) -> Div {
    h_flex().gap(px(10.)).items_start().child(
        div()
            .w(px(130.))
            .flex_none()
            .pt(px(3.))
            .text_color(p.muted)
            .child(label.to_string()),
    )
}

/// A bordered row of a list on a page.
pub fn row(p: &Palette) -> Div {
    h_flex()
        .gap(px(8.))
        .px(px(6.))
        .py(px(3.))
        .min_w_0()
        .border_1()
        .border_color(p.border)
        .rounded(px(5.))
}

/// Text in the monospaced face, at a row's size.
pub fn mono(text: impl Into<SharedString>) -> Div {
    div()
        .font_family(crate::diff::mono())
        .text_size(px(11.))
        .child(text.into())
}

/// Small muted text, as a row's detail.
pub fn small(text: impl Into<SharedString>, p: &Palette) -> Div {
    div()
        .text_size(px(10.5))
        .text_color(p.muted)
        .child(text.into())
}

/// A small rounded label: a place a pack is in, a role, a state.
pub fn chip(text: impl Into<SharedString>, p: &Palette) -> Div {
    div()
        .px(px(6.))
        .rounded(px(9.))
        .border_1()
        .border_color(p.border_strong)
        .text_size(px(10.))
        .text_color(p.muted)
        .whitespace_nowrap()
        .child(text.into())
}

/// An item of a page's list on its left: its title, a word beside it, and
/// a line under it.
pub fn item(
    id: impl Into<SharedString>,
    title: impl Into<SharedString>,
    aside: impl Into<SharedString>,
    sub: Option<AnyElement>,
    on: bool,
    p: &Palette,
) -> Stateful<Div> {
    let hover = p.surface_2;
    v_flex()
        .id(id.into())
        .px(px(8.))
        .py(px(5.))
        .gap(px(2.))
        .rounded(px(6.))
        .border_1()
        .cursor_pointer()
        .border_color(if on { p.accent } else { p.border })
        .when(on, |d| d.bg(p.accent_soft))
        .when(!on, move |d| d.hover(move |s| s.bg(hover)))
        .child(
            h_flex()
                .gap(px(6.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title.into()),
                )
                .child(small(aside, p)),
        )
        .when_some(sub, |d, s| d.child(s))
}

/// A page's list on the left and the chosen item on the right.
pub fn split(left: impl IntoElement, right: impl IntoElement) -> Div {
    h_flex()
        .items_start()
        .gap(px(12.))
        .child(v_flex().w(px(220.)).flex_none().gap(px(4.)).child(left))
        .child(v_flex().flex_1().min_w_0().gap(px(4.)).child(right))
}

/// A one-line input, holding text.
pub fn input(
    value: &str,
    placeholder: &str,
    window: &mut Window,
    cx: &mut App,
) -> Entity<InputState> {
    let (value, placeholder) = (value.to_string(), placeholder.to_string());
    cx.new(|cx| {
        let mut s = InputState::new(window, cx).placeholder(placeholder);
        s.set_value(value, window, cx);
        s
    })
}

/// A box of several lines, holding text.
pub fn textarea(value: &str, window: &mut Window, cx: &mut App) -> Entity<TextareaState> {
    let value = value.to_string();
    cx.new(|cx| {
        let mut s = TextareaState::new(window, cx);
        s.set_value(value, window, cx);
        s
    })
}

/// What an input holds, trimmed.
pub fn text(input: &Entity<InputState>, cx: &App) -> String {
    input.read(cx).value().trim().to_string()
}

/// One of a list, chosen from a menu: the button shows the chosen one's
/// label, and choosing calls set.
pub fn choice<V: 'static, T: Clone + PartialEq + 'static>(
    id: impl Into<SharedString>,
    options: Vec<(String, T)>,
    chosen: Option<T>,
    me: WeakEntity<V>,
    set: impl Fn(&mut V, T, &mut Window, &mut Context<V>) + 'static,
) -> impl IntoElement {
    let label = options
        .iter()
        .find(|(_, v)| Some(v) == chosen.as_ref())
        .map_or_else(|| "Choose".to_string(), |(l, _)| l.clone());
    let set = Rc::new(set);
    Button::new(id.into())
        .small()
        .outline()
        .label(format!("{label} ▾"))
        .dropdown_menu(move |mut menu, _, cx| {
            crate::ui::modal(cx);
            menu = menu.max_h(px(360.)).scrollable(true);
            for (name, value) in options.clone() {
                let (me, set) = (me.clone(), set.clone());
                let on = Some(&value) == chosen.as_ref();
                menu = menu.item(PopupMenuItem::new(name).checked(on).on_click(
                    move |_, window, cx| {
                        let (value, set) = (value.clone(), set.clone());
                        let _ = me.update(cx, |v, cx| set(v, value, window, cx));
                    },
                ));
            }
            menu
        })
}

/// A button that deletes or removes something only when pressed twice:
/// the first press arms it, saying what it will do, the second does it.
/// armed is whether this one is armed.
pub fn twice(
    id: impl Into<SharedString>,
    label: &str,
    confirm: &str,
    armed: bool,
    busy: bool,
    p: &Palette,
) -> Stateful<Div> {
    if busy {
        disabled(id, label.to_string(), p)
    } else if armed {
        crate::ui::styled_button(id, confirm.to_string(), p.bad, p.bad, p.on_accent, p.bad)
    } else {
        button(id, label.to_string(), p)
    }
}
