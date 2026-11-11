//! What gpui-component's editor does with what the Code tab's shared
//! documents ask of it, which the app's editor builds on: an edit from
//! elsewhere puts the caret after it and enters the one undo history, so the
//! app keeps the selection itself (mapped through the edit) and its own
//! history (as hangar-proto change sets, taking Undo and Redo before the
//! editor does); local and applied edits are told alike; a large file; and
//! other people's selections as decorations, their carets placed by
//! range_to_bounds; and the text keeping its width as a hover opens.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use gpui::{EntityInputHandler, HighlightStyle, TestAppContext, hsla};
use gpui_component::input::{EditorState, InputEditsEvent, InputEvent, TextDecoration};
use lsp_types::{Position, Range, TextEdit};

fn insert_at(line: u32, character: u32, text: &str) -> Vec<TextEdit> {
    vec![TextEdit {
        range: Range::new(
            Position::new(line, character),
            Position::new(line, character),
        ),
        new_text: text.into(),
    }]
}

#[gpui::test]
fn an_applied_edit_puts_the_caret_after_it(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (editor, cx) = cx.add_window_view(|window, cx| {
        let mut e = EditorState::new(window, cx).language("rust");
        e.set_value("fn one() {}\nfn two() {}\n", window, cx);
        e
    });
    cx.update(|window, cx| {
        editor.update(cx, |e, cx| {
            // The caret after "fn two", on the second line.
            e.set_selected_range(18..18, cx);
            e.apply_lsp_edits(&insert_at(0, 0, "// remote\n"), window, cx);
        })
    });
    editor.read_with(cx, |e, _| {
        assert_eq!(e.value(), "// remote\nfn one() {}\nfn two() {}\n");
        assert_eq!(
            e.selected_range(),
            10..10,
            "the caret is after the applied edit"
        );
    });
}

#[gpui::test]
fn an_applied_edit_is_in_the_undo_history(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (editor, cx) = cx.add_window_view(|window, cx| {
        let mut e = EditorState::new(window, cx);
        e.set_value("one\ntwo\n", window, cx);
        e
    });
    cx.update(|window, cx| {
        editor.update(cx, |e, cx| {
            e.focus(window, cx);
            // Typed at the end of "two".
            e.replace_text_in_range(Some(7..7), "!", window, cx);
            // Someone else's line, above it.
            e.apply_lsp_edits(&insert_at(0, 0, "zero\n"), window, cx);
        })
    });
    editor.read_with(cx, |e, _| assert_eq!(e.value(), "zero\none\ntwo!\n"));
    cx.update(|window, cx| {
        editor.update(cx, |_, cx| {
            window.dispatch_action(Box::new(gpui_component::input::Undo), cx)
        })
    });
    editor.read_with(cx, |e, _| {
        assert_eq!(
            e.value(),
            "one\ntwo!\n",
            "undo takes back the applied edit, not the local one"
        )
    });
}

#[gpui::test]
fn local_and_applied_edits_are_told_alike(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (editor, cx) = cx.add_window_view(|window, cx| {
        let mut e = EditorState::new(window, cx);
        e.set_value("abc", window, cx);
        e
    });
    let seen: Rc<RefCell<Vec<String>>> = Rc::default();
    let log = seen.clone();
    let log2 = seen.clone();
    let _subs = cx.update(|_, cx| {
        (
            cx.subscribe(&editor, move |_, event: &InputEvent, _| {
                if matches!(event, InputEvent::Change) {
                    log.borrow_mut().push("Change".into());
                }
            }),
            cx.subscribe(&editor, move |_, event: &InputEditsEvent, _| {
                for (e, _) in event.edits() {
                    log2.borrow_mut().push(format!(
                        "edit {}..{} -> ..{}",
                        e.start_byte, e.old_end_byte, e.new_end_byte
                    ));
                }
            }),
        )
    });
    cx.update(|window, cx| {
        editor.update(cx, |e, cx| {
            e.replace_text_in_range(Some(3..3), "d", window, cx)
        })
    });
    cx.run_until_parked();
    let local = seen.borrow().len();
    cx.update(|window, cx| {
        editor.update(cx, |e, cx| {
            e.apply_lsp_edits(&insert_at(0, 0, "X"), window, cx)
        })
    });
    cx.run_until_parked();
    let seen = seen.borrow();
    assert_eq!(seen[..local], ["edit 3..3 -> ..4", "Change"]);
    assert_eq!(seen[local..], ["edit 0..0 -> ..1", "Change"]);
}

