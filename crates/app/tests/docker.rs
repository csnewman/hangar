//! The Docker tool window, as IntelliJ's Services window: containers by
//! Compose project and the images; the chosen container's log, followed,
//! or its details; and what can be done to each, from the bar and the
//! menus.

use std::collections::HashSet;
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::{Duration, Instant};

use gpui::{AppContext, Modifiers, MouseButton, TestAppContext, VisualTestContext};
use gpui_component::{Root, WindowExt};
use hangar_app::docker::{
    Chosen, Container, DockerView, Network, Overview, Row, RunInTerminal, Sections, Volume, bytes,
    published, rows, run_command, shell_command,
};
use hangar_app::logview::{Lines, strip_ansi};
use hangar_app::model::Tone;
use hangar_app::places::Places;
use hangar_app::store::MemoryTokens;
use hangar_app::workspace::Workspace;
use serde_json::{Value, json};
use tungstenite::Message;

fn overview() -> Value {
    let compose = |id: &str, service: &str, state: &str, status: &str, ports: Value| {
        json!({
            "id": id, "name": format!("shop-{service}-1"), "image": format!("shop-{service}"),
            "state": state, "status": status, "ports": ports, "created": 1.7e9,
            "command": "run", "project": "shop", "service": service,
            "workingDir": "/home/dev/shop", "configFiles": ["/home/dev/shop/compose.yaml"],
        })
    };
    json!({
        "available": true,
        "containers": [
            compose("aaaaaaaaaaaa", "web", "running", "Up 3 minutes", json!(["8080→80/tcp"])),
            compose("bbbbbbbbbbbb", "worker", "exited", "Exited (1) 2 minutes ago", json!([])),
            {"id": "cccccccccccc", "name": "scratch", "image": "alpine", "state": "running",
             "status": "Up 1 hour", "ports": [], "created": 1.7e9, "command": "sh"},
        ],
        "images": [
            {"id": "sha256:1111111111111111", "tags": ["alpine:3"], "size": 8.0e6, "created": 1.7e9, "containers": 1},
            {"id": "sha256:2222222222222222", "tags": [], "size": 1.2e9, "created": 1.7e9, "containers": 0},
        ],
    })
}

#[test]
fn projects_then_lone_containers_then_images() {
    let o: Overview = serde_json::from_value(overview()).unwrap();
    assert_eq!(o.summary(), "2 of 3 running");
    let r = rows(&o, &HashSet::new(), Sections::default());
    assert_eq!(
        r,
        [
            Row::Project {
                name: "shop".into(),
                up: 1,
                count: 2,
                open: true
            },
            Row::Container {
                id: "aaaaaaaaaaaa".into(),
                nested: true
            },
            Row::Container {
                id: "bbbbbbbbbbbb".into(),
                nested: true
            },
            Row::Container {
                id: "cccccccccccc".into(),
                nested: false
            },
            Row::Images {
                count: 2,
                size: 1.208e9,
                open: false
            },
        ]
    );
    let closed = HashSet::from(["shop".to_string()]);
    let r = rows(
        &o,
        &closed,
        Sections {
            images: true,
            ..Default::default()
        },
    );
    assert_eq!(
        r.len(),
        5,
        "the project alone, scratch, the images and both"
    );
    assert!(matches!(r[0], Row::Project { open: false, .. }));
    assert_eq!(
        r[3..],
        [
            Row::Image {
                id: "sha256:1111111111111111".into()
            },
            Row::Image {
                id: "sha256:2222222222222222".into()
            }
        ],
        "tagged by tag; untagged by ID"
    );

    let unavailable = Overview {
        available: false,
        why: Some("Docker is not installed".into()),
        ..Default::default()
    };
    assert!(
        rows(
            &unavailable,
            &HashSet::new(),
            Sections {
                images: true,
                ..Default::default()
            }
        )
        .is_empty()
    );
}

#[test]
fn a_container_says_how_it_is() {
    let o: Overview = serde_json::from_value(overview()).unwrap();
    let web = o.container("aaaaaaaaaaaa").unwrap();
    assert_eq!(web.label(true), "web", "under its project, its service");
    assert_eq!(web.label(false), "shop-web-1");
    assert_eq!(web.meta(), ":8080", "running: its published ports");
    assert_eq!(web.tone(), Tone::Good);
    let worker = o.container("bbbbbbbbbbbb").unwrap();
    assert_eq!(worker.meta(), "exited (1)");
    assert_eq!(worker.tone(), Tone::Bad, "a fault");
    let clean = Container {
        state: "exited".into(),
        status: "Exited (0) 1 hour ago".into(),
        ..Default::default()
    };
    assert_eq!(clean.tone(), Tone::Idle, "it finished");
    let scratch = o.container("cccccccccccc").unwrap();
    assert_eq!(scratch.meta(), "alpine", "nothing published: its image");

    assert_eq!(bytes(1.208e9), "1.2 GB");
    assert_eq!(bytes(512.), "512 B");
    assert_eq!(bytes(250e6), "250 MB");
    assert_eq!(
        shell_command("shop-web-1"),
        "docker exec -it shop-web-1 sh -c 'command -v bash >/dev/null && exec bash || exec sh'"
    );
}

