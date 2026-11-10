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

/// A program that asks for Kitty's escape codes is sent Shift-Enter as
/// itself, so it can tell it from Enter.
#[gpui::test]
fn shift_enter_reaches_a_program_that_asks(cx: &mut TestAppContext) {
    use tungstenite::Message;
    cx.executor().allow_parking();
    cx.update(gpui_component::init);
    cx.update(hangar_app::termview::bind_keys);
    let ln = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", ln.local_addr().unwrap());
    let (typed_tx, typed) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (tcp, _) = ln.accept().unwrap();
        let mut ws = tungstenite::accept(tcp).unwrap();
        ws.send(Message::text(
            r#"{"session":{"id":"s1","title":"agent","created":"2026-11-07T10:00:00Z","clients":1,"cols":80,"rows":24},"history":0}"#,
        ))
        .unwrap();
        ws.send(Message::binary(b"o\x1b[>1u> ".to_vec())).unwrap();
        while let Ok(m) = ws.read() {
            if let Message::Binary(b) = m
                && b.first() == Some(&b'i')
            {
                typed_tx.send(b[1..].to_vec()).unwrap();
            }
        }
    });
    let client = Client::new(&url, "hgr_t");
    let mut term = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let t = cx.new(|cx| TerminalView::new(&client, "env1", None, cx));
        term = Some(t.clone());
        Root::new(t, window, cx)
    });
    let term = term.unwrap();
    let start = std::time::Instant::now();
    while term.read_with(cx, |t, _| t.state() != &hangar_app::termview::State::Live) {
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "attached"
        );
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let focus = term.read_with(cx, |t, cx| t.focus_handle(cx));
    cx.update(|window, cx| window.focus(&focus, cx));
    cx.run_until_parked();
    // The program's ask arrives with its output, before the keys.
    std::thread::sleep(std::time::Duration::from_millis(100));
    cx.run_until_parked();
    cx.simulate_keystrokes("shift-enter");
    cx.simulate_keystrokes("enter");
    let first = typed
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    let second = typed
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    assert_eq!(first, b"\x1b[13;2u");
    assert_eq!(second, b"\r");
}

/// A session that shows output, then hands on what it is sent.
fn recording(output: &'static [u8]) -> (String, std::sync::mpsc::Receiver<Vec<u8>>) {
    use tungstenite::Message;
    let ln = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", ln.local_addr().unwrap());
    let (typed_tx, typed) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (tcp, _) = ln.accept().unwrap();
        let mut ws = tungstenite::accept(tcp).unwrap();
        ws.send(Message::text(
            r#"{"session":{"id":"s1","title":"zsh","created":"2026-11-07T10:00:00Z","clients":1,"cols":80,"rows":24},"history":0}"#,
        ))
        .unwrap();
        let mut o = b"o".to_vec();
        o.extend_from_slice(output);
        ws.send(Message::binary(o)).unwrap();
        while let Ok(m) = ws.read() {
            if let Message::Binary(b) = m
                && b.first() == Some(&b'i')
            {
                typed_tx.send(b[1..].to_vec()).unwrap();
            }
        }
    });
    (url, typed)
}

/// A terminal in a window, attached to a session at url, focused.
fn attached<'a>(
    cx: &'a mut TestAppContext,
    url: &str,
) -> (Entity<TerminalView>, &'a mut VisualTestContext) {
    cx.executor().allow_parking();
    cx.update(gpui_component::init);
    cx.update(hangar_app::termview::bind_keys);
    let client = Client::new(url, "hgr_t");
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
    // The output arrives after the session's hello.
    std::thread::sleep(std::time::Duration::from_millis(100));
    cx.run_until_parked();
    let focus = term.read_with(cx, |t, cx| t.focus_handle(cx));
    cx.update(|window, cx| window.focus(&focus, cx));
    cx.run_until_parked();
    (term, cx)
}

/// Right-clicks the terminal's second line, with modifiers, then presses
/// keys: in its menu, if it opens.
fn right_click(cx: &mut VisualTestContext, modifiers: gpui::Modifiers, keys: &str) {
    let at = cx.debug_bounds("terminal").unwrap().origin
        + gpui::point(gpui::px(14.), gpui::px(4. + 17. + 8.));
    cx.simulate_mouse_down(at, gpui::MouseButton::Right, modifiers);
    cx.simulate_mouse_up(at, gpui::MouseButton::Right, modifiers);
    cx.run_until_parked();
    cx.simulate_keystrokes(keys);
    cx.run_until_parked();
}

fn clipboard(cx: &mut VisualTestContext) -> Option<String> {
    cx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()))
}

/// The terminal's menu, as IntelliJ's: Copy (only with something
/// selected), Paste, Select All, Find, Clear Buffer, then the session's.
#[gpui::test]
fn the_menu_selects_copies_pastes_and_clears(cx: &mut TestAppContext) {
    let (url, typed) = recording(b"cargo build --release\r\nerror: it broke\r\n$ ");
    let (_, cx) = attached(cx, &url);
    let none = gpui::Modifiers::none();

    // With nothing selected, Copy is passed over: Paste, then Select All.
    right_click(cx, none, "down down enter");
    right_click(cx, none, "down enter");
    assert_eq!(
        clipboard(cx).as_deref(),
        Some("cargo build --release\nerror: it broke\n$"),
        "Select All, then Copy"
    );

    cx.update(|_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("ls\n".into())));
    right_click(cx, none, "down down enter");
    let pasted = typed
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    assert_eq!(
        pasted, b"ls\r",
        "Paste sends the text, its line end as Enter"
    );

    // Copy, Paste, Select All, Find, Clear Buffer.
    right_click(cx, none, "down down down down down enter");
    right_click(cx, none, "down down enter");
    right_click(cx, none, "down enter");
    assert_eq!(
        clipboard(cx).as_deref(),
        Some("$"),
        "Clear Buffer leaves the prompt"
    );

    // Select All's and Copy's keys.
    let (all, copy) = if cfg!(target_os = "macos") {
        ("cmd-a", "cmd-c")
    } else {
        ("ctrl-shift-a", "ctrl-shift-c")
    };
    cx.update(|_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(String::new())));
    cx.simulate_keystrokes(all);
    cx.simulate_keystrokes(copy);
    assert_eq!(clipboard(cx).as_deref(), Some("$"));
}

/// A program that asked for the mouse is told of a right-click, and no
/// menu opens; with Shift, the menu opens all the same.
#[gpui::test]
fn a_program_that_asked_for_the_mouse_gets_the_right_click(cx: &mut TestAppContext) {
    let (url, typed) = recording(b"\x1b[?1000h\x1b[?1006h$ ");
    let (_, cx) = attached(cx, &url);
    let next = || {
        typed
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
    };
    cx.update(|_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("pasted".into())));

    right_click(cx, gpui::Modifiers::none(), "down enter");
    assert_eq!(next(), b"\x1b[<2;1;2M", "pressed at column 1, row 2");
    assert_eq!(next(), b"\x1b[<2;1;2m", "and released");
    assert_eq!(next(), b"\x1b[B", "no menu took the keys");
    assert_eq!(next(), b"\r");

    right_click(cx, gpui::Modifiers::shift(), "down enter");
    assert_eq!(next(), b"pasted", "Shift gives the menu, and its Paste");
}