#[gpui::test]
fn a_large_file(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let text: String = (0..100_000)
        .map(|i| format!("let line_{i} = {i}; // some text on the line\n"))
        .collect();
    let (editor, cx) =
        cx.add_window_view(|window, cx| EditorState::new(window, cx).language("rust"));
    let start = Instant::now();
    cx.update(|window, cx| editor.update(cx, |e, cx| e.set_value(text.clone(), window, cx)));
    cx.run_until_parked();
    eprintln!("100k lines loaded in {:?}", start.elapsed());
    let start = Instant::now();
    for i in 0..100 {
        cx.update(|window, cx| {
            editor.update(cx, |e, cx| {
                e.apply_lsp_edits(&insert_at(50_000 + i, 0, "x"), window, cx)
            })
        });
    }
    cx.run_until_parked();
    eprintln!("100 remote edits mid-file in {:?}", start.elapsed());
    let start = Instant::now();
    for i in 0..100 {
        cx.update(|window, cx| {
            editor.update(cx, |e, cx| {
                let at = e.value().len() / 2 + i;
                e.replace_text_in_range(Some(at..at), "y", window, cx)
            })
        });
    }
    cx.run_until_parked();
    eprintln!("100 local keystrokes mid-file in {:?}", start.elapsed());
}

#[gpui::test]
fn other_peoples_selections_as_decorations(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (editor, cx) = cx.add_window_view(|window, cx| {
        let mut e = EditorState::new(window, cx);
        e.set_value("hello world\n", window, cx);
        e
    });
    let decorations = cx.update(|_, cx| {
        editor.update(cx, |e, cx| {
            e.create_decorations_collection(
                vec![TextDecoration::new(
                    6..11,
                    HighlightStyle {
                        background_color: Some(hsla(0.6, 0.8, 0.5, 0.3)),
                        ..Default::default()
                    },
                )],
                cx,
            )
        })
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    // Where a caret is drawn, to put another person's name over it.
    let bounds = cx.update(|_, cx| editor.read(cx).range_to_bounds(&(6..6)));
    assert!(bounds.is_some(), "an offset has a place on screen");
    let _ = decorations;
}

/// An edit's event reaches its subscribers once the update making it is
/// over, not while it runs: a flag set around the edit is clear by then,
/// so the app tells its own edits from the user's by the text each leaves.
/// Setting the whole value is not told as an edit.
#[gpui::test]
fn edits_are_told_after_the_update_making_them(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (editor, cx) = cx.add_window_view(EditorState::new);
    let applying = Rc::new(RefCell::new(false));
    let seen: Rc<RefCell<Vec<(bool, String)>>> = Rc::default();
    let (flag, log) = (applying.clone(), seen.clone());
    let _sub = cx.update(|_, cx| {
        cx.subscribe(&editor, move |_, event: &InputEditsEvent, _| {
            for (_, after) in event.edits() {
                log.borrow_mut().push((*flag.borrow(), after.to_string()));
            }
        })
    });
    cx.update(|window, cx| {
        *applying.borrow_mut() = true;
        editor.update(cx, |e, cx| {
            e.set_value("one\n", window, cx);
            e.replace_text_in_range(Some(0..0), "zero ", window, cx)
        });
        *applying.borrow_mut() = false;
    });
    cx.run_until_parked();
    assert_eq!(*seen.borrow(), [(false, "zero one\n".to_string())]);
}

/// Go to Line takes a line, or a line and a column after a colon, from 1.
#[test]
fn a_place_typed_for_go_to_line() {
    use hangar_app::docedit::line_column;
    assert_eq!(line_column("45"), Some((45, 1)));
    assert_eq!(line_column(" 45 : 9 "), Some((45, 9)));
    assert_eq!(line_column("0"), None);
    assert_eq!(line_column("12:0"), None);
    assert_eq!(line_column("twelve"), None);
    assert_eq!(line_column(""), None);
}

/// A hover provider that has something to say about every word.
struct Says;

impl gpui_component::input::HoverProvider for Says {
    fn hover(
        &self,
        _: &gpui_component::input::Rope,
        _: usize,
        _: &mut gpui::Window,
        _: &mut gpui::App,
    ) -> gpui::Task<anyhow::Result<Option<lsp_types::Hover>>> {
        gpui::Task::ready(Ok(Some(lsp_types::Hover {
            contents: lsp_types::HoverContents::Scalar(lsp_types::MarkedString::String(
                "i64".into(),
            )),
            range: None,
        })))
    }
}

struct Narrow {
    state: gpui::Entity<EditorState>,
}

impl gpui::Render for Narrow {
    fn render(
        &mut self,
        _: &mut gpui::Window,
        _: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        use gpui::{ParentElement, Styled};
        gpui::div()
            .w(gpui::px(240.))
            .h(gpui::px(300.))
            .child(hangar_app::docedit::code_editor(&self.state))
    }
}

/// The pointer resting on a symbol opens its hover, and the text keeps its
/// width: soft-wrapped lines stay where they were.
#[gpui::test]
fn a_hover_leaves_the_text_its_width(cx: &mut TestAppContext) {
    use gpui::AppContext;
    cx.update(gpui_component::init);
    let mut state = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let s = cx.new(|cx| {
            let mut e = EditorState::new(window, cx)
                .language("rust")
                .line_number(true);
            e.lsp_mut().hover_provider = Some(Rc::new(Says));
            e
        });
        state = Some(s.clone());
        gpui_component::Root::new(cx.new(|_| Narrow { state: s }), window, cx)
    });
    let state = state.unwrap();
    cx.update(|window, cx| {
        state.update(cx, |e, cx| {
            e.set_value(
                "fn record(&mut self, amount: i64) -> i64 { amount }",
                window,
                cx,
            )
        })
    });
    cx.run_until_parked();
    let width = |cx: &mut gpui::VisualTestContext| {
        cx.update(|_, cx| state.read(cx).input_bounds().size.width)
    };
    let before = width(cx);
    let text = cx.update(|_, cx| state.read(cx).text_bounds()).unwrap();
    let on = gpui::point(text.left() + gpui::px(60.), text.top() + gpui::px(6.));
    cx.simulate_mouse_move(on, None, gpui::Modifiers::none());
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(400));
    cx.run_until_parked();
    cx.simulate_mouse_move(on, None, gpui::Modifiers::none());
    cx.run_until_parked();
    assert!(
        cx.update(|_, cx| state.read(cx).hover_popover().is_some()),
        "the hover is open"
    );
    assert_eq!(width(cx), before, "the text's width");
}

