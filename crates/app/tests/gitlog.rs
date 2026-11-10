use chrono::{Local, TimeZone};
use hangar_app::gitlog::{Commit, Edge, EdgeKind, layout, when};

fn c(hash: &str, parents: &[&str]) -> Commit {
    Commit {
        hash: hash.into(),
        parents: parents.iter().map(|p| p.to_string()).collect(),
        author: "a".into(),
        email: String::new(),
        time: 0.,
        subject: hash.into(),
        refs: vec![],
    }
}

fn kinds(edges: &[Edge]) -> Vec<(EdgeKind, usize, usize)> {
    edges.iter().map(|e| (e.kind, e.from, e.to)).collect()
}

#[test]
fn a_straight_history_is_one_lane() {
    let (rows, lanes) = layout(&[c("c", &["b"]), c("b", &["a"]), c("a", &[])]);
    assert_eq!(lanes, 1);
    assert!(rows.iter().all(|r| r.col == 0 && r.color == 0));
    assert_eq!(kinds(&rows[0].edges), [(EdgeKind::Out, 0, 0)]);
    assert_eq!(
        kinds(&rows[1].edges),
        [(EdgeKind::In, 0, 0), (EdgeKind::Out, 0, 0)]
    );
    assert_eq!(kinds(&rows[2].edges), [(EdgeKind::In, 0, 0)]);
}

#[test]
fn a_merged_branch_takes_a_lane_of_its_own_until_it_forks() {
    // m merges f into main; f and b both come from a.
    let (rows, lanes) = layout(&[
        c("m", &["b", "f"]),
        c("f", &["a"]),
        c("b", &["a"]),
        c("a", &[]),
    ]);
    assert_eq!(lanes, 2);
    assert_eq!(
        kinds(&rows[0].edges),
        [(EdgeKind::Out, 0, 0), (EdgeKind::Out, 0, 1)]
    );
    // f in the second lane, its own colour, its first parent carrying it on.
    assert_eq!((rows[1].col, rows[1].color), (1, 1));
    assert_eq!(
        kinds(&rows[1].edges),
        [
            (EdgeKind::Through, 0, 0),
            (EdgeKind::In, 1, 1),
            (EdgeKind::Out, 1, 1)
        ]
    );
    // b back in the first lane while f's lane passes, both leading to a.
    assert_eq!((rows[2].col, rows[2].color), (0, 0));
    // a: both lanes come in.
    assert_eq!(rows[3].col, 0);
    assert_eq!(
        kinds(&rows[3].edges),
        [(EdgeKind::In, 0, 0), (EdgeKind::In, 1, 0)]
    );
}

#[test]
fn a_second_tip_starts_a_new_colour_in_a_free_lane() {
    let (rows, _) = layout(&[c("x", &["a"]), c("y", &["a"]), c("a", &[])]);
    assert_eq!((rows[0].col, rows[0].color), (0, 0));
    assert_eq!((rows[1].col, rows[1].color), (1, 1));
}

#[test]
fn times_are_shown_by_how_long_ago() {
    let now = Local.with_ymd_and_hms(2026, 11, 8, 15, 0, 0).unwrap();
    let today = Local.with_ymd_and_hms(2026, 11, 8, 9, 5, 0).unwrap();
    let earlier = Local.with_ymd_and_hms(2026, 3, 2, 9, 5, 0).unwrap();
    let last_year = Local.with_ymd_and_hms(2025, 3, 2, 9, 5, 0).unwrap();
    assert_eq!(when(today.timestamp() as f64, now), "09:05");
    assert_eq!(when(earlier.timestamp() as f64, now), "2 Mar");
    assert_eq!(when(last_year.timestamp() as f64, now), "2 Mar 2025");
}

mod window {
    //! The Git window's menu on a commit: its revision number and subject
    //! copied, the commit right-clicked selected.

    use std::net::TcpListener;
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    use gpui::{AppContext, Modifiers, MouseButton, TestAppContext, VisualTestContext};
    use gpui_component::Root;
    use hangar_app::gitlog::GitView;
    use hangar_app::places::Places;
    use hangar_app::store::MemoryTokens;
    use hangar_app::workspace::Workspace;
    use serde_json::{Value, json};
    use tungstenite::Message;

    /// One project with a repository of two commits.
    fn service() -> String {
        let ln = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", ln.local_addr().unwrap());
        thread::spawn(move || {
            for tcp in ln.incoming() {
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
                                "repos": [{"path": "", "name": "app", "kind": "folder", "branch": "main", "changes": []}],
                            }),
                            "git.log" => json!({ "more": false, "commits": [
                                {"hash": "a1b2c3d4e5f6", "parents": ["0f0f0f"], "author": "Ada", "time": 1.7e9, "subject": "Fix the ledger's rounding"},
                                {"hash": "0f0f0f", "parents": [], "author": "Ada", "time": 1.6e9, "subject": "First"},
                            ]}),
                            _ => json!({}),
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

    #[gpui::test]
    fn a_commits_hash_and_subject_are_copied(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        cx.update(gpui_component::init);
        let url = service();
        let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
        let id = places.update(cx, |p, cx| p.add_server(&url, "hgr_t", cx).unwrap());
        let mut view = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let ws = cx.new(|cx| Workspace::new(places.clone(), &id, "e1", "/home/dev", cx));
            let v = cx.new(|cx| GitView::new(ws, cx));
            view = Some(v.clone());
            Root::new(v, window, cx)
        });
        let view = view.unwrap();
        until(cx, "the history", |cx| {
            cx.debug_bounds("commit-0").is_some()
        });
        let menu = |cx: &mut VisualTestContext, keys: &str| {
            let at = cx.debug_bounds("commit-0").unwrap().center();
            cx.simulate_mouse_down(at, MouseButton::Right, Modifiers::none());
            cx.simulate_mouse_up(at, MouseButton::Right, Modifiers::none());
            cx.run_until_parked();
            cx.simulate_keystrokes(keys);
            cx.run_until_parked();
            cx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()))
        };
        assert_eq!(menu(cx, "down enter").as_deref(), Some("a1b2c3d4e5f6"));
        assert_eq!(
            view.read_with(cx, |v, _| v.selected().map(str::to_string))
                .as_deref(),
            Some("a1b2c3d4e5f6"),
            "the commit right-clicked is selected"
        );
        assert_eq!(
            menu(cx, "down down enter").as_deref(),
            Some("Fix the ledger's rounding")
        );
    }
}
