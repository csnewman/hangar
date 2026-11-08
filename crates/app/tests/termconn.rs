//! A terminal session's connection against a server speaking the client
//! API's terminal socket: the attach's reply, output and size in, keys and
//! sizes out, and the shell's exit.

use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use hangar_app::api::Client;
use hangar_app::termconn::{self, Command, Event};
use tungstenite::Message;

#[test]
// tungstenite's handshake callback returns its own large error type.
#[allow(clippy::result_large_err)]
fn a_session_attaches_talks_and_exits() {
    let ln = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", ln.local_addr().unwrap());
    let (got_tx, got_rx) = mpsc::channel();
    thread::spawn(move || {
        let (tcp, _) = ln.accept().unwrap();
        let mut path = String::new();
        let mut auth = String::new();
        let mut ws = tungstenite::accept_hdr(
            tcp,
            |req: &tungstenite::handshake::server::Request, resp| {
                path = req.uri().to_string();
                auth = req.headers()["authorization"].to_str().unwrap().to_string();
                Ok(resp)
            },
        )
        .unwrap();
        got_tx.send(format!("{path} {auth}")).unwrap();
        ws.send(Message::text(
            r#"{"session":{"id":"s1","title":"zsh","created":"2026-11-07T10:00:00Z","clients":1,"cols":80,"rows":24},"history":6}"#,
        ))
        .unwrap();
        ws.send(Message::binary(b"oold\r\nnew".to_vec())).unwrap();
        ws.send(Message::binary(vec![b's', 0, 100, 0, 30, 0, 1, 1]))
            .unwrap();
        // What the client sends: its keys and its size.
        for _ in 0..2 {
            if let Message::Binary(b) = ws.read().unwrap() {
                got_tx.send(format!("{:?}", b.to_vec())).unwrap();
            }
        }
        ws.send(Message::binary(vec![b'x', 0, 0, 0, 3])).unwrap();
        let _ = ws.read();
    });

    let (tx, rx) = mpsc::channel();
    let conn = termconn::attach(
        &Client::new(&url, "hgr_t"),
        "env1",
        Some("s1"),
        120,
        40,
        move |e| tx.send(e).is_ok(),
    );
    let next = || rx.recv_timeout(Duration::from_secs(5)).expect("an event");
    assert_eq!(
        got_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        "/api/v1/environments/env1/terminal?cols=120&rows=40&session=s1 Bearer hgr_t"
    );
    assert_eq!(
        next(),
        Event::Attached {
            session: "s1".into(),
            title: "zsh".into(),
            history: 6
        }
    );
    assert_eq!(next(), Event::Output(b"old\r\nnew".to_vec()));
    assert_eq!(
        next(),
        Event::Size {
            cols: 100,
            rows: 30,
            owner: true
        }
    );
    conn.send(Command::Input(b"ls\r".to_vec()));
    conn.send(Command::Resize(90, 20));
    assert_eq!(
        got_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        format!("{:?}", b"ils\r".to_vec())
    );
    assert_eq!(
        got_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        format!("{:?}", vec![b'r', 0, 90, 0, 20])
    );
    assert_eq!(next(), Event::Exited(3));
}

/// Answers one HTTP request on ln with status and an error body, held open
/// a moment: closing a socket with unread input resets it, losing what was
/// written.
fn refuse(ln: &TcpListener, status: &str, error: &str) {
    use std::io::{Read, Write};
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
    let body = format!(r#"{{"error":"{error}"}}"#);
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    tcp.write_all(resp.as_bytes()).unwrap();
    thread::sleep(Duration::from_millis(500));
}

#[test]
fn a_refused_attach_says_why() {
    let ln = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", ln.local_addr().unwrap());
    thread::spawn(move || refuse(&ln, "404 Not Found", "no such session"));
    let (tx, rx) = mpsc::channel();
    let _conn = termconn::attach(
        &Client::new(&url, "hgr_t"),
        "env1",
        Some("gone"),
        80,
        24,
        move |e| tx.send(e).is_ok(),
    );
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        Event::Failed("no such session".into())
    );
}

/// An environment not running may be resuming: its terminal keeps trying.
#[test]
fn an_environment_not_running_is_tried_again() {
    let ln = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", ln.local_addr().unwrap());
    thread::spawn(move || refuse(&ln, "409 Conflict", "the environment is not running"));
    let (tx, rx) = mpsc::channel();
    let _conn = termconn::attach(
        &Client::new(&url, "hgr_t"),
        "env1",
        None,
        80,
        24,
        move |e| tx.send(e).is_ok(),
    );
    assert!(matches!(
        rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        Event::Reconnecting { why, .. } if why == "the environment is not running"
    ));
}

/// A connection lost after attaching is dialled again, to the same
/// session, which replays its past.
#[test]
// tungstenite's handshake callback returns its own large error type.
#[allow(clippy::result_large_err)]
fn a_lost_connection_attaches_again_to_its_session() {
    let ln = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", ln.local_addr().unwrap());
    let (paths_tx, paths_rx) = mpsc::channel();
    thread::spawn(move || {
        for _ in 0..2 {
            let (tcp, _) = ln.accept().unwrap();
            let mut path = String::new();
            let mut ws = tungstenite::accept_hdr(
                tcp,
                |req: &tungstenite::handshake::server::Request, resp| {
                    path = req.uri().to_string();
                    Ok(resp)
                },
            )
            .unwrap();
            paths_tx.send(path).unwrap();
            ws.send(Message::text(
                r#"{"session":{"id":"s1","title":"zsh","created":"2026-11-07T10:00:00Z","clients":1,"cols":80,"rows":24},"history":0}"#,
            ))
            .unwrap();
            ws.send(Message::binary(b"oprompt$ ".to_vec())).unwrap();
            // Dropped without a close, as a network that went away.
            drop(ws);
        }
        thread::sleep(Duration::from_secs(2));
    });
    let (tx, rx) = mpsc::channel();
    let _conn = termconn::attach(
        &Client::new(&url, "hgr_t"),
        "env1",
        None,
        80,
        24,
        move |e| tx.send(e).is_ok(),
    );
    let next = || rx.recv_timeout(Duration::from_secs(5)).expect("an event");
    let path = || paths_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(path(), "/api/v1/environments/env1/terminal?cols=80&rows=24");
    assert!(matches!(next(), Event::Attached { session, .. } if session == "s1"));
    assert_eq!(next(), Event::Output(b"prompt$ ".to_vec()));
    assert!(matches!(next(), Event::Reconnecting { .. }));
    assert_eq!(
        path(),
        "/api/v1/environments/env1/terminal?cols=80&rows=24&session=s1"
    );
    assert!(matches!(next(), Event::Attached { session, .. } if session == "s1"));
}
