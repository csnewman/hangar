//! The Terminal window's sessions: a tab's menu ends its session, asking
//! first, and Close Tab in a terminal closes its tab, leaving the session
//! running.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use chrono::Utc;
use gpui::{AppContext, Modifiers, MouseButton, TestAppContext, VisualTestContext};
use gpui_component::WindowExt;
use hangar_app::api::Environment;
use hangar_app::place::Update;
use hangar_app::places::Places;
use hangar_app::shell::Shell;
use hangar_app::store::MemoryTokens;
use hangar_app::tools::Tool;

fn running() -> Environment {
    Environment {
        id: "e1".into(),
        short_id: "abc123".into(),
        name: "ledger-1".into(),
        owner: "alice".into(),
        template: "t".into(),
        phase: "running".into(),
        desired: "running".into(),
        reason: None,
        progress: None,
        worker: None,
        worker_online: Some(true),
        cpus: None,
        memory_mib: None,
        editor_path: Some("/home/dev".into()),
        ports_public: false,
        web_names: vec![],
        task: None,
        agents: vec![],
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

/// A server with two sessions, s1 and s2, which attaches to either and
/// says which it was asked to end.
fn server() -> (String, Receiver<String>) {
    let ln = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", ln.local_addr().unwrap());
    let (ended_tx, ended) = channel();
    std::thread::spawn(move || {
        for tcp in ln.incoming() {
            let Ok(tcp) = tcp else { return };
            let ended = ended_tx.clone();
            std::thread::spawn(move || serve(tcp, ended));
        }
    });
    (url, ended)
}

// tungstenite's handshake callback answers with a whole response.
#[allow(clippy::result_large_err)]
fn serve(tcp: TcpStream, ended: Sender<String>) {
    let mut head = [0u8; 2048];
    let n = tcp.peek(&mut head).unwrap_or(0);
    let head = String::from_utf8_lossy(&head[..n]).to_ascii_lowercase();
    if head.contains("upgrade: websocket") {
        let mut session = String::from("s1");
        let Ok(mut ws) = tungstenite::accept_hdr(
            tcp,
            |req: &tungstenite::handshake::server::Request, resp| {
                if let Some(q) = req.uri().query()
                    && let Some(s) = q.split('&').find_map(|kv| kv.strip_prefix("session="))
                {
                    session = s.to_string();
                }
                Ok(resp)
            },
        ) else {
            return;
        };
        let hello = format!(
            r#"{{"session":{{"id":"{session}","title":"{session}","created":"2026-11-07T10:00:00Z","clients":1,"cols":80,"rows":24}},"history":0}}"#
        );
        let _ = ws.send(tungstenite::Message::text(hello));
        while ws.read().is_ok() {}
        return;
    }
    let mut r = BufReader::new(tcp.try_clone().unwrap());
    let mut first = String::new();
    r.read_line(&mut first).unwrap();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
            break;
        }
    }
    let mut parts = first.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let (status, body) = match (method, path) {
        ("GET", "/api/v1/environments/e1/terminals") => (
            "200 OK",
            r#"[{"id":"s1","title":"s1","created_at":"2026-11-07T10:00:00Z"},{"id":"s2","title":"s2","created_at":"2026-11-07T10:01:00Z"}]"#,
        ),
        ("DELETE", p) if p.starts_with("/api/v1/environments/e1/terminals/") => {
            ended.send(p.to_string()).unwrap();
            ("204 No Content", "")
        }
        _ => ("404 Not Found", "{}"),
    };
    let mut tcp = tcp;
    let _ = write!(
        tcp,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

fn until(cx: &mut VisualTestContext, what: &str, done: impl Fn(&mut VisualTestContext) -> bool) {
    let start = Instant::now();
    while !done(cx) {
        assert!(start.elapsed() < Duration::from_secs(10), "{what}");
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[gpui::test]
fn a_session_is_ended_once_asked_and_a_tab_closed_leaves_it(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(|cx| {
        gpui_component::init(cx);
        hangar_app::envtab::bind_keys(cx);
        hangar_app::termview::bind_keys(cx);
    });
    let (url, ended) = server();
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let id = places.update(cx, |p, cx| {
        let id = p.add_server(&url, "hgr_t", cx).unwrap();
        p.apply(&id, Update::Snapshot(vec![running()]), cx);
        id
    });
    let mut shell = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let s = cx.new(|cx| Shell::new(places.clone(), window, cx));
        shell = Some(s.clone());
        gpui_component::Root::new(s, window, cx)
    });
    let shell = shell.unwrap();
    cx.simulate_resize(gpui::size(gpui::px(1200.), gpui::px(800.)));
    cx.update(|window, cx| shell.update(cx, |s, cx| s.open(&id, "e1", window, cx)));
    let tab = shell.read_with(cx, |s, _| s.active_view()).unwrap();
    tab.update(cx, |t, cx| {
        if !t.is_open(Tool::Terminal) {
            t.toggle(Tool::Terminal, cx)
        }
    });
    until(cx, "both sessions' tabs", |cx| {
        cx.debug_bounds("term-tab-1").is_some()
    });

    // The second tab's menu: New Session, Close Tab, Close Other Tabs, End
    // Session.
    let second = cx.debug_bounds("term-tab-1").unwrap().center();
    cx.simulate_mouse_down(second, MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(second, MouseButton::Right, Modifiers::none());
    cx.run_until_parked();
    cx.simulate_keystrokes("down down down down enter");
    cx.run_until_parked();
    assert!(
        cx.update(|window, cx| window.has_active_dialog(cx)),
        "End Session asks first"
    );
    assert!(ended.try_recv().is_err(), "and ends nothing until told");
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let ok = cx.debug_bounds("dialog-ok").unwrap().center();
    cx.simulate_click(ok, Modifiers::none());
    let path = ended.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(path, "/api/v1/environments/e1/terminals/s2");
    until(cx, "its tab goes", |cx| {
        cx.debug_bounds("term-tab-1").is_none()
    });
    assert!(cx.debug_bounds("term-tab-0").is_some(), "the first stays");

    // Close Tab's key, in the terminal: its tab goes, its session stays.
    let first = cx.debug_bounds("term-tab-0").unwrap().center();
    cx.simulate_click(first, Modifiers::none());
    cx.run_until_parked();
    let close = if cfg!(target_os = "macos") {
        "cmd-w"
    } else {
        "ctrl-shift-w"
    };
    cx.simulate_keystrokes(close);
    until(cx, "the tab closes", |cx| {
        cx.debug_bounds("term-tab-0").is_none()
    });
    std::thread::sleep(Duration::from_millis(200));
    assert!(ended.try_recv().is_err(), "the session is left running");
}
