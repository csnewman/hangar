//! IntelliJ's line commands in a file's editor, from their keys: each is
//! one change, put in the editor and sent to the shared document.

use std::net::TcpListener;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use gpui::{AppContext, TestAppContext, VisualTestContext};
use hangar_app::api::Environment;
use hangar_app::place::Update;
use hangar_app::places::Places;
use hangar_app::shell::Shell;
use hangar_app::store::MemoryTokens;
use serde_json::{Value, json};
use tungstenite::Message;

const TEXT: &str = "fn a() {\n    one();\n    two();\n}\n";

/// A Code service with one project holding main.rs; what is pushed to the
/// document is sent to pushed.
fn service(pushed: mpsc::Sender<Value>) -> String {
    let ln = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", ln.local_addr().unwrap());
    thread::spawn(move || {
        for tcp in ln.incoming() {
            let pushed = pushed.clone();
            thread::spawn(move || {
                // The app's plain requests to the server are not this
                // service's.
                let Ok(mut ws) = tungstenite::accept(tcp.unwrap()) else {
                    return;
                };
                while let Ok(Message::Binary(b)) = ws.read() {
                    let req: Value = serde_json::from_slice(&b[1..]).unwrap();
                    let (method, params) = (req["method"].as_str().unwrap(), &req["params"]);
                    let result = match method {
                        "hello" => json!({ "root": params["root"], "home": "/home/dev" }),
                        "workspace.get" => json!({ "folders": ["/home/dev/app"] }),
                        "doc.open" => json!({
                            "doc": 1, "text": TEXT, "epoch": "e1", "version": 0,
                            "writable": true, "client": "c1", "path": params["path"],
                        }),
                        "doc.push" => {
                            pushed.send(params.clone()).unwrap();
                            Value::Null
                        }
                        _ => json!([]),
                    };
                    let reply = json!({ "id": req["id"], "result": result });
                    let mut frame = vec![b'r'];
                    frame.extend(serde_json::to_vec(&reply).unwrap());
                    if ws.send(Message::Binary(frame.into())).is_err() {
                        return;
                    }
                }
            });
        }
    });
    url
}

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
        display: None,
        image_update: None,
        web_names: vec![],
        task: None,
        agents: vec![],
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

fn until(cx: &mut VisualTestContext, what: &str, ready: impl Fn(&mut VisualTestContext) -> bool) {
    let start = Instant::now();
    while !ready(cx) {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "waiting for {what}"
        );
        cx.run_until_parked();
        thread::sleep(Duration::from_millis(10));
    }
}

#[gpui::test]
fn line_commands_change_the_document(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(|cx| {
        gpui_component::init(cx);
        hangar_app::envtab::bind_keys(cx);
        hangar_app::docedit::bind_keys(cx);
    });
    let (pushed_tx, pushed) = mpsc::channel();
    let url = service(pushed_tx);
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let id = places.update(cx, |p, cx| {
        let id = p.add_server(&url, "hgr_t", cx).unwrap();
        p.apply(&id, Update::Snapshot(vec![running()]), cx);
        id
    });
    let (shell, cx) = cx.add_window_view(|window, cx| Shell::new(places.clone(), window, cx));
    cx.update(|window, cx| shell.update(cx, |s, cx| s.open(&id, "e1", window, cx)));
    let tab = shell.read_with(cx, |s, _| s.active_view()).unwrap();
    cx.update(|window, cx| {
        tab.update(cx, |t, cx| {
            t.open_file("/home/dev/app", "main.rs", window, cx)
        })
    });
    let doc = tab.read_with(cx, |t, _| t.active_file()).unwrap();
    let text =
        |cx: &mut VisualTestContext| doc.read_with(cx, |d, cx| d.text_and_caret(cx).0.to_string());
    until(cx, "the file open", |cx| text(cx) == TEXT);
    // The caret on "one();".
    cx.update(|window, cx| doc.update(cx, |d, cx| d.go_to(1, 6, window, cx)));
    cx.run_until_parked();
    let mac = cfg!(target_os = "macos");
    let key = |m: &str, k: &str| {
        if mac {
            format!("cmd-{k}")
        } else {
            format!("{m}-{k}")
        }
    };

    cx.simulate_keystrokes(&key("ctrl", "d"));
    assert_eq!(
        text(cx),
        "fn a() {\n    one();\n    one();\n    two();\n}\n",
        "duplicated"
    );
    let push = pushed.recv_timeout(Duration::from_secs(5)).expect("sent");
    assert_eq!(push["doc"], 1);

    // The caret is on the copy, which goes; it is then on "two();".
    cx.simulate_keystrokes(if mac { "cmd-backspace" } else { "ctrl-y" });
    assert_eq!(text(cx), TEXT, "the copy deleted");

    cx.simulate_keystrokes("alt-shift-up");
    assert_eq!(
        text(cx),
        "fn a() {\n    two();\n    one();\n}\n",
        "moved up"
    );

    cx.simulate_keystrokes(&key("ctrl", "/"));
    assert_eq!(
        text(cx),
        "fn a() {\n    // two();\n    one();\n}\n",
        "commented"
    );

    // Undo takes each back, one at a time.
    cx.simulate_keystrokes(&key("ctrl", "z"));
    assert_eq!(text(cx), "fn a() {\n    two();\n    one();\n}\n");
}
