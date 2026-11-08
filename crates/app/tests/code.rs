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

/// A connection lost is dialled again and the service greeted again; what
/// is asked while it is lost fails at once rather than waiting.
#[test]
fn a_lost_connection_is_made_again() {
    let ln = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", ln.local_addr().unwrap());
    let (drop_tx, drop_rx) = mpsc::channel::<()>();
    thread::spawn(move || {
        for n in 0..2 {
            let (tcp, _) = ln.accept().unwrap();
            let mut ws = tungstenite::accept(tcp).unwrap();
            let Ok(Message::Binary(b)) = ws.read() else {
                return;
            };
            let req: Value = serde_json::from_slice(&b[1..]).unwrap();
            ws.send(frame(
                json!({ "id": req["id"], "result": { "root": "/r", "n": n } }),
            ))
            .unwrap();
            if n == 0 {
                // Dropped without a close, as a network that went away.
                drop_rx.recv().unwrap();
                drop(ws);
            } else {
                thread::sleep(Duration::from_secs(2));
            }
        }
    });
    let (tx, rx) = mpsc::channel();
    let code = Code::connect(&Client::new(&url, "hgr_t"), "env1", "/r", move |n| {
        tx.send(n).is_ok()
    });
    let next = || rx.recv_timeout(Duration::from_secs(5)).expect("a notice");
    assert!(matches!(next(), Notice::Ready(h) if h["n"] == 0));
    drop_tx.send(()).unwrap();
    assert!(matches!(next(), Notice::Lost { .. }));
    assert!(
        futures_block(code.call("fs.list", json!({}))).is_err(),
        "a call while lost fails"
    );
    assert!(matches!(next(), Notice::Ready(h) if h["n"] == 1));
}

/// A server that refuses the token is not dialled again.
#[test]
fn a_refused_connection_ends() {
    use std::io::{Read, Write};
    let ln = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", ln.local_addr().unwrap());
    thread::spawn(move || {
        let (mut tcp, _) = ln.accept().unwrap();
        let mut req = Vec::new();
        let mut buf = [0; 1024];
        while !req.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = tcp.read(&mut buf).unwrap();
            if n == 0 {
                return;
            }
            req.extend_from_slice(&buf[..n]);
        }
        let body = r#"{"error":"the token is not valid"}"#;
        let resp = format!(
            "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        tcp.write_all(resp.as_bytes()).unwrap();
        thread::sleep(Duration::from_millis(500));
    });
    let (tx, rx) = mpsc::channel();
    let _code = Code::connect(&Client::new(&url, "hgr_bad"), "env1", "/r", move |n| {
        tx.send(n).is_ok()
    });
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        Notice::Closed("the token is not valid".into())
    );
}