#[test]
fn a_log_is_kept_as_lines_without_escapes() {
    assert_eq!(
        strip_ansi("\x1b[1;32mok\x1b[0m \x1b]0;title\x07done"),
        "ok done"
    );
    let mut l = Lines::default();
    l.push("starting\r\nloading 10%\rloading 100%\nhal");
    assert_eq!(l.len(), 3, "the line being written shows");
    l.push("f\x1b[0m done\n");
    assert_eq!(
        l.text(),
        "starting\nloading 100%\nhalf done\n",
        "a carriage return redraws; an escape split across pieces is taken out"
    );
    for i in 0..hangar_app::logview::MAX_LINES {
        l.push(&format!("{i}\n"));
    }
    assert_eq!(l.len(), hangar_app::logview::MAX_LINES, "the oldest go");
    assert_eq!(l.line(0), "0");
}

/// A Code service holding the overview's Docker, which says each docker
/// method asked of it, with its parameters. A log's first output comes
/// before docker.logs answers, as it can.
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
                let mut streams = 6;
                loop {
                    let Ok(Message::Binary(b)) = ws.read() else {
                        return;
                    };
                    let req: Value = serde_json::from_slice(&b[1..]).unwrap();
                    let (method, params) = (req["method"].as_str().unwrap(), &req["params"]);
                    if method.starts_with("docker.") {
                        let _ = tx.send((method.to_string(), params.clone()));
                    }
                    let result = match method {
                        "hello" => json!({ "root": params["root"], "home": "/home/dev" }),
                        "workspace.get" => json!({ "folders": ["/home/dev/shop"] }),
                        "docker.overview" => overview(),
                        "docker.logs" => {
                            streams += 1;
                            let id = params["id"].as_str().unwrap_or("");
                            let early = json!({ "event": "docker.log", "params": { "stream": streams, "text": format!("{id} says\nhello ") } });
                            if !send(&mut ws, early) {
                                return;
                            }
                            json!({ "stream": streams })
                        }
                        _ => json!(null),
                    };
                    if !send(&mut ws, json!({ "id": req["id"], "result": result })) {
                        return;
                    }
                    if method == "docker.logs" {
                        let later = json!({ "event": "docker.log", "params": { "stream": streams, "text": "world\n" } });
                        if !send(&mut ws, later) {
                            return;
                        }
                    }
                }
            });
        }
    });
    (url, asked)
}

fn send(ws: &mut tungstenite::WebSocket<std::net::TcpStream>, v: Value) -> bool {
    let mut frame = vec![b'r'];
    frame.extend(serde_json::to_vec(&v).unwrap());
    ws.send(Message::Binary(frame.into())).is_ok()
}

fn click(cx: &mut VisualTestContext, at: &'static str) {
    until(cx, at, |cx| cx.debug_bounds(at).is_some());
    let c = cx.debug_bounds(at).unwrap().center();
    cx.simulate_click(c, Modifiers::none());
    cx.run_until_parked();
}

fn until(cx: &mut VisualTestContext, what: &str, ready: impl Fn(&mut VisualTestContext) -> bool) {
    let start = Instant::now();
    while !ready(cx) {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "waiting for {what}"
        );
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        thread::sleep(Duration::from_millis(10));
    }
}

