//! File Properties: a file's line separators, encoding, byte order mark
//! and permissions, each changed in place and read again.

use std::net::TcpListener;
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::{Duration, Instant};

use gpui::{AppContext, Context, IntoElement, Modifiers, Render, TestAppContext, Window, div};
use gpui_component::Root;
use hangar_app::api::Client;
use hangar_app::code::Code;
use hangar_app::props::mode_string;
use serde_json::{Value, json};
use tungstenite::Message;

#[test]
fn permissions_read_as_ls_shows_them() {
    assert_eq!(mode_string(0o100644), "rw-r--r--");
    assert_eq!(mode_string(0o755), "rwxr-xr-x");
    assert_eq!(mode_string(0o400), "r--------");
}

/// A service with one CRLF file, which says what it is asked to change.
fn service() -> (String, Receiver<(String, Value)>) {
    let ln = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", ln.local_addr().unwrap());
    let (tx, asked) = channel();
    thread::spawn(move || {
        for tcp in ln.incoming() {
            let tx = tx.clone();
            thread::spawn(move || {
                let Ok(mut ws) = tungstenite::accept(tcp.unwrap()) else {
                    return;
                };
                while let Ok(Message::Binary(b)) = ws.read() {
                    let req: Value = serde_json::from_slice(&b[1..]).unwrap();
                    let (method, params) = (req["method"].as_str().unwrap(), req["params"].clone());
                    let result = match method {
                        "hello" => json!({ "root": params["root"], "home": "/home/dev" }),
                        "fs.textProps" => json!({
                            "encoding": "UTF-8", "bom": false, "separator": "CRLF",
                            "mode": 0o100644, "size": 1200, "modified": 1.7e9,
                        }),
                        _ => json!(null),
                    };
                    if method.starts_with("fs.") && method != "fs.textProps" {
                        tx.send((method.to_string(), params)).unwrap();
                    }
                    let mut frame = vec![b'r'];
                    frame.extend(
                        serde_json::to_vec(&json!({ "id": req["id"], "result": result })).unwrap(),
                    );
                    if ws.send(Message::Binary(frame.into())).is_err() {
                        return;
                    }
                }
            });
        }
    });
    (url, asked)
}

struct Blank;

impl Render for Blank {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

#[gpui::test]
fn a_files_separators_and_permissions_are_changed(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(gpui_component::init);
    let (url, asked) = service();
    let code = Code::connect(&Client::new(&url, "hgr_t"), "e1", "/home/dev/app", |_| true);
    let (_, cx) = cx.add_window_view(|window, cx| Root::new(cx.new(|_| Blank), window, cx));
    cx.update(|window, cx| {
        hangar_app::props::show(code, "/home/dev/app/notes.txt".into(), window, cx)
    });
    let click = |cx: &mut gpui::VisualTestContext, at: &'static str| {
        let start = Instant::now();
        while cx.debug_bounds(at).is_none() {
            assert!(start.elapsed() < Duration::from_secs(5), "{at}");
            cx.run_until_parked();
            cx.update(|window, _| window.refresh());
            thread::sleep(Duration::from_millis(10));
        }
        let c = cx.debug_bounds(at).unwrap().center();
        cx.simulate_click(c, Modifiers::none());
    };
    let next = |cx: &mut gpui::VisualTestContext| {
        let start = Instant::now();
        loop {
            cx.run_until_parked();
            if let Ok(m) = asked.recv_timeout(Duration::from_millis(20)) {
                return m;
            }
            assert!(start.elapsed() < Duration::from_secs(5), "a change");
        }
    };
    click(cx, "props-sep-LF");
    let (m, p) = next(cx);
    assert_eq!(m, "fs.convert");
    assert_eq!(
        p,
        json!({ "path": "/home/dev/app/notes.txt", "separator": "LF" })
    );
    // The file is read again before another change is taken.
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(300) {
        cx.run_until_parked();
        thread::sleep(Duration::from_millis(10));
    }
    click(cx, "props-writable");
    let (m, p) = next(cx);
    assert_eq!(m, "fs.setMode");
    assert_eq!(p["writable"], false, "rw-: made read-only");
}