struct InDocEditor {
    state: gpui::Entity<EditorState>,
    asked: Rc<std::cell::Cell<usize>>,
}

impl gpui::Render for InDocEditor {
    fn render(
        &mut self,
        _: &mut gpui::Window,
        _: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        use gpui::{InteractiveElement, ParentElement, Styled};
        let asked = self.asked.clone();
        gpui::div()
            .key_context("DocEditor")
            .size_full()
            .on_action(move |_: &hangar_app::docedit::ContextActions, _, _| {
                asked.set(asked.get() + 1)
            })
            .child(hangar_app::docedit::code_editor(&self.state))
    }
}

/// ⌥↩ in a file's editor asks for its context actions, as IntelliJ's Show
/// Context Actions does, and so does the editor's own ⌘. ; neither types
/// anything.
#[gpui::test]
fn alt_enter_asks_for_context_actions(cx: &mut TestAppContext) {
    use gpui::{AppContext, Focusable};
    cx.update(|cx| {
        gpui_component::init(cx);
        hangar_app::docedit::bind_keys(cx);
    });
    let asked = Rc::new(std::cell::Cell::new(0));
    let mut state = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let s = cx.new(|cx| EditorState::new(window, cx).language("rust"));
        state = Some(s.clone());
        let view = InDocEditor {
            state: s,
            asked: asked.clone(),
        };
        gpui_component::Root::new(cx.new(|_| view), window, cx)
    });
    let state = state.unwrap();
    cx.update(|window, cx| {
        state.update(cx, |e, cx| e.set_value("struct Ledger {}", window, cx));
        window.focus(&state.focus_handle(cx), cx);
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("alt-enter");
    cx.run_until_parked();
    assert_eq!(asked.get(), 1, "⌥↩");
    let dot = if cfg!(target_os = "macos") {
        "cmd-."
    } else {
        "ctrl-."
    };
    cx.simulate_keystrokes(dot);
    cx.run_until_parked();
    assert_eq!(asked.get(), 2);
    assert_eq!(
        cx.update(|_, cx| state.read(cx).value().to_string()),
        "struct Ledger {}",
        "nothing typed"
    );
}
