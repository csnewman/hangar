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
    // takes keys. It fails on the connection's own thread, which wakes the
    // view when it hears of it: soon or, on Windows, seconds later.
    cx.executor().allow_parking();
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

/// A session the test speaks for: what it sends goes to the terminal, and
/// every frame the terminal sends comes back.
fn shared(
    hello: &'static str,
) -> (
    String,
    std::sync::mpsc::Sender<Vec<u8>>,
    std::sync::mpsc::Receiver<Vec<u8>>,
) {
    use tungstenite::Message;
    let ln = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", ln.local_addr().unwrap());
    let (to_tx, to) = std::sync::mpsc::channel::<Vec<u8>>();
    let (from_tx, from) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (tcp, _) = ln.accept().unwrap();
        let mut ws = tungstenite::accept(tcp).unwrap();
        // Reads time out, so what the test sends goes out between them; the
        // handshake waits for the terminal's request however long it takes.
        ws.get_ref()
            .set_read_timeout(Some(std::time::Duration::from_millis(10)))
            .unwrap();
        ws.send(Message::text(hello)).unwrap();
        loop {
            while let Ok(b) = to.try_recv() {
                ws.send(Message::binary(b)).unwrap();
            }
            match ws.read() {
                Ok(Message::Binary(b)) => {
                    let _ = from_tx.send(b.to_vec());
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => return,
            }
        }
    });
    (url, to_tx, from)
}

