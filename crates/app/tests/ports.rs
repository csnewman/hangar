//! The Ports tool window: what the environment listens on, each with its
//! program and its address from here, the system's own left out; its web
//! servers by name; and who may reach them, said and changed.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use chrono::Utc;
use gpui::{AppContext, Modifiers, TestAppContext, VisualTestContext};
use gpui_component::Root;
use hangar_app::api::{Environment, Me};
use hangar_app::place::Update;
use hangar_app::places::Places;
use hangar_app::ports::{Port, PortsView, host_url, shown};
use hangar_app::store::MemoryTokens;
use hangar_app::workspace::Workspace;
use serde_json::{Value, json};
use tungstenite::Message;

fn listening() -> Value {
    json!([
        {"port": 53, "loopback": true},
        {"port": 5173, "loopback": true, "pid": 41, "command": "/usr/bin/node /home/dev/app/node_modules/.bin/vite"},
        {"port": 8090, "loopback": false},
    ])
}

#[test]
fn a_port_says_its_program_and_its_address() {
    let ports: Vec<Port> = serde_json::from_value(listening()).unwrap();
    let list = shown(&ports);
    assert_eq!(
        list.iter().map(|p| p.port).collect::<Vec<_>>(),
        [5173, 8090],
        "the resolver's 53, the system's own, left out"
    );
    assert_eq!(
        list[0].program().as_deref(),
        Some("node /home/dev/app/node_modules/.bin/vite")
    );
    assert_eq!(list[1].program(), None, "a process not the user's");
    assert_eq!(
        host_url(
            "http://hangar.localhost:8080",
            Some("5173"),
            "abc123",
            ".hangar.localhost:8080"
        ),
        "http://5173-envabc123.hangar.localhost:8080/"
    );
    assert_eq!(
        host_url(
            "https://hangar.example.com",
            None,
            "abc123",
            ".hangar.example.com"
        ),
        "https://envabc123.hangar.example.com/"
    );
}

