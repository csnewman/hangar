//! A Code service connection against a server speaking the client API's
//! Code socket: hello first, requests answered by id, events, and the
//! changes git reports made into marks.

use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use hangar_app::api::Client;
use hangar_app::code::{Code, Notice};
use hangar_app::project::marks;
use serde_json::{Value, json};
use tungstenite::Message;

fn frame(v: Value) -> Message {
    let mut b = vec![b'r'];
    b.extend_from_slice(v.to_string().as_bytes());
    Message::binary(b)
}

#[test]
fn requests_are_answered_and_events_told() {
    let ln = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", ln.local_addr().unwrap());
    let (path_tx, path_rx) = mpsc::channel();
    thread::spawn(move || {
        let (tcp, _) = ln.accept().unwrap();
        #[allow(clippy::result_large_err)]
        let mut ws = tungstenite::accept_hdr(
            tcp,
            |req: &tungstenite::handshake::server::Request, resp| {
                path_tx.send(req.uri().to_string()).unwrap();
                Ok(resp)
            },
        )
        .unwrap();
        loop {
            let Ok(Message::Binary(b)) = ws.read() else {
                return;
            };
            let req: Value = serde_json::from_slice(&b[1..]).unwrap();
            let reply = match req["method"].as_str().unwrap() {
                "hello" => {
                    json!({ "id": req["id"], "result": { "root": req["params"]["root"], "home": "/home/dev" } })
                }
                "fs.list" => {
                    ws.send(frame(
                        json!({ "event": "fs.changed", "params": { "dirs": ["/home/dev/app"] } }),
                    ))
                    .unwrap();
                    json!({ "id": req["id"], "result": [{ "name": "src", "dir": true, "size": 0 }] })
                }
                _ => json!({ "id": req["id"], "error": "no such method" }),
            };
            ws.send(frame(reply)).unwrap();
        }
    });

    let (tx, rx) = mpsc::channel();
    let code = Code::connect(
        &Client::new(&url, "hgr_t"),
        "env1",
        "/home/dev/app",
        move |n| tx.send(n).is_ok(),
    );
    assert_eq!(
        path_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        "/api/v1/environments/env1/code?root=/home/dev/app"
    );
    let Notice::Ready(hello) = rx.recv_timeout(Duration::from_secs(5)).unwrap() else {
        panic!("not ready")
    };
    assert_eq!(hello["root"], "/home/dev/app");

    let listed = futures_block(code.call("fs.list", json!({ "path": "" })));
    assert_eq!(listed.unwrap()[0]["name"], "src");
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        Notice::Event("fs.changed".into(), json!({ "dirs": ["/home/dev/app"] }))
    );
    assert_eq!(
        futures_block(code.call("nope", json!({}))),
        Err("no such method".into())
    );
}

/// Waits for a future on this thread.
fn futures_block<T>(f: impl std::future::Future<Output = T>) -> T {
    pollster::block_on(f)
}

#[test]
fn git_changes_become_marks() {
    let m = marks(&json!({ "changes": [
        { "path": "src/a.rs", "index": ".", "worktree": "M" },
        { "path": "src/b.rs", "index": "?", "worktree": "?" },
        { "path": "src/c.rs", "index": "A", "worktree": "." },
        { "path": "src/d.rs", "index": ".", "worktree": "D" },
        { "path": "src/e.rs", "index": "U", "worktree": "U" },
    ]}));
    assert_eq!(m["src/a.rs"], 'M');
    assert_eq!(m["src/b.rs"], 'A');
    assert_eq!(m["src/c.rs"], 'A');
    assert_eq!(m["src/d.rs"], 'D');
    assert_eq!(m["src/e.rs"], '!');
}
