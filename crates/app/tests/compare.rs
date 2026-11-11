//! Comparing a file: with another file or with text, side by side, and
//! the branches and commits it may be compared with, chosen from a list
//! narrowed as one types.

use std::net::TcpListener;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use gpui::{AppContext, TestAppContext, VisualTestContext};
use gpui_component::Root;
use hangar_app::diff::{DiffView, Kind, Other, Row, Spec};
use hangar_app::pick::{Choice, narrowed};
use hangar_app::places::Places;
use hangar_app::project::{branch_choices, revision_choices};
use hangar_app::store::MemoryTokens;
use hangar_app::workspace::Workspace;
use serde_json::{Value, json};
use tungstenite::Message;

#[test]
fn the_branches_and_commits_to_compare_with() {
    let b = json!({
        "current": "main",
        "local": [{"name": "main"}, {"name": "feature/rates"}],
        "remote": ["origin/main"],
    });
    let names: Vec<String> = branch_choices(&b).into_iter().map(|c| c.value).collect();
    assert_eq!(
        names,
        ["feature/rates", "origin/main"],
        "every one but main"
    );

    let l = json!({ "commits": [
        {"hash": "a1b2c3d4e5f6", "author": "Ada", "subject": "Fix the rounding"},
        {"hash": "0f0f0f0f0f0f", "author": "Ada", "subject": ""},
    ]});
    let r = revision_choices(&l);
    assert_eq!(r[0].label, "Fix the rounding");
    assert_eq!(r[0].detail.as_deref(), Some("a1b2c3d4 · Ada"));
    assert_eq!(r[0].value, "a1b2c3d4e5f6");
    assert_eq!(r[1].label, "0f0f0f0f", "no subject: its hash");

    let all = [
        Choice {
            label: "Fix the rounding".into(),
            detail: Some("a1b2c3d4 · Ada".into()),
            value: "a".into(),
        },
        Choice {
            label: "First".into(),
            detail: Some("0f0f0f0f · Grace".into()),
            value: "b".into(),
        },
    ];
    let values = |q: &str| {
        narrowed(&all, q)
            .iter()
            .map(|c| c.value.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(values("round"), ["a"]);
    assert_eq!(values("GRACE"), ["b"], "the detail too, ignoring case");
    assert_eq!(values(" "), ["a", "b"]);
}

/// A Code service in /home/dev/app reading its two files.
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
                    let result = match (method, params["path"].as_str()) {
                        ("hello", _) => json!({ "root": params["root"], "home": "/home/dev" }),
                        ("workspace.get", _) => json!({ "folders": ["/home/dev/app"] }),
                        ("fs.read", Some("/home/dev/app/new.txt")) => {
                            json!({ "text": "one\ntwo\nthree\n" })
                        }
                        ("fs.read", Some("/home/dev/app/old.txt")) => {
                            json!({ "text": "one\n2\nthree\n" })
                        }
                        _ => json!(null),
                    };
                    let mut frame = vec![b'r'];
                    frame.extend(
                        serde_json::to_vec(&json!({ "id": req["id"], "result": result })).unwrap(),
                    );
                    if ws.send(Message::Binary(frame.into())).is_err() {
                        return;
                    }
                }
            });
        }
    });
    url
}

fn until(cx: &mut VisualTestContext, what: &str, ready: impl Fn(&mut VisualTestContext) -> bool) {
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

fn changed(v: &DiffView) -> usize {
    v.rows()
        .iter()
        .filter(|r| {
            matches!(
                r,
                Row::Pair {
                    kind: Kind::Changed,
                    ..
                }
            )
        })
        .count()
}

#[gpui::test]
fn a_file_is_compared_with_another_and_with_text(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(gpui_component::init);
    let url = service();
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let id = places.update(cx, |p, cx| p.add_server(&url, "hgr_t", cx).unwrap());
    let mut ws = None;
    let mut view = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let w = cx.new(|cx| Workspace::new(places.clone(), &id, "e1", "/home/dev", cx));
        let other = Other::File {
            root: "/home/dev/app".into(),
            abs: "/home/dev/app/old.txt".into(),
        };
        let spec = Spec::compare("/home/dev/app", "new.txt", other);
        let v = cx.new(|cx| DiffView::new(w.clone(), spec, cx));
        ws = Some(w);
        view = Some(v.clone());
        Root::new(v, window, cx)
    });
    let (ws, view) = (ws.unwrap(), view.unwrap());
    until(cx, "the comparison", |cx| {
        view.read_with(cx, |v, _| changed(v)) == 1
    });
    assert_eq!(view.read_with(cx, |v, _| v.title()), "old.txt vs new.txt");

    // The same text as the file: shown whole, nothing changed.
    let text = Other::Text("one\ntwo\nthree\n".into());
    let same = cx.update(|_, cx| {
        cx.new(|cx| {
            DiffView::new(
                ws.clone(),
                Spec::compare("/home/dev/app", "new.txt", text),
                cx,
            )
        })
    });
    until(cx, "the clipboard's comparison", |cx| {
        same.read_with(cx, |v, _| v.rows().len()) == 3
    });
    assert_eq!(same.read_with(cx, |v, _| changed(v)), 0);
    assert_eq!(same.read_with(cx, |v, _| v.title()), "Clipboard vs new.txt");
}
