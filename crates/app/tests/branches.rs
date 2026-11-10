//! The branch widget, as IntelliJ's: the branch a repository is on; its
//! pop-up lists the actions and the branches, typing narrows them, and
//! Enter checks a branch out or makes one.

use std::net::TcpListener;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::{Duration, Instant};

use gpui::{AppContext, Modifiers, TestAppContext, VisualTestContext};
use gpui_component::Root;
use hangar_app::branches::{Branch, BranchWidget, Branches, Do, OpenCommit, Row, first, rows};
use hangar_app::places::Places;
use hangar_app::store::MemoryTokens;
use hangar_app::workspace::Workspace;
use serde_json::{Value, json};
use tungstenite::Message;

fn branches() -> Branches {
    let local = |name: &str, ahead: Option<i64>| Branch {
        name: name.into(),
        upstream: None,
        ahead,
        behind: None,
    };
    Branches {
        current: Some("main".into()),
        local: vec![local("main", Some(2)), local("feature/rates", None)],
        remote: vec!["origin/main".into(), "origin/fix/rounding".into()],
    }
}

fn labels(rows: &[Row]) -> Vec<String> {
    rows.iter()
        .map(|r| match r {
            Row::Heading(h) => format!("# {h}"),
            Row::Item {
                label,
                hint,
                current,
                ..
            } => format!(
                "{}{label}{}",
                if *current { "✓ " } else { "" },
                hint.as_ref().map(|h| format!(" {h}")).unwrap_or_default()
            ),
            Row::More(n) => format!("+{n}"),
        })
        .collect()
}

#[test]
fn the_actions_then_local_and_remote_branches() {
    let r = rows("", &branches());
    assert_eq!(
        labels(&r),
        [
            "Update Project",
            "Commit…",
            "Push",
            "New Branch…",
            "# Local",
            "✓ main ↑2",
            "feature/rates",
            "# Remote",
            "origin/main",
            "origin/fix/rounding",
        ]
    );
    assert_eq!(
        first(&r, ""),
        Some(0),
        "the first action, with nothing typed"
    );
}

#[test]
fn typing_narrows_and_names_a_new_branch() {
    let r = rows("rat", &branches());
    assert_eq!(
        labels(&r),
        ["New Branch “rat”", "# Local", "feature/rates"],
        "the actions that match, and the branches"
    );
    let at = first(&r, "rat").unwrap();
    assert_eq!(
        r[at].does(),
        Some(&Do::Checkout("feature/rates".into())),
        "Enter takes the branch found, not a new one"
    );

    let r = rows("main", &branches());
    assert!(
        !labels(&r).iter().any(|l| l.starts_with("New Branch")),
        "no new branch with a local one's name"
    );

    let many = Branches {
        current: None,
        local: (0..12)
            .map(|i| Branch {
                name: format!("b{i}"),
                upstream: None,
                ahead: None,
                behind: None,
            })
            .collect(),
        remote: vec![],
    };
    assert_eq!(rows("", &many).last(), Some(&Row::More(4)));
}

/// A Code service with one repository on main, which says each git method
/// asked of it, with its parameters.
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
                while let Ok(Message::Binary(b)) = ws.read() {
                    let req: Value = serde_json::from_slice(&b[1..]).unwrap();
                    let (method, params) = (req["method"].as_str().unwrap(), &req["params"]);
                    let result = match method {
                        "hello" => json!({ "root": params["root"], "home": "/home/dev" }),
                        "workspace.get" => json!({ "folders": ["/home/dev/app"] }),
                        "git.status" => json!({
                            "repo": true, "changes": [],
                            "repos": [{"path": "", "name": "app", "kind": "folder", "branch": "main", "ahead": 2, "changes": []}],
                        }),
                        "git.branches" => json!({
                            "current": "main",
                            "local": [{"name": "main", "ahead": 2}, {"name": "feature/rates"}],
                            "remote": ["origin/main"],
                        }),
                        _ => json!(null),
                    };
                    if method.starts_with("git.") && method != "git.status" {
                        let _ = tx.send((method.to_string(), params.clone()));
                    }
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
    (url, asked)
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
fn a_branch_is_checked_out_and_one_made(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(gpui_component::init);
    let (url, asked) = service();
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let id = places.update(cx, |p, cx| p.add_server(&url, "hgr_t", cx).unwrap());
    let mut widget = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let ws = cx.new(|cx| Workspace::new(places.clone(), &id, "e1", "/home/dev", cx));
        let w = cx.new(|cx| BranchWidget::new(ws, cx));
        widget = Some(w.clone());
        Root::new(w, window, cx)
    });
    let widget = widget.unwrap();
    let commits = std::rc::Rc::new(std::cell::Cell::new(0));
    let seen = commits.clone();
    cx.update(|_, cx| {
        cx.subscribe(&widget, move |_, _: &OpenCommit, _| {
            seen.set(seen.get() + 1)
        })
        .detach()
    });
    until(cx, "the branch", |cx| {
        cx.debug_bounds("branch-widget").is_some()
    });
    let next = || asked.recv_timeout(Duration::from_secs(5)).unwrap();
    let open = |cx: &mut VisualTestContext| {
        let at = cx.debug_bounds("branch-widget").unwrap().center();
        cx.simulate_click(at, Modifiers::none());
        until(cx, "the pop-up's branches", |cx| {
            cx.debug_bounds("branch-row-feature/rates").is_some()
        });
    };

    open(cx);
    assert_eq!(next().0, "git.branches");
    cx.simulate_input("rat");
    cx.simulate_keystrokes("enter");
    let (method, params) = next();
    assert_eq!(method, "git.checkout");
    assert_eq!(params["name"], "feature/rates");
    until(cx, "the pop-up closes", |cx| {
        cx.debug_bounds("branch-popup").is_none()
    });

    open(cx);
    assert_eq!(next().0, "git.branches");
    cx.simulate_input("fix/limits");
    cx.simulate_keystrokes("enter");
    let (method, params) = next();
    assert_eq!(method, "git.createBranch", "nothing found: a new branch");
    assert_eq!(params["name"], "fix/limits");

    // Commit… asks for the Commit window: the second action.
    open(cx);
    assert_eq!(next().0, "git.branches");
    cx.simulate_keystrokes("down enter");
    until(cx, "Commit… is asked for", |_| commits.get() == 1);

    // Push, by a click.
    open(cx);
    assert_eq!(next().0, "git.branches");
    let push = cx.debug_bounds("branch-row-Push").unwrap().center();
    cx.simulate_click(push, Modifiers::none());
    assert_eq!(next().0, "git.push");
}
