//! The Project window's menu on a file: making a file beside it, through a
//! stand-in Code service, as a right-click, a name typed and Enter make it.

use std::net::TcpListener;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use gpui::{AppContext, Modifiers, MouseButton, TestAppContext, VisualTestContext};
use gpui_component::Root;
use hangar_app::places::Places;
use hangar_app::project::{OpenFile, ProjectView};
use hangar_app::store::MemoryTokens;
use hangar_app::treeops::{Target, join};
use hangar_app::workspace::Workspace;
use serde_json::{Value, json};
use tungstenite::Message;

/// A Code service with one project, /home/dev/app, holding src/ and
/// main.rs; what it is asked to change is sent to asked.
fn service(asked: mpsc::Sender<(String, Value)>) -> String {
    let ln = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", ln.local_addr().unwrap());
    thread::spawn(move || {
        for tcp in ln.incoming() {
            let asked = asked.clone();
            thread::spawn(move || {
                let mut ws = tungstenite::accept(tcp.unwrap()).unwrap();
                while let Ok(Message::Binary(b)) = ws.read() {
                    let req: Value = serde_json::from_slice(&b[1..]).unwrap();
                    let (method, params) = (req["method"].as_str().unwrap(), &req["params"]);
                    let result = match method {
                        "hello" => json!({ "root": params["root"], "home": "/home/dev" }),
                        "workspace.get" => json!({ "folders": ["/home/dev/app"] }),
                        "fs.list" if params["path"] == "" => json!([
                            { "name": "src", "dir": true, "size": 0 },
                            { "name": "main.rs", "dir": false, "size": 10 },
                        ]),
                        "fs.list" => json!([]),
                        "git.status" => json!({ "changes": [] }),
                        m if m.starts_with("fs.") => {
                            asked.send((m.to_string(), params.clone())).unwrap();
                            Value::Null
                        }
                        _ => {
                            let reply = json!({ "id": req["id"], "error": "no such method" });
                            send(&mut ws, reply);
                            continue;
                        }
                    };
                    send(&mut ws, json!({ "id": req["id"], "result": result }));
                }
            });
        }
    });
    url
}

fn send(ws: &mut tungstenite::WebSocket<std::net::TcpStream>, v: Value) {
    let mut b = vec![b'r'];
    b.extend_from_slice(v.to_string().as_bytes());
    ws.send(Message::binary(b)).unwrap();
}

/// Runs the app until ready says so, the service answering on its threads.
fn until(cx: &mut VisualTestContext, what: &str, ready: impl Fn(&mut VisualTestContext) -> bool) {
    let start = Instant::now();
    while !ready(cx) {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "waiting for {what}"
        );
        cx.run_until_parked();
        thread::sleep(Duration::from_millis(10));
    }
}

#[gpui::test]
fn new_file_from_a_files_menu_is_made_beside_it(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(gpui_component::init);
    let (asked_tx, asked) = mpsc::channel();
    let url = service(asked_tx);
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let id = places.update(cx, |p, cx| p.add_server(&url, "hgr_t", cx).unwrap());
    let (opened_tx, opened) = mpsc::channel();
    let mut tree = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let ws = cx.new(|cx| Workspace::new(places.clone(), &id, "e1", "/home/dev", cx));
        let view = cx.new(|cx| ProjectView::new(ws, cx));
        cx.subscribe(&view, move |_, _, e: &OpenFile, _| {
            opened_tx.send(e.clone()).unwrap();
        })
        .detach();
        tree = Some(view.clone());
        Root::new(view, window, cx)
    });
    let _tree = tree.unwrap();
    until(cx, "the project's files", |cx| {
        cx.debug_bounds("tree-row-main.rs").is_some()
    });

    let row = cx.debug_bounds("tree-row-main.rs").unwrap();
    cx.simulate_mouse_down(row.center(), MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(row.center(), MouseButton::Right, Modifiers::none());
    cx.run_until_parked();
    // New File… is the menu's first item.
    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();
    cx.simulate_input("notes.md");
    cx.simulate_keystrokes("enter");
    let made = std::cell::RefCell::new(None);
    until(cx, "the service asked to make a file", |_| {
        if let Ok(m) = asked.try_recv() {
            *made.borrow_mut() = Some(m);
        }
        made.borrow().is_some()
    });
    let (method, params) = made.into_inner().unwrap();
    assert_eq!(method, "fs.create");
    assert_eq!(params["path"], "/home/dev/app/notes.md", "beside main.rs");
    let open = std::cell::RefCell::new(None);
    until(cx, "the new file opened", |_| {
        if let Ok(o) = opened.try_recv() {
            *open.borrow_mut() = Some(o);
        }
        open.borrow().is_some()
    });
    let open = open.into_inner().unwrap();
    assert_eq!(
        (open.root.as_str(), open.path.as_str()),
        ("/home/dev/app", "notes.md")
    );
}

