//! What gpui-component's editor does with what the Code tab's shared
//! documents ask of it, which the app's editor builds on: an edit from
//! elsewhere puts the caret after it and enters the one undo history, so the
//! app keeps the selection itself (mapped through the edit) and its own
//! history (as hangar-proto change sets, taking Undo and Redo before the
//! editor does); local and applied edits are told alike; a large file; and
//! other people's selections as decorations, their carets placed by
//! range_to_bounds.

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
