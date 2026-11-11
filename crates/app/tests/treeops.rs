//! The Project window's menu on a file: making a file beside it, through a
//! stand-in Code service, as a right-click, a name typed into the tree and
//! Enter make it; a name taken is said under it, and what is made is
//! selected.

use std::net::TcpListener;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use gpui::{AppContext, Modifiers, MouseButton, TestAppContext, VisualTestContext};
use gpui_component::{Root, WindowExt};
use hangar_app::places::Places;
use hangar_app::project::{OpenFile, ProjectView};
use hangar_app::store::MemoryTokens;
use hangar_app::treeops::{Check, Target, check, join, place};
use hangar_app::workspace::Workspace;
use serde_json::{Value, json};
use tungstenite::Message;

/// A Code service with one project, /home/dev/app, holding src/ and
/// main.rs; what it is asked to change is sent to asked.
fn service(asked: mpsc::Sender<(String, Value)>) -> String {
    let ln = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", ln.local_addr().unwrap());
    // The files made in the project's root, listed after its own.
    let made = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    thread::spawn(move || {
        for tcp in ln.incoming() {
            let (asked, made) = (asked.clone(), made.clone());
            thread::spawn(move || {
                let mut ws = tungstenite::accept(tcp.unwrap()).unwrap();
                while let Ok(Message::Binary(b)) = ws.read() {
                    let req: Value = serde_json::from_slice(&b[1..]).unwrap();
                    let (method, params) = (req["method"].as_str().unwrap(), &req["params"]);
                    let result = match method {
                        "hello" => json!({ "root": params["root"], "home": "/home/dev" }),
                        "workspace.get" => json!({ "folders": ["/home/dev/app"] }),
                        "fs.list" if params["path"] == "" || params["path"] == "/home/dev/app" => {
                            let mut entries = vec![
                                json!({ "name": "src", "dir": true, "size": 0 }),
                                json!({ "name": "main.rs", "dir": false, "size": 10 }),
                            ];
                            // More than the window shows, as a real
                            // folder has.
                            for i in 0..120 {
                                entries.push(json!({ "name": format!("note{i:03}.md"), "dir": false, "size": 1 }));
                            }
                            for name in made.lock().unwrap().iter() {
                                entries.push(json!({ "name": name, "dir": false, "size": 0 }));
                            }
                            Value::Array(entries)
                        }
                        "fs.list"
                            if params["path"] == "src" || params["path"] == "/home/dev/app/src" =>
                        {
                            json!([
                            { "name": ".env", "dir": false, "size": 3 },
                            { "name": "target", "dir": true, "size": 0, "ignored": true },
                            { "name": "lib.rs", "dir": false, "size": 3 },
                            ])
                        }
                        "fs.list" => json!([]),
                        "git.status" => {
                            json!({ "repo": true, "changes": [], "repos": [{ "path": "" }] })
                        }
                        "git.log" => json!({ "commits": [
                            { "hash": "a1b2c3d4e5f6", "author": "Ada", "subject": "Fix the rounding" },
                            { "hash": "0f0f0f0f0f0f", "author": "Ada", "subject": "First" },
                        ]}),
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
                        // A file made in the root is listed there, and the
                        // folder said to have changed, as the watcher says.
                        "fs.create" => {
                            asked.send(("fs.create".into(), params.clone())).unwrap();
                            let path = params["path"].as_str().unwrap_or_default();
                            if let Some(name) = path.strip_prefix("/home/dev/app/") {
                                made.lock().unwrap().push(name.to_string());
                            }
                            send(&mut ws, json!({ "id": req["id"], "result": null }));
                            let changed = json!({ "event": "fs.changed", "params": { "dirs": ["/home/dev/app"] } });
                            send(&mut ws, changed);
                            continue;
                        }
                        // What goes to the trash is answered with the
                        // id that brings it back.
                        "fs.trash" => {
                            asked.send(("fs.trash".into(), params.clone())).unwrap();
                            let items: Vec<Value> = params["paths"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .enumerate()
                                .map(|(i, p)| json!({ "path": p, "id": format!("t{i}") }))
                                .collect();
                            Value::Array(items)
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
    // New follows Open and Open With, a file's first items; its File comes
    // first.
    cx.simulate_keystrokes("down down down right enter");
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
    until(cx, "the row typed in goes", |cx| {
        cx.update(|window, _| window.refresh());
        cx.debug_bounds("tree-naming").is_none()
    });
    tree.read_with(cx, |t, _| {
        let s = t.selected().unwrap();
        assert_eq!(
            (s.root.as_deref(), s.path.as_str()),
            (Some("/home/dev/app"), "notes.md"),
            "the new file is selected"
        );
    });
}

/// A name already taken is said under it in the tree, which keeps it; the
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
    cx.simulate_keystrokes("down down down right enter");
    cx.run_until_parked();
    cx.simulate_input("main.rs");
    // Said as it is typed, before Enter, as VS Code says it.
    until(cx, "the name said to be taken", |cx| {
        cx.update(|window, _| window.refresh());
        cx.debug_bounds("name-wrong").is_some()
    });
    cx.simulate_keystrokes("enter");
    // Enter takes no name said to be wrong; the row stays, for another.
    let asked_at = Instant::now();
    while asked_at.elapsed() < Duration::from_millis(400) {
        cx.run_until_parked();
        thread::sleep(Duration::from_millis(10));
    }
    cx.update(|window, _| window.refresh());
    assert!(cx.debug_bounds("tree-naming").is_some(), "the row stays");
    assert!(asked.try_recv().is_err(), "nothing was made");

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    assert!(
        cx.debug_bounds("tree-naming").is_none(),
        "Escape lets it go"
    );

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
    // Its New, File makes the file in the project's root.
    cx.simulate_keystrokes("down right enter");
    cx.run_until_parked();
    cx.simulate_input("zz-last.md");
    cx.simulate_keystrokes("enter");
    until(cx, "the file made is selected", |cx| {
        tree.read_with(cx, |t, _| t.selected().map(|s| s.path.clone())) == Some("zz-last.md".into())
    });
    until(cx, "it is scrolled into view", |cx| {
        cx.update(|window, _| window.refresh());
        let height = cx.update(|w, _| w.bounds().size.height);
        cx.debug_bounds("tree-row-zz-last.md")
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

/// Git → Compare with Revision…: the commits that changed the file, in a
/// list; Enter compares it with the first.
#[gpui::test]
fn a_file_is_compared_with_a_revision_chosen(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(gpui_component::init);
    let (asked_tx, _asked) = mpsc::channel();
    let url = service(asked_tx);
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let id = places.update(cx, |p, cx| p.add_server(&url, "hgr_t", cx).unwrap());
    let (diff_tx, diffs) = mpsc::channel();
    let (_, cx) = cx.add_window_view(|window, cx| {
        let ws = cx.new(|cx| Workspace::new(places.clone(), &id, "e1", "/home/dev", cx));
        let view = cx.new(|cx| ProjectView::new(ws, cx));
        cx.subscribe(&view, move |_, _, e: &hangar_app::diff::ShowDiff, _| {
            diff_tx.send(e.0.clone()).unwrap();
        })
        .detach();
        Root::new(view, window, cx)
    });
    until(cx, "the project's files", |cx| {
        cx.debug_bounds("tree-row-main.rs").is_some()
    });
    // Its status read, so the menu knows it is in a repository.
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(300) {
        cx.run_until_parked();
        thread::sleep(Duration::from_millis(10));
    }
    let row = cx.debug_bounds("tree-row-main.rs").unwrap();
    cx.simulate_mouse_down(row.center(), MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(row.center(), MouseButton::Right, Modifiers::none());
    cx.run_until_parked();
    // Git is the seventeenth item that can be chosen; its fourth is
    // Compare with Revision…, after Show History, Annotate and Compare
    // with Branch….
    let to_git = ["down"; 17].join(" ");
    cx.simulate_keystrokes(&format!("{to_git} right down down down enter"));
    until(cx, "the list of commits", |cx| {
        cx.debug_bounds("pick-row-1").is_some()
    });
    cx.simulate_keystrokes("enter");
    let start = Instant::now();
    let spec = loop {
        cx.run_until_parked();
        if let Ok(s) = diffs.try_recv() {
            break s;
        }
        assert!(start.elapsed() < Duration::from_secs(5), "a diff asked for");
    };
    assert_eq!(
        (
            spec.root.as_str(),
            spec.path.as_str(),
            spec.against.as_str()
        ),
        ("/home/dev/app", "main.rs", "a1b2c3d4e5f6")
    );
}

/// A file dragged onto a folder is moved there; src has a main.rs, asked
/// about first.
#[gpui::test]
fn a_file_dragged_onto_a_folder_is_moved_there(cx: &mut TestAppContext) {
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
        cx.debug_bounds("tree-row-main.rs").is_some()
    });
    let next = |cx: &mut VisualTestContext| {
        let start = Instant::now();
        loop {
            cx.run_until_parked();
            if let Ok(m) = asked.recv_timeout(Duration::from_millis(20)) {
                return m;
            }
            assert!(start.elapsed() < Duration::from_secs(5), "a call");
        }
    };
    let start = cx.debug_bounds("tree-row-main.rs").unwrap().center();
    cx.simulate_mouse_move(start, None, Modifiers::none());
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    for dy in [-4., -10., -16.] {
        cx.simulate_mouse_move(
            start + gpui::point(gpui::px(0.), gpui::px(dy)),
            MouseButton::Left,
            Modifiers::none(),
        );
    }
    cx.run_until_parked();
    let src = cx.debug_bounds("tree-row-src").unwrap().center();
    cx.simulate_mouse_move(src, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(src, MouseButton::Left, Modifiers::none());
    let (m, p) = next(cx);
    assert_eq!(m, "fs.transfer");
    assert_eq!(
        (p["op"].as_str(), p["to"].as_str(), p["clash"].as_str()),
        (Some("move"), Some("/home/dev/app/src"), Some("ask"))
    );
    assert_eq!(p["from"], json!(["/home/dev/app/main.rs"]));
    until(cx, "the question", |cx| {
        cx.debug_bounds("clash-replace").is_some()
    });
    let replace = cx.debug_bounds("clash-replace").unwrap().center();
    cx.simulate_click(replace, Modifiers::none());
    let (_, p) = next(cx);
    assert_eq!(p["clash"], "replace");
}

#[test]
fn what_may_be_dropped_where() {
    use hangar_app::project::may_drop;
    assert!(may_drop("/a/b.txt", false, "/a/c"));
    assert!(!may_drop("/a/b.txt", false, "/a"), "its own folder");
    assert!(!may_drop("/a/d", true, "/a/d"), "itself");
    assert!(!may_drop("/a/d", true, "/a/d/e"), "a folder in it");
    assert!(may_drop("/a/d", true, "/a/de"), "a name it begins");
    assert!(!may_drop("/x", false, "/"), "the root's own");
}

/// ⌘-click adds a row to those chosen and ⇧-click takes a range; the menu
/// on them compares two files, and Delete asks about them all.
#[gpui::test]
fn several_rows_are_chosen_and_acted_on(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(|cx| {
        gpui_component::init(cx);
        hangar_app::project::bind_keys(cx);
    });
    let (asked_tx, asked) = mpsc::channel();
    let url = service(asked_tx);
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let id = places.update(cx, |p, cx| p.add_server(&url, "hgr_t", cx).unwrap());
    let (diff_tx, diffs) = mpsc::channel();
    let mut tree = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let ws = cx.new(|cx| Workspace::new(places.clone(), &id, "e1", "/home/dev", cx));
        let view = cx.new(|cx| ProjectView::new(ws, cx));
        cx.subscribe(&view, move |_, _, e: &hangar_app::diff::ShowDiff, _| {
            diff_tx.send(e.0.clone()).unwrap();
        })
        .detach();
        tree = Some(view.clone());
        Root::new(view, window, cx)
    });
    let tree = tree.unwrap();
    until(cx, "the project's files", |cx| {
        cx.debug_bounds("tree-row-note001.md").is_some()
    });
    let chosen = |cx: &mut VisualTestContext| {
        tree.read_with(cx, |t, _| {
            t.chosen().into_iter().map(|s| s.path).collect::<Vec<_>>()
        })
    };
    let toggle = if cfg!(target_os = "macos") {
        Modifiers::command()
    } else {
        Modifiers::control()
    };
    let at = |cx: &mut VisualTestContext, row: &'static str| cx.debug_bounds(row).unwrap().center();
    let main = at(cx, "tree-row-main.rs");
    cx.simulate_click(main, Modifiers::none());
    let note = at(cx, "tree-row-note001.md");
    cx.simulate_click(note, toggle);
    assert_eq!(chosen(cx), ["note001.md", "main.rs"]);

    // Their menu: Cut, Copy, Copy Absolute Paths, Delete…, Compare Files.
    cx.simulate_mouse_down(note, MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(note, MouseButton::Right, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(chosen(cx).len(), 2, "a right-click among them keeps them");
    cx.simulate_keystrokes("down down down down down enter");
    cx.run_until_parked();
    let spec = diffs.try_recv().expect("Compare Files");
    assert_eq!(spec.path, "main.rs");
    assert_eq!(
        spec.other,
        Some(hangar_app::diff::Other::File {
            root: "/home/dev/app".into(),
            abs: "/home/dev/app/note001.md".into(),
        })
    );

    // A range, from the last plain click.
    let (n0, n2) = (at(cx, "tree-row-note000.md"), at(cx, "tree-row-note002.md"));
    cx.simulate_click(n0, Modifiers::none());
    cx.simulate_click(n2, Modifiers::shift());
    assert_eq!(chosen(cx), ["note002.md", "note000.md", "note001.md"]);
    cx.simulate_keystrokes("shift-up");
    assert_eq!(chosen(cx), ["note001.md", "note000.md"], "⇧↑ shrinks it");

    // Delete asks about them all, then trashes them together.
    let delete = if cfg!(target_os = "macos") {
        "cmd-backspace"
    } else {
        "delete"
    };
    cx.simulate_keystrokes(delete);
    cx.run_until_parked();
    let ok = cx.debug_bounds("dialog-ok").unwrap().center();
    cx.simulate_click(ok, Modifiers::none());
    let start = Instant::now();
    let (m, p) = loop {
        cx.run_until_parked();
        if let Ok(m) = asked.try_recv() {
            break m;
        }
        assert!(start.elapsed() < Duration::from_secs(5), "the trash");
    };
    assert_eq!(m, "fs.trash");
    assert_eq!(
        p["paths"],
        json!(["/home/dev/app/note001.md", "/home/dev/app/note000.md"])
    );
}

#[test]
fn names_are_read_and_checked_as_intellij_reads_them() {
    assert_eq!(place("/home/dev/app", "notes.md"), "/home/dev/app/notes.md");
    assert_eq!(
        place("/home/dev/app", "docs/a/b.md"),
        "/home/dev/app/docs/a/b.md"
    );
    assert_eq!(place("/home/dev/app", "./x/../y.md"), "/home/dev/app/y.md");
    assert_eq!(place("/home/dev/app", "../../z"), "/home/z");
    assert_eq!(place("/", "../a"), "/a");

    let listed = |f: &str| -> Option<Vec<(String, bool)>> {
        match f {
            "/home/dev/app" => Some(vec![("src".into(), true), ("main.rs".into(), false)]),
            "/home/dev/app/src" => Some(vec![("lib.rs".into(), false)]),
            _ => None,
        }
    };
    let at = |name: &str| check("/home/dev/app", name, None, &listed);
    assert_eq!(at(""), Check::Fine, "nothing typed, nothing said");
    assert_eq!(at("notes.md"), Check::Fine);
    assert_eq!(
        at("main.rs"),
        Check::Wrong("A file named “main.rs” already exists".into())
    );
    assert_eq!(
        at("src"),
        Check::Wrong("A directory named “src” already exists".into())
    );
    assert_eq!(
        at("src/lib.rs"),
        Check::Wrong("A file named “lib.rs” already exists".into())
    );
    assert_eq!(at("src/new.rs"), Check::Fine);
    assert_eq!(
        at("main.rs/x"),
        Check::Wrong("“main.rs” is a file, not a directory".into())
    );
    assert_eq!(
        at("docs/main.rs"),
        Check::Fine,
        "made on the way: nothing there"
    );
    assert_eq!(at("x/.."), Check::Wrong("“..” is not a name".into()));
    assert_eq!(
        at("../a"),
        Check::List("/home/dev".into()),
        "its folder listed first"
    );
    assert_eq!(
        check("/", "../a", None, &listed),
        Check::Wrong("There is nothing above /".into())
    );
    assert_eq!(
        check(
            "/home/dev/app",
            "main.rs",
            Some("/home/dev/app/main.rs"),
            &listed
        ),
        Check::Fine,
        "what is renamed may keep its name"
    );
}

/// ⌘N opens New's list under the row, File marked: Enter asks its name,
/// and a name with folders in it makes them on the way. Directory's name is
/// checked as File's is.
#[gpui::test]
fn new_from_the_keys_makes_folders_on_the_way(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(gpui_component::init);
    hangar_app::project::bind_keys(&mut cx.app.borrow_mut());
    let (asked_tx, asked) = mpsc::channel();
    let url = service(asked_tx);
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let id = places.update(cx, |p, cx| p.add_server(&url, "hgr_t", cx).unwrap());
    let (opened_tx, opened) = mpsc::channel();
    let (_, cx) = cx.add_window_view(|window, cx| {
        let ws = cx.new(|cx| Workspace::new(places.clone(), &id, "e1", "/home/dev", cx));
        let view = cx.new(|cx| ProjectView::new(ws, cx));
        cx.subscribe(&view, move |_, _, e: &OpenFile, _| {
            opened_tx.send(e.clone()).unwrap();
        })
        .detach();
        Root::new(view, window, cx)
    });
    until(cx, "the project's files", |cx| {
        cx.debug_bounds("tree-row-main.rs").is_some()
    });
    let row = cx.debug_bounds("tree-row-main.rs").unwrap();
    cx.simulate_click(row.center(), Modifiers::none());
    let new = if cfg!(target_os = "macos") {
        "cmd-n"
    } else {
        "ctrl-n"
    };
    cx.simulate_keystrokes(new);
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    assert!(
        cx.debug_bounds("tree-naming").is_some(),
        "File, marked, takes its name in the tree"
    );
    cx.simulate_input("docs/today.md");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    let next = |cx: &mut VisualTestContext| {
        let got = std::cell::RefCell::new(None);
        until(cx, "a call", |_| {
            if let Ok(m) = asked.try_recv() {
                *got.borrow_mut() = Some(m);
            }
            got.borrow().is_some()
        });
        got.into_inner().unwrap()
    };
    let (m, p) = next(cx);
    assert_eq!(
        (m.as_str(), &p["path"]),
        ("fs.mkdir", &json!("/home/dev/app/docs"))
    );
    let (m, p) = next(cx);
    assert_eq!(
        (m.as_str(), &p["path"]),
        ("fs.create", &json!("/home/dev/app/docs/today.md"))
    );
    until(cx, "the file opened", |_| {
        opened
            .try_recv()
            .is_ok_and(|o| o.root == "/home/dev/app" && o.path == "docs/today.md")
    });

    // Directory, the list's second item: a file on the way is said.
    let row = cx.debug_bounds("tree-row-main.rs").unwrap();
    cx.simulate_click(row.center(), Modifiers::none());
    cx.simulate_keystrokes(new);
    cx.run_until_parked();
    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();
    cx.simulate_input("src/lib.rs/x");
    until(cx, "the file on the way said", |cx| {
        cx.update(|window, _| window.refresh());
        cx.debug_bounds("name-wrong").is_some()
    });
    cx.simulate_keystrokes("enter");
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(200) {
        cx.run_until_parked();
        thread::sleep(Duration::from_millis(10));
    }
    assert!(asked.try_recv().is_err(), "nothing was made");
    cx.update(|window, _| window.refresh());
    assert!(cx.debug_bounds("tree-naming").is_some());
}

/// The row a name is typed in sits where VS Code puts it: a new directory
/// first among its folder's entries, a new file after the folders, and a
/// rename in the place of what is renamed, its name chosen up to its
/// extension. Clicking away takes a name that can be taken.
#[gpui::test]
fn names_are_typed_where_the_entry_goes(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(gpui_component::init);
    hangar_app::project::bind_keys(&mut cx.app.borrow_mut());
    let (asked_tx, asked) = mpsc::channel();
    let url = service(asked_tx);
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let id = places.update(cx, |p, cx| p.add_server(&url, "hgr_t", cx).unwrap());
    let (_, cx) = cx.add_window_view(|window, cx| {
        let ws = cx.new(|cx| Workspace::new(places.clone(), &id, "e1", "/home/dev", cx));
        let view = cx.new(|cx| ProjectView::new(ws, cx));
        Root::new(view, window, cx)
    });
    // Focus leaving the field is told only in the window in front.
    cx.update(|window, _| window.activate_window());
    until(cx, "the project's files", |cx| {
        cx.debug_bounds("tree-row-main.rs").is_some()
    });
    let new = if cfg!(target_os = "macos") {
        "cmd-n"
    } else {
        "ctrl-n"
    };
    let bounds = |cx: &mut VisualTestContext, at: &'static str| {
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        cx.debug_bounds(at).unwrap_or_else(|| panic!("{at}"))
    };
    let main = bounds(cx, "tree-row-main.rs").center();
    cx.simulate_click(main, Modifiers::none());
    cx.simulate_keystrokes(new);
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    let (naming, src, main_row) = (
        bounds(cx, "tree-naming"),
        bounds(cx, "tree-row-src"),
        bounds(cx, "tree-row-main.rs"),
    );
    assert_eq!(naming.top(), src.bottom(), "a file after the folders");
    assert_eq!(main_row.top(), naming.bottom());
    cx.simulate_keystrokes("escape");

    cx.simulate_keystrokes(new);
    cx.run_until_parked();
    cx.simulate_keystrokes("down enter");
    let (naming, project) = (
        bounds(cx, "tree-naming"),
        bounds(cx, "project-row-/home/dev/app"),
    );
    assert_eq!(naming.top(), project.bottom(), "a folder first");
    cx.simulate_keystrokes("escape");

    cx.simulate_keystrokes("shift-f6");
    let naming = bounds(cx, "tree-naming");
    cx.update(|window, _| window.refresh());
    assert!(
        cx.debug_bounds("tree-row-main.rs").is_none(),
        "the row renamed is stood in for"
    );
    assert_eq!(naming.top(), src.bottom());
    // "main" is chosen, so what is typed takes its place.
    cx.simulate_input("app");
    cx.run_until_parked();
    let src = bounds(cx, "tree-row-src").center();
    cx.simulate_click(src, Modifiers::none());
    let start = Instant::now();
    let (m, p) = loop {
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        if let Ok(got) = asked.recv_timeout(Duration::from_millis(20)) {
            break got;
        }
        assert!(start.elapsed() < Duration::from_secs(5), "the rename");
    };
    assert_eq!(m, "fs.rename");
    assert_eq!(
        (p["from"].as_str(), p["to"].as_str()),
        (Some("/home/dev/app/main.rs"), Some("/home/dev/app/app.rs"))
    );
}

/// ⌘Z in the tree undoes what it did to files, as IntelliJ's does: a file
/// deleted comes back out of the trash, ⇧⌘Z deletes it again, and a rename
/// is renamed back.
#[gpui::test]
fn file_changes_are_undone_and_redone(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(gpui_component::init);
    hangar_app::project::bind_keys(&mut cx.app.borrow_mut());
    let (asked_tx, asked) = mpsc::channel();
    let url = service(asked_tx);
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let id = places.update(cx, |p, cx| p.add_server(&url, "hgr_t", cx).unwrap());
    let (_, cx) = cx.add_window_view(|window, cx| {
        let ws = cx.new(|cx| Workspace::new(places.clone(), &id, "e1", "/home/dev", cx));
        let view = cx.new(|cx| ProjectView::new(ws, cx));
        Root::new(view, window, cx)
    });
    cx.update(|window, _| window.activate_window());
    until(cx, "the project's files", |cx| {
        cx.debug_bounds("tree-row-main.rs").is_some()
    });
    let mac = cfg!(target_os = "macos");
    let (delete, undo, redo) = if mac {
        ("cmd-backspace", "cmd-z", "cmd-shift-z")
    } else {
        ("delete", "ctrl-z", "ctrl-shift-z")
    };
    let next = |cx: &mut VisualTestContext| {
        let got = std::cell::RefCell::new(None);
        until(cx, "a call", |_| {
            if let Ok(m) = asked.try_recv() {
                *got.borrow_mut() = Some(m);
            }
            got.borrow().is_some()
        });
        let got = got.into_inner().unwrap();
        // Its answer is taken before the next key is pressed.
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(150) {
            cx.run_until_parked();
            thread::sleep(Duration::from_millis(10));
        }
        got
    };
    let row = cx.debug_bounds("tree-row-main.rs").unwrap();
    cx.simulate_click(row.center(), Modifiers::none());
    cx.simulate_keystrokes(delete);
    until(cx, "Delete asks", |cx| {
        cx.update(|window, _| window.refresh());
        cx.debug_bounds("dialog-ok").is_some()
    });
    let ok = cx.debug_bounds("dialog-ok").unwrap().center();
    cx.simulate_click(ok, Modifiers::none());
    let (m, p) = next(cx);
    assert_eq!(
        (m.as_str(), &p["paths"]),
        ("fs.trash", &json!(["/home/dev/app/main.rs"]))
    );

    let row = cx.debug_bounds("tree-row-src").unwrap();
    cx.simulate_click(row.center(), Modifiers::none());
    cx.simulate_keystrokes(undo);
    let (m, p) = next(cx);
    assert_eq!(m, "fs.restore");
    assert_eq!(
        p["items"],
        json!([{ "path": "/home/dev/app/main.rs", "id": "t0" }])
    );
    cx.simulate_keystrokes(redo);
    let (m, p) = next(cx);
    assert_eq!(
        (m.as_str(), &p["paths"]),
        ("fs.trash", &json!(["/home/dev/app/main.rs"]))
    );
    cx.simulate_keystrokes(undo);
    assert_eq!(next(cx).0, "fs.restore");

    // A rename, undone by renaming back.
    let row = cx.debug_bounds("tree-row-main.rs").unwrap();
    cx.simulate_click(row.center(), Modifiers::none());
    cx.simulate_keystrokes("shift-f6");
    cx.run_until_parked();
    cx.simulate_input("app");
    cx.simulate_keystrokes("enter");
    let (m, p) = next(cx);
    assert_eq!(
        (m.as_str(), p["to"].as_str()),
        ("fs.rename", Some("/home/dev/app/app.rs"))
    );
    until(cx, "the row typed in goes", |cx| {
        cx.update(|window, _| window.refresh());
        cx.debug_bounds("tree-naming").is_none()
    });
    cx.simulate_keystrokes(undo);
    let (m, p) = next(cx);
    assert_eq!(
        (m.as_str(), p["from"].as_str(), p["to"].as_str()),
        (
            "fs.rename",
            Some("/home/dev/app/app.rs"),
            Some("/home/dev/app/main.rs")
        )
    );
}