fn environment(public: bool) -> Environment {
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
        ports_public: public,
        display: None,
        image_update: None,
        web_names: vec!["docs".into()],
        task: None,
        agents: vec![],
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

/// A server whose Code service listens on the three ports, and which says
/// each change of who may reach them.
fn server() -> (String, Receiver<Value>) {
    let ln = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", ln.local_addr().unwrap());
    let (tx, asked) = channel();
    std::thread::spawn(move || {
        for tcp in ln.incoming() {
            let Ok(tcp) = tcp else { return };
            let tx = tx.clone();
            std::thread::spawn(move || serve(tcp, tx));
        }
    });
    (url, asked)
}

fn serve(tcp: TcpStream, asked: Sender<Value>) {
    let mut head = [0u8; 2048];
    let n = tcp.peek(&mut head).unwrap_or(0);
    if String::from_utf8_lossy(&head[..n])
        .to_ascii_lowercase()
        .contains("upgrade: websocket")
    {
        let Ok(mut ws) = tungstenite::accept(tcp) else {
            return;
        };
        while let Ok(Message::Binary(b)) = ws.read() {
            let req: Value = serde_json::from_slice(&b[1..]).unwrap();
            let result = match req["method"].as_str().unwrap() {
                "hello" => json!({ "root": req["params"]["root"], "home": "/home/dev" }),
                "workspace.get" => json!({ "folders": ["/home/dev"] }),
                "ports.list" => listening(),
                _ => json!(null),
            };
            let mut frame = vec![b'r'];
            frame
                .extend(serde_json::to_vec(&json!({ "id": req["id"], "result": result })).unwrap());
            if ws.send(Message::Binary(frame.into())).is_err() {
                return;
            }
        }
        return;
    }
    let mut r = BufReader::new(tcp.try_clone().unwrap());
    let mut first = String::new();
    r.read_line(&mut first).unwrap();
    let mut length = 0;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
            break;
        }
        if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
            length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    r.read_exact(&mut body).unwrap();
    let (status, reply) = if first.starts_with("PUT /api/v1/environments/e1/ports ") {
        let asked_for: Value = serde_json::from_slice(&body).unwrap();
        let public = asked_for["public"].as_bool().unwrap();
        asked.send(asked_for).unwrap();
        (
            "200 OK",
            serde_json::to_string(&environment(public)).unwrap(),
        )
    } else {
        ("404 Not Found", "{}".to_string())
    };
    let mut tcp = tcp;
    let _ = write!(
        tcp,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
        reply.len()
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
fn the_ports_are_listed_and_made_public(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(gpui_component::init);
    let (url, asked) = server();
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let id = places.update(cx, |p, cx| {
        let id = p.add_server(&url, "hgr_t", cx).unwrap();
        p.apply(&id, Update::Snapshot(vec![environment(false)]), cx);
        p.apply(
            &id,
            Update::Me(Me {
                id: "u1".into(),
                username: "alice".into(),
                display_name: String::new(),
                admin: false,
                ssh: None,
                environment_host_suffix: Some(".hangar.test".into()),
            }),
            cx,
        );
        id
    });
    let mut view = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let ws = cx.new(|cx| Workspace::new(places.clone(), &id, "e1", "/home/dev", cx));
        let v = cx.new(|cx| PortsView::new(ws, places.clone(), &id, "e1", cx));
        view = Some(v.clone());
        Root::new(v, window, cx)
    });
    let view = view.unwrap();
    cx.simulate_resize(gpui::size(gpui::px(320.), gpui::px(700.)));

    until(cx, "the ports", |cx| cx.debug_bounds("port-5173").is_some());
    assert!(cx.debug_bounds("port-53").is_none(), "the system's own");
    assert_eq!(
        view.read_with(cx, |v, _| v.summary()),
        Some("2 listening".into())
    );
    assert!(cx.debug_bounds("web-default").is_some());
    assert!(cx.debug_bounds("web-docs").is_some(), "the template's name");

    // Copy URL.
    let copy = cx.debug_bounds("port-5173-copy").unwrap().center();
    cx.simulate_click(copy, Modifiers::none());
    let copied = cx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()));
    let scheme = url.split("://").next().unwrap();
    assert_eq!(
        copied,
        Some(format!("{scheme}://5173-envabc123.hangar.test/"))
    );

    // Public, asked of the server.
    let public = cx.debug_bounds("ports-public").unwrap().center();
    cx.simulate_click(public, Modifiers::none());
    let start = Instant::now();
    let got = loop {
        cx.run_until_parked();
        if let Ok(v) = asked.recv_timeout(Duration::from_millis(20)) {
            break v;
        }
        assert!(start.elapsed() < Duration::from_secs(5), "asked");
    };
    assert_eq!(got, json!({ "public": true }));
}

#[test]
fn a_port_a_running_container_publishes_is_said_to_be_its() {
    use hangar_app::docker::Container;
    let c = |name: &str, state: &str, ports: &[&str]| Container {
        name: name.into(),
        state: state.into(),
        ports: ports.iter().map(|p| p.to_string()).collect(),
        ..Default::default()
    };
    let found = hangar_app::ports::publishers(&[
        c("db", "running", &["5432→5432/tcp"]),
        c("web", "running", &["8080→80/tcp", "9000/tcp"]),
        c("old", "exited", &["3000→3000/tcp"]),
    ]);
    assert_eq!(found.get(&5432).map(String::as_str), Some("db (5432/tcp)"));
    assert_eq!(found.get(&8080).map(String::as_str), Some("web (80/tcp)"));
    assert_eq!(found.len(), 2);
}

#[test]
fn the_toolbar_chips_are_the_ports_its_network_reaches() {
    use hangar_app::ports::{Port, chips};
    let p = |port: u16, loopback: bool, pid: Option<u32>| Port {
        port,
        loopback,
        pid,
        command: None,
    };
    let ports = [
        p(8080, false, Some(1)),
        p(5173, false, Some(2)),
        p(5173, false, Some(2)),
        p(53, true, None),
        p(6060, true, Some(3)),
    ];
    assert_eq!(chips(&ports), [5173, 8080]);
}
