//! The Project window's menu on a file: making a file beside it, through a
//! stand-in Code service, as a right-click, a name typed and Enter make it;
//! a name taken is said in the pop-up, and what is made is selected.

use std::net::TcpListener;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use gpui::{AppContext, Modifiers, MouseButton, TestAppContext, VisualTestContext};
use gpui_component::{Root, WindowExt};
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
                        "fs.list" if params["path"] == "" => {
                            let mut entries = vec![
                                json!({ "name": "src", "dir": true, "size": 0 }),
                                json!({ "name": "main.rs", "dir": false, "size": 10 }),
                            ];
                            // More than the window shows, as a real
                            // folder has.
                            for i in 0..120 {
                                entries.push(json!({ "name": format!("note{i:03}.md"), "dir": false, "size": 1 }));
                            }
                            Value::Array(entries)
                        }
                        "fs.list" if params["path"] == "src" => json!([
                            { "name": ".env", "dir": false, "size": 3 },
                            { "name": "target", "dir": true, "size": 0, "ignored": true },
                            { "name": "lib.rs", "dir": false, "size": 3 },
                        ]),
                        "fs.list" => json!([]),
                        "git.status" => json!({ "changes": [] }),
                        "fs.create"
                            if params["path"]
                                .as_str()
                                .is_some_and(|p| p.ends_with("/main.rs")) =>
                        {
                            let reply =
                                json!({ "id": req["id"], "error": "main.rs already exists" });
                            send(&mut ws, reply);
                            continue;
                        }
                        // A name src has is asked about.
                        "fs.transfer" => {
                            asked.send(("fs.transfer".into(), params.clone())).unwrap();
                            let to = params["to"].as_str().unwrap_or_default();
                            if params["clash"] == "ask" && to == "/home/dev/app/src" {
                                json!({ "clashes": ["main.rs"], "done": [] })
                            } else {
                                let name = params["name"].as_str().unwrap_or("x");
                                json!({ "clashes": [], "done": [{ "from": params["from"][0], "to": format!("{to}/{name}") }] })
                            }
                        }
                        m if m.starts_with("fs.") || m == "workspace.set" => {
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
    let tree = tree.unwrap();
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
    assert!(
        !cx.update(|window, cx| window.has_active_dialog(cx)),
        "the pop-up closed"
    );
    tree.read_with(cx, |t, _| {
        let s = t.selected().unwrap();
        assert_eq!(
            (s.root.as_deref(), s.path.as_str()),
            (Some("/home/dev/app"), "notes.md"),
            "the new file is selected"
        );
    });
}

/// A name already taken is said under it in the pop-up, which stays; the
/// file made with another is selected and scrolled into view, here at the
/// foot of a long folder.
#[gpui::test]
fn a_name_taken_is_said_and_the_file_made_is_shown(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(gpui_component::init);
    let (asked_tx, asked) = mpsc::channel();
    let url = service(asked_tx);
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let id = places.update(cx, |p, cx| p.add_server(&url, "hgr_t", cx).unwrap());
    let mut tree = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let ws = cx.new(|cx| Workspace::new(places.clone(), &id, "e1", "/home/dev", cx));
        let view = cx.new(|cx| ProjectView::new(ws, cx));
        tree = Some(view.clone());
        Root::new(view, window, cx)
    });
    let tree = tree.unwrap();
    until(cx, "the project's files", |cx| {
        cx.debug_bounds("tree-row-main.rs").is_some()
    });
    assert!(
        cx.debug_bounds("tree-row-note119.md")
            .is_none_or(|b| b.top() > cx.update(|w, _| w.bounds().size.height)),
        "the folder's foot is out of view"
    );
    let row = cx.debug_bounds("tree-row-main.rs").unwrap();
    cx.simulate_mouse_down(row.center(), MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(row.center(), MouseButton::Right, Modifiers::none());
    cx.run_until_parked();
    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();
    cx.simulate_input("main.rs");
    cx.simulate_keystrokes("enter");
    // The service refuses; the pop-up stays, for another name.
    let asked_at = Instant::now();
    while asked_at.elapsed() < Duration::from_millis(400) {
        cx.run_until_parked();
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        cx.update(|window, cx| window.has_active_dialog(cx)),
        "the pop-up stays"
    );
    assert!(asked.try_recv().is_err(), "nothing was made");

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!cx.update(|window, cx| window.has_active_dialog(cx)));

    // A project's own row is selected by the right-click that opens its
    // menu, as its files are.
    let root = cx.debug_bounds("project-row-/home/dev/app").unwrap();
    cx.simulate_mouse_down(root.center(), MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(root.center(), MouseButton::Right, Modifiers::none());
    cx.run_until_parked();
    tree.read_with(cx, |t, _| {
        let s = t.selected().unwrap();
        assert_eq!(
            (s.root.as_deref(), s.path.as_str()),
            (Some("/home/dev/app"), "")
        );
    });
    // Its New File… makes the file in the project's root.
    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();
    cx.simulate_input("note119.md");
    cx.simulate_keystrokes("enter");
    until(cx, "the file made is selected", |cx| {
        tree.read_with(cx, |t, _| t.selected().map(|s| s.path.clone())) == Some("note119.md".into())
    });
    assert!(!cx.update(|window, cx| window.has_active_dialog(cx)));
    until(cx, "it is scrolled into view", |cx| {
        cx.update(|window, _| window.refresh());
        let height = cx.update(|w, _| w.bounds().size.height);
        cx.debug_bounds("tree-row-note119.md")
            .is_some_and(|b| b.top() >= gpui::px(0.) && b.bottom() <= height)
    });
}