#[gpui::test]
fn containers_are_followed_and_acted_on(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(gpui_component::init);
    let (url, asked) = service();
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let id = places.update(cx, |p, cx| p.add_server(&url, "hgr_t", cx).unwrap());
    let mut docker = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let ws = cx.new(|cx| Workspace::new(places.clone(), &id, "e1", "/home/dev", cx));
        let d = cx.new(|cx| DockerView::new(ws, cx));
        docker = Some(d.clone());
        Root::new(d, window, cx)
    });
    cx.simulate_resize(gpui::size(gpui::px(1000.), gpui::px(500.)));
    let docker = docker.unwrap();
    let runs = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    let seen = runs.clone();
    cx.update(|_, cx| {
        cx.subscribe(&docker, move |_, e: &RunInTerminal, _| {
            seen.borrow_mut().push(e.0.clone())
        })
        .detach()
    });
    let next = |want: &str| loop {
        let (method, params) = asked.recv_timeout(Duration::from_secs(5)).unwrap();
        if method == want {
            return params;
        }
    };
    until(cx, "the containers", |cx| {
        cx.debug_bounds("docker-row-shop-web-1").is_some()
    });
    assert_eq!(
        docker.read_with(cx, |d, _| d.summary()),
        Some("2 of 3 running".into())
    );

    // Choosing a container follows its log; what came before docker.logs
    // answered is kept.
    click(cx, "docker-row-shop-web-1");
    assert_eq!(next("docker.logs")["id"], "aaaaaaaaaaaa");
    until(cx, "the log", |cx| {
        docker.read_with(cx, |d, cx| d.log(cx).read(cx).lines().text())
            == "aaaaaaaaaaaa says\nhello world\n"
    });

    // Another container stops following the first.
    click(cx, "docker-row-shop-worker-1");
    assert_eq!(next("docker.logsStop")["stream"], 7);
    assert_eq!(next("docker.logs")["id"], "bbbbbbbbbbbb");
    until(cx, "its log alone", |cx| {
        docker.read_with(cx, |d, cx| d.log(cx).read(cx).lines().text())
            == "bbbbbbbbbbbb says\nhello world\n"
    });

    // The bar: an exited container starts; it has no shell.
    click(cx, "docker-start");
    let p = next("docker.act");
    assert_eq!(
        (p["id"].as_str(), p["action"].as_str()),
        (Some("bbbbbbbbbbbb"), Some("start"))
    );
    click(cx, "docker-shell");
    assert!(runs.borrow().is_empty(), "not while it is stopped");

    // A running one: Restart and Shell.
    click(cx, "docker-row-shop-web-1");
    click(cx, "docker-restart");
    let p = next("docker.act");
    assert_eq!(
        (p["id"].as_str(), p["action"].as_str()),
        (Some("aaaaaaaaaaaa"), Some("restart"))
    );
    click(cx, "docker-shell");
    assert_eq!(*runs.borrow(), [shell_command("shop-web-1")]);

    // Inspect stops following and shows the details.
    click(cx, "docker-inspect-tab");
    next("docker.logsStop");
    until(cx, "the details", |cx| {
        cx.debug_bounds("docker-copy-id").is_some()
    });

    // The menu: Pause, the seventh item, after Show Log, Open Log in
    // Editor, Open Shell, Open :8080 in Browser, Stop and Restart.
    let row = cx.debug_bounds("docker-row-shop-web-1").unwrap().center();
    cx.simulate_mouse_down(row, MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(row, MouseButton::Right, Modifiers::none());
    cx.run_until_parked();
    cx.simulate_keystrokes("down down down down down down down enter");
    let p = next("docker.act");
    assert_eq!(p["action"], "pause");

    // A project: its details, and Down once asked.
    click(cx, "docker-project-shop");
    assert_eq!(
        docker.read_with(cx, |d, _| d.chosen().cloned()),
        Some(Chosen::Project("shop".into()))
    );
    click(cx, "docker-down");
    assert!(
        cx.update(|window, cx| window.has_active_dialog(cx)),
        "Down asks first"
    );
    click(cx, "dialog-ok");
    let p = next("docker.project");
    assert_eq!(
        (p["project"].as_str(), p["action"].as_str()),
        (Some("shop"), Some("down"))
    );

    // Keys move through the rows, and close the project.
    cx.simulate_keystrokes("down");
    assert_eq!(
        docker.read_with(cx, |d, _| d.chosen().cloned()),
        Some(Chosen::Container("aaaaaaaaaaaa".into()))
    );
    cx.simulate_keystrokes("up left");
    until(cx, "the project closed", |cx| {
        cx.debug_bounds("docker-row-shop-web-1").is_none()
    });

    // The images open; one in use cannot be removed.
    click(cx, "docker-images");
    click(cx, "docker-image-alpine:3");
    click(cx, "docker-remove-image");
    assert!(
        !cx.update(|window, cx| window.has_active_dialog(cx)),
        "alpine is in use"
    );
}

#[test]
fn volumes_and_networks_have_sections_of_their_own() {
    let o = Overview {
        available: true,
        volumes: vec![Volume {
            name: "shop_data".into(),
            containers: vec!["web-1".into()],
            ..Default::default()
        }],
        networks: vec![Network {
            id: "c".repeat(64),
            name: "bridge".into(),
            builtin: true,
            ..Default::default()
        }],
        ..Default::default()
    };
    let shut = rows(&o, &HashSet::new(), Sections::default());
    assert_eq!(
        shut,
        [
            Row::Images {
                count: 0,
                size: 0.,
                open: false
            },
            Row::Volumes {
                count: 1,
                open: false
            },
            Row::Networks {
                count: 1,
                open: false
            },
        ]
    );
    assert!(shut.iter().all(Row::heading));
    let open = rows(
        &o,
        &HashSet::new(),
        Sections {
            volumes: true,
            networks: true,
            ..Default::default()
        },
    );
    assert_eq!(
        open[2..],
        [
            Row::Volume {
                name: "shop_data".into()
            },
            Row::Networks {
                count: 1,
                open: true
            },
            Row::Network { id: "c".repeat(64) },
        ]
    );
    // None of either: no heading for it.
    let bare = Overview {
        available: true,
        ..Default::default()
    };
    assert_eq!(rows(&bare, &HashSet::new(), Sections::default()).len(), 1);
}

#[test]
fn a_containers_published_ports_are_its_host_ports() {
    let ports = [
        "8080→80/tcp".to_string(),
        "127.0.0.1:5432→5432/tcp".to_string(),
        "6379/tcp".to_string(),
        "8080→80/tcp".to_string(),
    ];
    assert_eq!(published(&ports), [5432, 8080]);
    assert_eq!(
        run_command("nginx:latest"),
        "docker run --rm -it nginx:latest"
    );
}
