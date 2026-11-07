//! A server place: its event stream read, the connection kept against a
//! server (a small one here, speaking just enough HTTP), and what is known
//! of it kept on disk.

use std::io::{BufRead, BufReader, Cursor, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use hangar_app::api::{Client, Event, Events};
use hangar_app::place::{self, Connection, PlaceState, Update};

fn env_json(id: &str, name: &str, phase: &str) -> String {
    format!(
        r#"{{"id":"{id}","short_id":"abc123","name":"{name}","owner":"alice","template":"t","phase":"{phase}","desired":"running","created_at":"2026-11-07T10:00:00Z","updated_at":"2026-11-07T10:00:00Z","task":{{"id":"t1","text":"Fix rounding","set_by":"agent","set_at":"2026-11-07T10:00:00Z"}},"agents":[{{"id":"s1","agent":"claude","state":"asking","message":"May I?","since":"2026-11-07T10:01:00Z","needs_you":true}}],"later_field":{{"x":1}}}}"#
    )
}

#[test]
fn the_stream_is_read_as_events() {
    let stream = format!(
        ": ping\n\nevent: environment\ndata: {}\n\nevent: novel\ndata: {{}}\n\nevent: removed\r\ndata: {{\"id\":\"gone\"}}\r\n\r\nevent: synced\ndata: {{}}\n\n",
        env_json("e1", "dev", "running")
    );
    let events: Vec<Event> = Events::new(Cursor::new(stream))
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(events.len(), 3, "{events:?}");
    let Event::Environment(e) = &events[0] else {
        panic!("{:?}", events[0])
    };
    assert_eq!(e.task.as_ref().unwrap().text, "Fix rounding");
    assert!(e.agents[0].needs_you);
    assert_eq!(events[1], Event::Removed("gone".into()));
    assert_eq!(events[2], Event::Synced);
}

/// A server for one test: answers each request with what reply says, and
/// keeps an event stream open until the test ends.
fn serve(reply: impl Fn(&str, &str, &mut TcpStream) + Send + Sync + 'static) -> String {
    let ln = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = ln.local_addr().unwrap();
    let reply = std::sync::Arc::new(reply);
    thread::spawn(move || {
        for conn in ln.incoming() {
            let Ok(mut conn) = conn else { return };
            let reply = reply.clone();
            thread::spawn(move || {
                let mut r = BufReader::new(conn.try_clone().unwrap());
                loop {
                    let mut first = String::new();
                    if r.read_line(&mut first).unwrap_or(0) == 0 {
                        return;
                    }
                    let mut auth = String::new();
                    loop {
                        let mut h = String::new();
                        r.read_line(&mut h).unwrap();
                        if h.trim().is_empty() {
                            break;
                        }
                        if let Some(v) = h
                            .strip_prefix("Authorization: ")
                            .or(h.strip_prefix("authorization: "))
                        {
                            auth = v.trim().to_string();
                        }
                    }
                    let path = first.split(' ').nth(1).unwrap_or("").to_string();
                    reply(&path, &auth, &mut conn);
                }
            });
        }
    });
    format!("http://{addr}")
}

fn json(conn: &mut TcpStream, status: &str, body: &str) {
    write!(
        conn,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
}

#[test]
fn a_place_connects_and_follows_changes() {
    let url = serve(|path, auth, conn| {
        if auth != "Bearer hgr_good" && path != "/api/v1/version" {
            return json(conn, "401 Unauthorized", r#"{"error":"not signed in"}"#);
        }
        match path {
            "/api/v1/version" => json(conn, "200 OK", r#"{"versions":["v1"],"server":"test"}"#),
            "/api/v1/me" => json(
                conn,
                "200 OK",
                r#"{"id":"u1","username":"alice","display_name":"Alice","admin":false}"#,
            ),
            "/api/v1/events" => {
                write!(conn, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n").unwrap();
                write!(
                    conn,
                    "event: environment\ndata: {}\n\nevent: environment\ndata: {}\n\nevent: synced\ndata: {{}}\n\n",
                    env_json("e1", "dev", "running"),
                    env_json("e2", "docs", "stopped")
                )
                .unwrap();
                thread::sleep(Duration::from_millis(200));
                write!(conn, "event: removed\ndata: {{\"id\":\"e2\"}}\n\n").unwrap();
                write!(
                    conn,
                    "event: environment\ndata: {}\n\n",
                    env_json("e1", "dev", "suspended")
                )
                .unwrap();
                thread::sleep(Duration::from_secs(10));
            }
            _ => json(conn, "404 Not Found", r#"{"error":"no"}"#),
        }
    });

    let (tx, rx) = mpsc::channel();
    let _c = place::connect(Client::new(&url, "hgr_good"), move |u| tx.send(u).is_ok());
    let mut state = PlaceState::default();
    let mut seen = Vec::new();
    while !(state.environments.len() == 1 && state.environments[0].phase == "suspended") {
        let u = rx.recv_timeout(Duration::from_secs(5)).expect("an update");
        seen.push(format!("{u:?}").chars().take(40).collect::<String>());
        state.apply(u);
    }
    assert_eq!(state.connection, Connection::Online);
    assert_eq!(state.me.as_ref().unwrap().username, "alice");
    assert!(seen[0].starts_with("Connection(Connecting"), "{seen:?}");
    assert!(seen.iter().any(|s| s.starts_with("Snapshot")), "{seen:?}");

    // What is known is kept, and read back without its connection.
    let dir = tempdir();
    state.save(&dir, "server/one").unwrap();
    let back = PlaceState::load(&dir, "server/one").unwrap();
    assert_eq!(back.environments, state.environments);
    assert_eq!(back.connection, Connection::Connecting);

    // A token the server refuses asks for a sign-in, and stops trying.
    let (tx, rx) = mpsc::channel();
    let _c = place::connect(Client::new(&url, "hgr_bad"), move |u| tx.send(u).is_ok());
    let mut last = None;
    while let Ok(u) = rx.recv_timeout(Duration::from_secs(2)) {
        last = Some(u);
    }
    assert_eq!(last, Some(Update::Connection(Connection::SignIn)));
}

#[test]
fn an_unreachable_place_is_offline_and_retried() {
    // A port nothing listens on.
    let url = {
        let ln = TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", ln.local_addr().unwrap())
    };
    let (tx, rx) = mpsc::channel();
    let _c = place::connect(Client::new(&url, "hgr_x"), move |u| tx.send(u).is_ok());
    let mut offline = 0;
    let mut connecting = 0;
    while let Ok(u) = rx.recv_timeout(Duration::from_secs(3)) {
        match u {
            Update::Connection(Connection::Offline { .. }) => offline += 1,
            Update::Connection(Connection::Connecting) => connecting += 1,
            u => panic!("{u:?}"),
        }
        if offline == 2 {
            break;
        }
    }
    assert_eq!((offline, connecting), (2, 2));
}

fn tempdir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("hangar-app-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
