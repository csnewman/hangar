use hangar_app::commit::{Repo, Target, amendable, commits, repos, sections, within};
use serde_json::json;

fn status() -> serde_json::Value {
    json!({
        "repo": true,
        "changes": [],
        "repos": [
            {"path": "", "name": "app", "kind": "folder", "branch": "main", "ahead": 1,
             "changes": [
                {"path": "src/main.rs", "index": "M", "worktree": "."},
                {"path": "src/lib.rs", "index": "M", "worktree": "M"},
                {"path": "README.md", "index": ".", "worktree": "M"},
                {"path": "notes.txt", "index": "?", "worktree": "?"},
                {"path": "both.rs", "index": "U", "worktree": "U"},
             ]},
            {"path": "vendor/lib", "name": "lib", "kind": "submodule", "parent": "",
             "changes": [{"path": "vendor/lib/x.c", "index": "A", "worktree": "."}]},
            {"path": "vendor/gone", "name": "gone", "kind": "submodule", "uninitialised": true,
             "changes": []},
        ]
    })
}

#[test]
fn a_repositorys_changes_are_split_as_the_web_ui_splits_them() {
    let rs = repos(&status());
    assert_eq!(rs.len(), 3);
    let (staged, changed, unversioned) = sections(&rs[0]);
    let paths = |cs: Vec<&hangar_app::commit::Change>| {
        cs.into_iter().map(|c| c.path.clone()).collect::<Vec<_>>()
    };
    assert_eq!(paths(staged), ["src/main.rs", "src/lib.rs"]);
    assert_eq!(paths(changed), ["src/lib.rs", "README.md", "both.rs"]);
    assert_eq!(paths(unversioned), ["notes.txt"]);
    assert_eq!(rs[0].branch.as_deref(), Some("main"));
    assert_eq!(rs[0].ahead, Some(1));
    assert!(rs[2].uninitialised);
}

#[test]
fn a_submodules_paths_are_shown_from_its_own_folder() {
    let rs = repos(&status());
    assert_eq!(within(&rs[1], "vendor/lib/x.c"), "x.c");
    assert_eq!(within(&rs[0], "src/main.rs"), "src/main.rs");
}

#[test]
fn a_submodule_is_committed_before_the_repository_naming_it() {
    let projects = vec![("/w/app".to_string(), repos(&status()))];
    let order: Vec<String> = commits(&projects, false)
        .unwrap()
        .into_iter()
        .map(|t| t.repo)
        .collect();
    assert_eq!(order, ["vendor/lib", ""]);
}

#[test]
fn commits_span_every_project_with_something_staged() {
    let staged = |name: &str| Repo {
        path: String::new(),
        name: name.into(),
        changes: vec![hangar_app::commit::Change {
            path: "f".into(),
            index: "M".into(),
            worktree: ".".into(),
            from: None,
            submodule: false,
        }],
        ..Default::default()
    };
    let clean = Repo {
        name: "clean".into(),
        ..Default::default()
    };
    let projects = vec![
        ("/w/a".to_string(), vec![staged("a")]),
        ("/w/b".to_string(), vec![clean.clone()]),
        ("/w/c".to_string(), vec![staged("c")]),
    ];
    let roots: Vec<String> = commits(&projects, false)
        .unwrap()
        .into_iter()
        .map(|t| t.root)
        .collect();
    assert_eq!(roots, ["/w/a", "/w/c"]);

    // Two repositories staged: which to amend is not clear.
    assert_eq!(amendable(&projects), None);
    assert!(commits(&projects, true).is_err());

    // One staged: that one.
    let one = vec![
        ("/w/a".to_string(), vec![staged("a")]),
        ("/w/b".to_string(), vec![clean.clone()]),
    ];
    assert_eq!(
        amendable(&one),
        Some(Target {
            root: "/w/a".into(),
            repo: String::new(),
            name: "a".into()
        })
    );

    // Nothing staged, and only one repository: that one.
    let alone = vec![("/w/b".to_string(), vec![clean])];
    assert_eq!(amendable(&alone).map(|t| t.name), Some("clean".into()));
}

