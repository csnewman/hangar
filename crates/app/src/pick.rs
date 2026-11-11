//! A pop-up list to choose from, as IntelliJ's Compare with Branch and
//! Compare with Revision have: what it holds read as it opens, narrowed as
//! text is typed, ↑ ↓ moving the mark, and Enter or a click taking it.

use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;

use gpui::{
    App, AppContext, Entity, Focusable, FontWeight, KeyDownEvent, ParentElement, Styled, Window,
    div, prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{WindowExt, h_flex, v_flex};

use crate::palette;

/// One of the list's rows: what it says, more about it, and what choosing
/// it gives.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub label: String,
    pub detail: Option<String>,
    pub value: String,
}

/// The rows a query leaves: those whose label or detail has it, ignoring
/// case.
pub fn narrowed<'a>(all: &'a [Choice], query: &str) -> Vec<&'a Choice> {
    let q = query.trim().to_lowercase();
    all.iter()
        .filter(|c| {
            q.is_empty()
                || c.label.to_lowercase().contains(&q)
                || c.detail
                    .as_ref()
                    .is_some_and(|d| d.to_lowercase().contains(&q))
        })
        .collect()
}

/// The most rows shown; typing finds the rest.
const SHOWN: usize = 200;

/// What is done with the row taken.
type Chosen = Rc<dyn Fn(Choice, &mut Window, &mut App)>;

struct Picking {
    query: Entity<InputState>,
    rows: Option<Result<Vec<Choice>, String>>,
    at: usize,
    empty: String,
    chosen: Chosen,
}

impl Picking {
    fn current(&self, cx: &App) -> Option<Choice> {
        let Some(Ok(all)) = &self.rows else {
            return None;
        };
        let query = self.query.read(cx).value();
        narrowed(all, &query).get(self.at).map(|c| (*c).clone())
    }
}

/// Opens the list, reading its rows from load; empty is said when there
/// are none. chosen has the row taken.
pub fn pick(
    title: &str,
    empty: &str,
    window: &mut Window,
    cx: &mut App,
    load: impl Future<Output = Result<Vec<Choice>, String>> + 'static,
    chosen: impl Fn(Choice, &mut Window, &mut App) + 'static,
) {
    let query = cx.new(|cx| InputState::new(window, cx).placeholder("Type to narrow"));
    let state = Rc::new(RefCell::new(Picking {
        query: query.clone(),
        rows: None,
        at: 0,
        empty: empty.to_string(),
        chosen: Rc::new(chosen),
    }));
    let handle = window.window_handle();
    {
        let state = state.clone();
        cx.subscribe(&query, move |_, e: &InputEvent, cx| match e {
            InputEvent::Change => {
                state.borrow_mut().at = 0;
                let _ = handle.update(cx, |_, window, _| window.refresh());
            }
            InputEvent::PressEnter { .. } => {
                let state = state.clone();
                let _ = handle.update(cx, |_, window, cx| take(&state, None, window, cx));
            }
            _ => {}
        })
        .detach();
    }
    {
        let state = state.clone();
        cx.spawn(async move |cx| {
            let rows = load.await;
            let _ = handle.update(cx, |_, window, _| {
                state.borrow_mut().rows = Some(rows);
                window.refresh();
            });
        })
        .detach();
    }
    let title = title.to_string();
    let first = std::cell::Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if first.replace(false) {
            window.focus(&query.focus_handle(cx), cx);
        }
        let p = palette::of(cx);
        let s = state.borrow();
        let mut list = v_flex().gap(px(1.));
        match &s.rows {
            None => list = list.child(note("Reading…", p.muted)),
            Some(Err(e)) => list = list.child(note(e, p.bad)),
            Some(Ok(all)) => {
                let rows = narrowed(all, &s.query.read(cx).value());
                if rows.is_empty() {
                    list = list.child(note(&s.empty, p.muted));
                }
                for (i, c) in rows.into_iter().take(SHOWN).enumerate() {
                    let on = i == s.at;
                    let st = state.clone();
                    list = list.child(
                        h_flex()
                            .id(("pick-row", i))
                            .debug_selector(move || format!("pick-row-{i}"))
                            .h(px(24.))
                            .px(px(8.))
                            .gap(px(8.))
                            .rounded(px(4.))
                            .cursor_pointer()
                            .when(on, |d| d.bg(p.selection))
                            .child(
                                div()
                                    .flex_none()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(c.label.clone()),
                            )
                            .when_some(c.detail.clone(), |d, detail| {
                                d.child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(11.))
                                        .text_color(p.muted)
                                        .child(detail),
                                )
                            })
                            .on_click(move |_, window, cx| take(&st, Some(i), window, cx)),
                    );
                }
            }
        }
        let keys = state.clone();
        dialog
            .title(
                div()
                    .text_size(px(12.))
                    .text_color(p.muted)
                    .child(title.clone()),
            )
            .w(px(480.))
            .margin_top(px(90.))
            .close_button(false)
            .overlay(false)
            .on_ok(|_, _, _| false)
            .child(
                v_flex()
                    .gap(px(6.))
                    .capture_key_down(move |e: &KeyDownEvent, window, cx| {
                        let len = match &keys.borrow().rows {
                            Some(Ok(all)) => {
                                narrowed(all, &keys.borrow().query.read(cx).value()).len()
                            }
                            _ => 0,
                        };
                        let mut s = keys.borrow_mut();
                        match e.keystroke.key.as_str() {
                            "down" => s.at = (s.at + 1).min(len.saturating_sub(1).min(SHOWN - 1)),
                            "up" => s.at = s.at.saturating_sub(1),
                            _ => return,
                        }
                        cx.stop_propagation();
                        window.refresh();
                    })
                    .child(Input::new(&query))
                    .child(
                        div()
                            .id("pick-rows")
                            .max_h(px(320.))
                            .overflow_y_scroll()
                            .child(list),
                    ),
            )
    });
}

/// Takes a row, the one marked when none is given, and closes the list.
fn take(state: &Rc<RefCell<Picking>>, row: Option<usize>, window: &mut Window, cx: &mut App) {
    if let Some(i) = row {
        state.borrow_mut().at = i;
    }
    let (choice, chosen) = {
        let s = state.borrow();
        (s.current(cx), s.chosen.clone())
    };
    let Some(choice) = choice else { return };
    window.close_dialog(cx);
    chosen(choice, window, cx);
}

fn note(text: &str, colour: gpui::Hsla) -> gpui::Div {
    div()
        .px(px(8.))
        .py(px(4.))
        .text_size(px(11.5))
        .text_color(colour)
        .child(text.to_string())
}
