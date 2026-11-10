//! The terminal in a window: Tab is the shell's, not the window's way to
//! move between fields; the mouse selects and Cmd-C copies; Find looks
//! through the output.

use gpui::{
    AppContext, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement, Render,
    TestAppContext, VisualTestContext, Window, div,
};
use gpui_component::Root;
use gpui_component::input::{Textarea, TextareaState};
use hangar_app::api::Client;
use hangar_app::termview::TerminalView;

/// A terminal with a text field after it, for Tab to move focus to.
struct Both {
    term: Entity<TerminalView>,
    field: Entity<TextareaState>,
}

impl Render for Both {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .child(self.term.clone())
            .child(Textarea::new(&self.field))
    }
}

fn open(cx: &mut TestAppContext) -> (FocusHandle, FocusHandle, &mut VisualTestContext) {
    cx.update(gpui_component::init);
    cx.update(hangar_app::termview::bind_keys);
    // Nothing answers there: the terminal fails to connect, and still
    // takes keys.
    let client = Client::new("http://127.0.0.1:9", "hgr_t");
    let (root, cx) = cx.add_window_view(|window, cx| {
        let term = cx.new(|cx| TerminalView::new(&client, "env1", None, cx));
        let field = cx.new(|cx| TextareaState::new(window, cx));
        let both = cx.new(|_| Both { term, field });
        Root::new(both, window, cx)
    });
    let both: Entity<Both> = root.read_with(cx, |r, _| r.view().clone().downcast().unwrap());
    let (term, field) = both.read_with(cx, |b, cx| {
        (b.term.focus_handle(cx), b.field.focus_handle(cx))
    });
    cx.update(|window, cx| window.focus(&term, cx));
    cx.run_until_parked();
    (term, field, cx)
}

#[gpui::test]
fn tab_stays_in_the_terminal(cx: &mut TestAppContext) {
    let (term, field, cx) = open(cx);
    cx.simulate_keystrokes("tab");
    cx.update(|window, cx| {
        assert!(
            term.is_focused(window),
            "Tab moved focus out of the terminal"
        );
        assert!(!field.contains_focused(window, cx));
    });
    cx.simulate_keystrokes("shift-tab");
    cx.update(|window, _| assert!(term.is_focused(window), "Shift-Tab moved focus"));
}

/// A session that shows a build's output, then reads what it is sent.
fn session() -> String {
    use tungstenite::Message;
    let ln = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", ln.local_addr().unwrap());
    std::thread::spawn(move || {
        let (tcp, _) = ln.accept().unwrap();
        let mut ws = tungstenite::accept(tcp).unwrap();
        ws.send(Message::text(
            r#"{"session":{"id":"s1","title":"zsh","created":"2026-11-07T10:00:00Z","clients":1,"cols":80,"rows":24},"history":0}"#,
        ))
        .unwrap();
        ws.send(Message::binary(
            b"ocargo build --release\r\nerror: it broke\r\n$ ".to_vec(),
        ))
        .unwrap();
        while ws.read().is_ok() {}
    });
    url
}

#[gpui::test]
fn a_word_is_selected_copied_and_found(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(gpui_component::init);
    cx.update(hangar_app::termview::bind_keys);
    let client = Client::new(&session(), "hgr_t");
    let mut term = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let t = cx.new(|cx| TerminalView::new(&client, "env1", None, cx));
        term = Some(t.clone());
        Root::new(t, window, cx)
    });
    let term = term.unwrap();
    cx.simulate_resize(gpui::size(gpui::px(800.), gpui::px(400.)));
    let start = std::time::Instant::now();
    while term.read_with(cx, |t, _| t.state() != &hangar_app::termview::State::Live) {
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "attached"
        );
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    cx.run_until_parked();

    // A double click on the second line's first word, "error", then
    // Cmd-C: the pointer's row and column are the cell's.
    let at = cx.debug_bounds("terminal").unwrap().origin
        + gpui::point(gpui::px(14.), gpui::px(4. + 17. + 8.));
    for count in [1, 2] {
        cx.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: at,
            modifiers: gpui::Modifiers::none(),
            click_count: count,
            first_mouse: false,
        });
        cx.simulate_event(gpui::MouseUpEvent {
            button: gpui::MouseButton::Left,
            position: at,
            modifiers: gpui::Modifiers::none(),
            click_count: count,
        });
    }
    let (copy, find) = if cfg!(target_os = "macos") {
        ("cmd-c", "cmd-f")
    } else {
        ("ctrl-shift-c", "ctrl-shift-f")
    };
    cx.simulate_keystrokes(copy);
    let copied = cx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()));
    assert_eq!(copied.as_deref(), Some("error"));

    // Find opens with the selection as its query, which is there.
    cx.simulate_keystrokes(find);
    cx.run_until_parked();
    assert_eq!(term.read_with(cx, |t, _| t.finding()), Some(true));
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-a backspace"
    } else {
        "ctrl-a backspace"
    });
    cx.run_until_parked();
    cx.simulate_input("nowhere");
    cx.run_until_parked();
    assert_eq!(term.read_with(cx, |t, _| t.finding()), Some(false));
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-a backspace"
    } else {
        "ctrl-a backspace"
    });
    cx.run_until_parked();
    cx.simulate_input("error");
    cx.run_until_parked();
    assert_eq!(term.read_with(cx, |t, _| t.finding()), Some(true));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(
        term.read_with(cx, |t, _| t.finding()),
        None,
        "Escape closes Find"
    );
}