/// While another window holds the size, the terminal is drawn at it and
/// says so, with Fit to This Window; typing takes the size too, as the
/// web's terminal does. Only the window holding it answers a program.
#[gpui::test]
fn the_size_another_window_holds_is_said(cx: &mut TestAppContext) {
    let (url, to, from) = shared(
        r#"{"session":{"id":"s1","title":"zsh","created":"2026-11-07T10:00:00Z","clients":2,"cols":100,"rows":30},"history":0,"owner":false}"#,
    );
    let (term, cx) = attached(cx, &url);
    let wait = std::time::Duration::from_secs(5);
    let until =
        |cx: &mut VisualTestContext, what: &str, done: &dyn Fn(&mut VisualTestContext) -> bool| {
            let start = std::time::Instant::now();
            while !done(cx) {
                assert!(start.elapsed() < wait, "{what}");
                cx.run_until_parked();
                cx.update(|window, _| window.refresh());
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };
    to.send(vec![b's', 0, 100, 0, 30, 0, 2, 0]).unwrap();
    until(cx, "the bar says so", &|cx| {
        cx.debug_bounds("term-fit").is_some()
    });

    // The window has said the size it would have, and taken nothing.
    std::thread::sleep(std::time::Duration::from_millis(100));
    let mut own = None;
    while let Ok(f) = from.try_recv() {
        assert_eq!(f[0], b'p', "only the size it would have: {f:?}");
        own = Some(f[1..5].to_vec());
    }
    let own = own.expect("its size");
    assert_ne!(own, vec![0, 100, 0, 30]);

    // A program's question is the holder's to answer.
    to.send(b"o\x1b[6n".to_vec()).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    cx.run_until_parked();
    assert!(from.try_recv().is_err(), "no answer from this window");

    // Fit to This Window takes the size.
    let fit = cx.debug_bounds("term-fit").unwrap().center();
    cx.simulate_click(fit, gpui::Modifiers::none());
    let mut claim = vec![b'r'];
    claim.extend_from_slice(&own);
    assert_eq!(from.recv_timeout(wait).unwrap(), claim);
    let mut held = vec![b's'];
    held.extend_from_slice(&own);
    held.extend_from_slice(&[0, 2, 1]);
    to.send(held).unwrap();
    until(cx, "the offer goes", &|cx| {
        cx.debug_bounds("term-fit").is_none()
    });
    assert!(
        cx.debug_bounds("term-sizing").is_some(),
        "the two windows watching are still said"
    );
    assert!(term.read_with(cx, |t, _| t.sizing().unwrap().owner));

    // Another window takes it; typing here takes it back.
    to.send(vec![b's', 0, 100, 0, 30, 0, 2, 0]).unwrap();
    until(cx, "the offer is back", &|cx| {
        cx.debug_bounds("term-fit").is_some()
    });
    cx.simulate_keystrokes("a");
    assert_eq!(from.recv_timeout(wait).unwrap(), claim, "the size first");
    assert_eq!(from.recv_timeout(wait).unwrap(), b"ia", "then the key");
}

#[gpui::test]
fn a_command_is_typed_once_a_new_session_starts(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(gpui_component::init);
    let (url, typed) = recording(b"$ ");
    let client = Client::new(&url, "hgr_t");
    let (_, cx) = cx.add_window_view(|window, cx| {
        let t = cx.new(|cx| {
            let mut t = TerminalView::new(&client, "env1", None, cx);
            t.run_once_started("docker exec -it web sh");
            t
        });
        Root::new(t, window, cx)
    });
    let start = std::time::Instant::now();
    let got = loop {
        cx.run_until_parked();
        if let Ok(b) = typed.recv_timeout(std::time::Duration::from_millis(50)) {
            break b;
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "typed"
        );
    };
    assert_eq!(got, b"docker exec -it web sh\r");
}

/// Once the shell exits the terminal says the session has ended and offers
/// another, as the web terminal does; Enter starts it, a new session.
#[gpui::test]
#[allow(clippy::result_large_err)]
fn another_session_starts_once_the_shell_exits(cx: &mut TestAppContext) {
    use tungstenite::Message;
    let ln = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", ln.local_addr().unwrap());
    let (paths_tx, paths) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for (n, tcp) in ln.incoming().enumerate() {
            let paths_tx = paths_tx.clone();
            std::thread::spawn(move || {
                let mut path = String::new();
                let mut ws = tungstenite::accept_hdr(
                    tcp.unwrap(),
                    |req: &tungstenite::handshake::server::Request, resp| {
                        path = req.uri().to_string();
                        Ok(resp)
                    },
                )
                .unwrap();
                paths_tx.send(path).unwrap();
                let id = format!("s{}", n + 1);
                ws.send(Message::text(format!(
                    r#"{{"session":{{"id":"{id}","title":"zsh","created":"2026-11-07T10:00:00Z","clients":1,"cols":80,"rows":24}},"history":0}}"#
                )))
                .unwrap();
                ws.send(Message::binary(b"o$ ".to_vec())).unwrap();
                // The first shell exits at once.
                if n == 0 {
                    ws.send(Message::binary(vec![b'x', 0, 0, 0, 0])).unwrap();
                }
                while ws.read().is_ok() {}
            });
        }
    });
    let (term, cx) = attached_any(cx, &url);
    let wait = std::time::Duration::from_secs(10);
    assert!(!paths.recv_timeout(wait).unwrap().contains("session="));
    let start = std::time::Instant::now();
    while !matches!(
        term.read_with(cx, |t, _| t.state().clone()),
        hangar_app::termview::State::Exited(_)
    ) {
        assert!(start.elapsed() < wait, "the shell exits");
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("start-another").is_some(),
        "another offered"
    );
    cx.simulate_keystrokes("enter");
    let second = paths.recv_timeout(wait).unwrap();
    assert!(!second.contains("session="), "a new session: {second}");
    let start = std::time::Instant::now();
    while term.read_with(cx, |t, _| t.session().map(str::to_string)) != Some("s2".into()) {
        assert!(start.elapsed() < wait, "attached to it");
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// A terminal in a window on a server at url, focused, in whatever state
/// it reaches.
fn attached_any<'a>(
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
    cx.update(|window, cx| window.focus(&term.focus_handle(cx), cx));
    (term, cx)
}