#[test]
fn a_repository_two_projects_hold_is_committed_once() {
    let ledger = repos(&json!({"repos": [{"path": "", "name": "ledger",
        "changes": [{"path": "a", "index": "M", "worktree": "."}]}]}));
    let home = repos(
        &json!({"repos": [{"path": "ledger", "name": "ledger", "kind": "clone",
        "changes": [{"path": "ledger/a", "index": "M", "worktree": "."}]}]}),
    );
    let projects = vec![
        ("/home/dev/ledger".to_string(), ledger),
        ("/home/dev".to_string(), home),
    ];
    let targets = commits(&projects, false).unwrap();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].root, "/home/dev/ledger");
    assert_eq!(
        amendable(&projects).map(|t| t.root),
        Some("/home/dev/ledger".into())
    );
}

mod window {
    //! The Commit window's menu on a change, through a stand-in Code
    //! service: staging, rolling back (asked first) and ignoring.

    use std::net::TcpListener;
    use std::sync::{Arc, mpsc};
    use std::thread;
    use std::time::{Duration, Instant};

    use gpui::{AppContext, Modifiers, MouseButton, TestAppContext, VisualTestContext};
    use gpui_component::{Root, WindowExt};
    use hangar_app::commit::CommitView;
    use hangar_app::places::Places;
    use hangar_app::store::MemoryTokens;
    use hangar_app::workspace::Workspace;
    use serde_json::{Value, json};
    use tungstenite::Message;

    /// One project, /home/dev/app, with a changed README.md and an
    /// untracked notes.txt; git calls made are sent to asked.
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
                            "git.status" => json!({
                                "repo": true,
                                "changes": [],
                                "repos": [{"path": "", "name": "app", "kind": "folder", "branch": "main",
                                    "changes": [
                                        {"path": "README.md", "index": ".", "worktree": "M"},
                                        {"path": "notes.txt", "index": "?", "worktree": "?"},
                                    ]}],
                            }),
                            m if m.starts_with("git.") => {
                                asked.send((m.to_string(), params.clone())).unwrap();
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

    fn until(
        cx: &mut VisualTestContext,
        what: &str,
        ready: impl Fn(&mut VisualTestContext) -> bool,
    ) {
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

    fn menu_on(cx: &mut VisualTestContext, row: &'static str, keys: &str) {
        let at = cx.debug_bounds(row).unwrap().center();
        cx.simulate_mouse_down(at, MouseButton::Right, Modifiers::none());
        cx.simulate_mouse_up(at, MouseButton::Right, Modifiers::none());
        cx.run_until_parked();
        cx.simulate_keystrokes(keys);
        cx.run_until_parked();
    }

    #[gpui::test]
    fn a_changes_menu_stages_rolls_back_and_ignores(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        cx.update(gpui_component::init);
        let (asked_tx, asked) = mpsc::channel();
        let url = service(asked_tx);
        let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
        let id = places.update(cx, |p, cx| p.add_server(&url, "hgr_t", cx).unwrap());
        let (_, cx) = cx.add_window_view(|window, cx| {
            let ws = cx.new(|cx| Workspace::new(places.clone(), &id, "e1", "/home/dev", cx));
            let view = cx.new(|cx| CommitView::new(ws, window, cx));
            Root::new(view, window, cx)
        });
        until(cx, "the changes", |cx| {
            cx.debug_bounds("change-w-README.md").is_some()
        });
        let next = |cx: &mut VisualTestContext| {
            let got = std::cell::RefCell::new(None);
            until(cx, "a git call", |_| {
                if got.borrow().is_none() {
                    *got.borrow_mut() = asked.try_recv().ok();
                }
                got.borrow().is_some()
            });
            got.into_inner().unwrap()
        };

        // Show Diff, Jump to Source, then Stage.
        menu_on(cx, "change-w-README.md", "down down down enter");
        let (method, params) = next(cx);
        assert_eq!(
            (method.as_str(), params["path"].as_str()),
            ("git.stage", Some("README.md"))
        );

        // Rollback… asks first.
        menu_on(cx, "change-w-README.md", "down down down down enter");
        assert!(
            cx.update(|window, cx| window.has_active_dialog(cx)),
            "asked first"
        );
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        // An untracked file can be ignored.
        until(cx, "the untracked file", |cx| {
            cx.debug_bounds("change-w-notes.txt").is_some()
        });
        menu_on(cx, "change-w-notes.txt", "down down down down down enter");
        let (method, params) = next(cx);
        assert_eq!(method, "git.ignore");
        assert_eq!(params["path"], "/home/dev/app/notes.txt");
    }
}