/// A context menu is modal, as a native one is: while it is up, a click on
/// another row only closes it, and the row right-clicked stays selected.
#[gpui::test]
fn a_click_beside_an_open_menu_only_closes_it(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(gpui_component::init);
    let (asked_tx, _asked) = mpsc::channel();
    let url = service(asked_tx);
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let id = places.update(cx, |p, cx| p.add_server(&url, "hgr_t", cx).unwrap());
    let mut tree = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let ws = cx.new(|cx| Workspace::new(places.clone(), &id, "e1", "/home/dev", cx));
        let view = cx.new(|cx| ProjectView::new(ws, cx));
        tree = Some(view.clone());
        let shell_like = cx.new(|cx| Shielded {
            inner: view,
            _o: cx.observe_global::<hangar_app::ui::OpenMenus>(|_, cx| cx.notify()),
        });
        Root::new(shell_like, window, cx)
    });
    let tree = tree.unwrap();
    until(cx, "the project's files", |cx| {
        cx.debug_bounds("tree-row-src").is_some()
    });
    let file = cx.debug_bounds("tree-row-main.rs").unwrap();
    cx.simulate_mouse_down(file.center(), MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(file.center(), MouseButton::Right, Modifiers::none());
    cx.run_until_parked();
    assert!(
        cx.update(|_, cx| hangar_app::ui::menu_open(cx)),
        "the menu is open"
    );
    let selected = |cx: &mut VisualTestContext| {
        tree.read_with(cx, |t, _| t.selected().map(|s| s.path.clone()))
    };
    assert_eq!(
        selected(cx).as_deref(),
        Some("main.rs"),
        "the row right-clicked"
    );
    let folder = cx.debug_bounds("tree-row-src").unwrap();
    cx.simulate_click(folder.center(), Modifiers::none());
    cx.run_until_parked();
    assert!(
        !cx.update(|_, cx| hangar_app::ui::menu_open(cx)),
        "the menu closed"
    );
    assert_eq!(
        selected(cx).as_deref(),
        Some("main.rs"),
        "the click selected nothing"
    );
}

/// The window's root as the shell draws it, for the shield.
struct Shielded {
    inner: gpui::Entity<ProjectView>,
    _o: gpui::Subscription,
}

impl gpui::Render for Shielded {
    fn render(
        &mut self,
        _: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        use gpui::{ParentElement, Styled};
        gpui::div()
            .relative()
            .size_full()
            .child(self.inner.clone())
            .children(hangar_app::ui::shield(cx))
    }
}

#[test]
fn a_target_knows_its_folder_and_paths() {
    let code = || {
        hangar_app::code::Code::connect(
            &hangar_app::api::Client::new("http://127.0.0.1:9", "hgr_t"),
            "e1",
            "/r",
            |_| false,
        )
    };
    let file = Target {
        code: code(),
        root: Some("/home/dev/app".into()),
        abs: "/home/dev/app/src/main.rs".into(),
        dir: false,
    };
    assert_eq!(file.name(), "main.rs");
    assert_eq!(file.folder(), "/home/dev/app/src");
    assert_eq!(file.relative(), "src/main.rs");
    let folder = Target {
        dir: true,
        abs: "/home/dev/app/src".into(),
        ..file.clone()
    };
    assert_eq!(folder.folder(), "/home/dev/app/src");
    let top = Target {
        code: code(),
        root: None,
        abs: "/etc".into(),
        dir: false,
    };
    assert_eq!(top.folder(), "/");
    assert_eq!(
        join("/home/dev/app/", "/notes.md"),
        "/home/dev/app/notes.md"
    );
}