/// Rows keep their height however many there are: the tree scrolls rather
/// than squeezing them.
#[gpui::test]
fn many_rows_keep_their_height(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(gpui_component::init);
    let (asked_tx, _asked) = mpsc::channel();
    let url = service(asked_tx);
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let id = places.update(cx, |p, cx| p.add_server(&url, "hgr_t", cx).unwrap());
    let (_, cx) = cx.add_window_view(|window, cx| {
        let ws = cx.new(|cx| Workspace::new(places.clone(), &id, "e1", "/home/dev", cx));
        let view = cx.new(|cx| ProjectView::new(ws, cx));
        Root::new(view, window, cx)
    });
    until(cx, "the project's files", |cx| {
        cx.debug_bounds("tree-row-note119.md").is_some()
    });
    for row in [
        "tree-row-main.rs",
        "tree-row-note000.md",
        "tree-row-note119.md",
    ] {
        let h = cx.debug_bounds(row).unwrap().size.height;
        assert_eq!(h, gpui::px(20.), "{row} squeezed to {h:?}");
    }
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

#[test]
fn the_workspace_reordered() {
    use hangar_app::treeops::{Reorder, reordered};
    let roots = vec!["/a".to_string(), "/b".into(), "/c".into()];
    assert_eq!(
        reordered(&roots, Reorder::Add("/d".into())),
        ["/a", "/b", "/c", "/d"]
    );
    assert_eq!(
        reordered(&roots, Reorder::Add("/b".into())),
        ["/a", "/b", "/c"],
        "once"
    );
    assert_eq!(
        reordered(&roots, Reorder::Remove("/b".into())),
        ["/a", "/c"]
    );
    assert_eq!(
        reordered(&roots, Reorder::Move("/c".into(), -1)),
        ["/a", "/c", "/b"]
    );
    assert_eq!(
        reordered(&roots, Reorder::Move("/a".into(), -1)),
        ["/a", "/b", "/c"],
        "the first stays"
    );
    assert_eq!(
        reordered(&roots, Reorder::Move("/a".into(), 1)),
        ["/b", "/a", "/c"]
    );
}

/// A folder's menu adds it to the workspace, which the service is asked to
/// keep: the projects there were, then the folder.
#[gpui::test]
fn a_folder_is_added_to_the_workspace(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(gpui_component::init);
    let (asked_tx, asked) = mpsc::channel();
    let url = service(asked_tx);
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let id = places.update(cx, |p, cx| p.add_server(&url, "hgr_t", cx).unwrap());
    let (_, cx) = cx.add_window_view(|window, cx| {
        let ws = cx.new(|cx| Workspace::new(places.clone(), &id, "e1", "/home/dev", cx));
        let view = cx.new(|cx| ProjectView::new(ws, cx));
        Root::new(view, window, cx)
    });
    until(cx, "the project's files", |cx| {
        cx.debug_bounds("tree-row-src").is_some()
    });
    let folder = cx.debug_bounds("tree-row-src").unwrap();
    cx.simulate_mouse_down(folder.center(), MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(folder.center(), MouseButton::Right, Modifiers::none());
    cx.run_until_parked();
    // Add to Workspace is a folder's last item.
    cx.simulate_keystrokes("up enter");
    let set = std::cell::RefCell::new(None);
    until(cx, "the workspace set", |_| {
        if let Ok(m) = asked.try_recv() {
            *set.borrow_mut() = Some(m);
        }
        set.borrow().is_some()
    });
    let (method, params) = set.into_inner().unwrap();
    assert_eq!(method, "workspace.set");
    assert_eq!(
        params["folders"],
        json!(["/home/dev/app", "/home/dev/app/src"])
    );
}

/// The tree's keys, as IntelliJ's: up and down, Home and End (scrolled to),
/// right and left over folders, Enter to open, Escape back to the editor,
/// Copy and Paste into a folder, and Delete asking first.
#[gpui::test]
fn the_tree_is_used_from_the_keyboard(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(|cx| {
        gpui_component::init(cx);
        hangar_app::project::bind_keys(cx);
    });
    let (asked_tx, asked) = mpsc::channel();
    let url = service(asked_tx);
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let id = places.update(cx, |p, cx| p.add_server(&url, "hgr_t", cx).unwrap());
    let (opened_tx, opened) = mpsc::channel();
    let (left_tx, left) = mpsc::channel();
    let mut tree = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let ws = cx.new(|cx| Workspace::new(places.clone(), &id, "e1", "/home/dev", cx));
        let view = cx.new(|cx| ProjectView::new(ws, cx));
        cx.subscribe(&view, move |_, _, e: &OpenFile, _| {
            opened_tx.send(e.clone()).unwrap();
        })
        .detach();
        cx.subscribe(&view, move |_, _, _: &hangar_app::project::Leave, _| {
            left_tx.send(()).unwrap();
        })
        .detach();
        tree = Some(view.clone());
        Root::new(view, window, cx)
    });
    let tree = tree.unwrap();
    until(cx, "the project's files", |cx| {
        cx.debug_bounds("tree-row-main.rs").is_some()
    });
    let selected = |cx: &mut VisualTestContext| {
        tree.read_with(cx, |t, _| t.selected().map(|s| s.path.clone()))
            .unwrap_or_default()
    };
    let main = cx.debug_bounds("tree-row-main.rs").unwrap();
    cx.simulate_click(main.center(), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(
        selected(cx),
        "main.rs",
        "a click selects, and takes the keys"
    );

    cx.simulate_keystrokes("up");
    assert_eq!(selected(cx), "src");
    cx.simulate_keystrokes("up");
    assert_eq!(selected(cx), "", "the project's own row");
    cx.simulate_keystrokes("left");
    assert!(
        cx.debug_bounds("tree-row-src").is_none(),
        "left shuts the project"
    );
    cx.simulate_keystrokes("right down");
    assert_eq!(selected(cx), "src", "right opens it again");
    cx.simulate_keystrokes("down down");
    assert_eq!(selected(cx), "note000.md");
    cx.simulate_keystrokes("left");
    assert_eq!(selected(cx), "", "left on a file goes to its folder");

    cx.simulate_keystrokes("end");
    assert_eq!(selected(cx), "note119.md");
    until(cx, "the last row is scrolled to", |cx| {
        cx.update(|window, _| window.refresh());
        let height = cx.update(|w, _| w.bounds().size.height);
        cx.debug_bounds("tree-row-note119.md")
            .is_some_and(|b| b.bottom() <= height)
    });
    cx.simulate_keystrokes("home down down");
    assert_eq!(selected(cx), "main.rs");

    cx.simulate_keystrokes("enter");
    let open = opened.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(open.path, "main.rs", "Enter opens the file");
    cx.simulate_keystrokes("escape");
    left.recv_timeout(Duration::from_secs(5))
        .expect("Escape gives the editor the keys");

    let (copy, paste, delete) = if cfg!(target_os = "macos") {
        ("cmd-c", "cmd-v", "cmd-backspace")
    } else {
        ("ctrl-c", "ctrl-v", "delete")
    };
    cx.simulate_keystrokes(copy);
    cx.simulate_keystrokes("up");
    cx.simulate_keystrokes(paste);
    let pasted = std::cell::RefCell::new(None);
    until(cx, "the copy is pasted into src", |_| {
        if let Ok(m) = asked.try_recv() {
            *pasted.borrow_mut() = Some(m);
        }
        pasted.borrow().is_some()
    });
    let (method, params) = pasted.into_inner().unwrap();
    assert_eq!(method, "fs.transfer");
    assert_eq!(params["op"], "copy");
    assert_eq!(params["from"], json!(["/home/dev/app/main.rs"]));
    assert_eq!(params["to"], "/home/dev/app/src");

    cx.simulate_keystrokes(delete);
    cx.run_until_parked();
    assert!(
        cx.update(|window, cx| window.has_active_dialog(cx)),
        "Delete asks first"
    );
}

fn running() -> hangar_app::api::Environment {
    hangar_app::api::Environment {
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
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

/// The file shown is selected in the Project window as it is shown, as
/// IntelliJ's Always Select Opened File; turned off, the tree is left be.
#[gpui::test]
fn the_file_shown_is_selected_in_the_tree(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(gpui_component::init);
    let (asked_tx, _asked) = mpsc::channel();
    let url = service(asked_tx);
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let id = places.update(cx, |p, cx| {
        let id = p.add_server(&url, "hgr_t", cx).unwrap();
        p.apply(
            &id,
            hangar_app::place::Update::Snapshot(vec![running()]),
            cx,
        );
        id
    });
    let (shell, cx) =
        cx.add_window_view(|window, cx| hangar_app::shell::Shell::new(places.clone(), window, cx));
    cx.update(|window, cx| shell.update(cx, |s, cx| s.open(&id, "e1", window, cx)));
    let tab = shell.read_with(cx, |s, _| s.active_view()).unwrap();
    until(cx, "the project's files", |cx| {
        cx.debug_bounds("tree-row-main.rs").is_some()
    });
    let tree = tab.read_with(cx, |t, _| t.project()).unwrap();
    let selected = |cx: &mut VisualTestContext| {
        tree.read_with(cx, |t, _| t.selected().map(|s| s.path.clone()))
            .unwrap_or_default()
    };
    for f in ["main.rs", "note090.md"] {
        cx.update(|window, cx| tab.update(cx, |t, cx| t.open_file("/home/dev/app", f, window, cx)));
        cx.run_until_parked();
        assert_eq!(selected(cx), f, "shown, it is selected");
    }
    until(cx, "and scrolled to", |cx| {
        cx.update(|window, _| window.refresh());
        let height = cx.update(|w, _| w.bounds().size.height);
        cx.debug_bounds("tree-row-note090.md")
            .is_some_and(|b| b.top() >= gpui::px(0.) && b.bottom() <= height)
    });

    cx.update(|_, cx| hangar_app::settings::Settings::update(cx, |s| s.select_opened = false));
    cx.update(|window, cx| {
        tab.update(cx, |t, cx| {
            t.open_file("/home/dev/app", "main.rs", window, cx)
        })
    });
    cx.run_until_parked();
    assert_eq!(
        selected(cx),
        "note090.md",
        "turned off, the tree is left be"
    );
}

#[gpui::test]
fn hidden_and_ignored_files_are_shown_as_asked(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(gpui_component::init);
    let (asked_tx, _asked) = mpsc::channel();
    let url = service(asked_tx);
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let id = places.update(cx, |p, cx| p.add_server(&url, "hgr_t", cx).unwrap());
    let (_, cx) = cx.add_window_view(|window, cx| {
        let ws = cx.new(|cx| Workspace::new(places.clone(), &id, "e1", "/home/dev", cx));
        let view = cx.new(|cx| ProjectView::new(ws, cx));
        Root::new(view, window, cx)
    });
    until(cx, "the project's files", |cx| {
        cx.debug_bounds("tree-row-src").is_some()
    });
    let src = cx.debug_bounds("tree-row-src").unwrap().center();
    cx.simulate_click(src, Modifiers::none());
    until(cx, "src's files", |cx| {
        cx.debug_bounds("tree-row-src/lib.rs").is_some()
    });
    assert!(
        cx.debug_bounds("tree-row-src/.env").is_some(),
        "shown at first"
    );
    assert!(cx.debug_bounds("tree-row-src/target").is_some());

    cx.update(|_, cx| hangar_app::settings::Settings::update(cx, |s| s.show_hidden = false));
    until(cx, "the hidden file gone", |cx| {
        cx.debug_bounds("tree-row-src/.env").is_none()
    });
    assert!(
        cx.debug_bounds("tree-row-src/target").is_some(),
        "ignored still shown"
    );

    cx.update(|_, cx| {
        hangar_app::settings::Settings::update(cx, |s| {
            s.show_hidden = true;
            s.show_ignored = false;
        })
    });
    until(cx, "the ignored folder gone", |cx| {
        cx.debug_bounds("tree-row-src/target").is_none()
            && cx.debug_bounds("tree-row-src/.env").is_some()
    });
}

/// Move… (F6) and Copy… (F5): a folder typed from the file's own and a
/// name; a name the folder has is asked about.
#[gpui::test]
fn a_file_is_moved_and_copied_to_a_folder_typed(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(|cx| {
        gpui_component::init(cx);
        hangar_app::project::bind_keys(cx);
    });
    let (asked_tx, asked) = mpsc::channel();
    let url = service(asked_tx);
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let id = places.update(cx, |p, cx| p.add_server(&url, "hgr_t", cx).unwrap());
    let (_, cx) = cx.add_window_view(|window, cx| {
        let ws = cx.new(|cx| Workspace::new(places.clone(), &id, "e1", "/home/dev", cx));
        let view = cx.new(|cx| ProjectView::new(ws, cx));
        Root::new(view, window, cx)
    });
    until(cx, "the project's files", |cx| {
        cx.debug_bounds("tree-row-main.rs").is_some()
    });
    let next = |cx: &mut VisualTestContext| {
        let start = Instant::now();
        loop {
            cx.run_until_parked();
            if let Ok((m, p)) = asked.recv_timeout(Duration::from_millis(20)) {
                return (m, p);
            }
            assert!(start.elapsed() < Duration::from_secs(5), "a call");
        }
    };
    let main = cx.debug_bounds("tree-row-main.rs").unwrap().center();
    cx.simulate_click(main, Modifiers::none());
    cx.run_until_parked();

    // Copy into a folder of its own folder's, under another name.
    cx.simulate_keystrokes("f5");
    cx.run_until_parked();
    assert!(cx.update(|window, cx| window.has_active_dialog(cx)));
    cx.simulate_input("/docs");
    cx.simulate_keystrokes("enter");
    let (m, p) = next(cx);
    assert_eq!(m, "fs.transfer");
    assert_eq!(p["op"], "copy");
    assert_eq!(p["from"], json!(["/home/dev/app/main.rs"]));
    assert_eq!(p["to"], "/home/dev/app/docs", "typed after its folder");
    assert_eq!(
        (p["name"].as_str(), p["clash"].as_str()),
        (Some("main.rs"), Some("ask"))
    );
    until(cx, "the dialog closes", |cx| {
        !cx.update(|window, cx| window.has_active_dialog(cx))
    });

    // Moved into src, which has a main.rs: Replace, asked.
    cx.simulate_click(main, Modifiers::none());
    cx.simulate_keystrokes("f6");
    cx.run_until_parked();
    cx.simulate_input("/src");
    cx.simulate_keystrokes("enter");
    let (_, p) = next(cx);
    assert_eq!(
        (p["op"].as_str(), p["clash"].as_str()),
        (Some("move"), Some("ask"))
    );
    until(cx, "the clash asked about", |cx| {
        cx.debug_bounds("clash-replace").is_some()
    });
    let replace = cx.debug_bounds("clash-replace").unwrap().center();
    cx.simulate_click(replace, Modifiers::none());
    let (_, p) = next(cx);
    assert_eq!(
        (p["to"].as_str(), p["clash"].as_str()),
        (Some("/home/dev/app/src"), Some("replace"))
    );
}

#[test]
fn a_folder_typed_is_from_the_files_own() {
    use hangar_app::transfer::folder_typed;
    assert_eq!(folder_typed("src/", "/home/dev/app"), "/home/dev/app/src");
    assert_eq!(folder_typed(" /tmp/x/ ", "/home/dev/app"), "/tmp/x");
    assert_eq!(folder_typed("/", "/home/dev/app"), "/");
    assert_eq!(folder_typed("", "/home/dev/app"), "/home/dev/app");
}
