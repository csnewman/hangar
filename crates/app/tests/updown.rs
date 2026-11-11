//! Files between this computer and the environment: files and a folder
//! uploaded a part at a time, a name the folder has asked about; and a
//! folder downloaded as a zip, written where the save panel says.

use std::collections::HashSet;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use gpui::{
    AppContext, Context, IntoElement, Modifiers, Render, TestAppContext, VisualTestContext, Window,
    div,
};
use gpui_component::Root;
use hangar_app::api::Client;
use hangar_app::code::Code;
use hangar_app::updown::{free_name, tops, uploads_of};
use serde_json::{Value, json};
use tungstenite::Message;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hangar-updown-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn what_is_uploaded_and_under_what_name() {
    let d = scratch("walk");
    std::fs::write(d.join("a.txt"), "a").unwrap();
    std::fs::create_dir_all(d.join("site/css")).unwrap();
    std::fs::write(d.join("site/index.html"), "<p>").unwrap();
    std::fs::write(d.join("site/css/main.css"), "p{}").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&d, d.join("site/up")).unwrap();
    let ups = uploads_of(&[d.join("a.txt"), d.join("site")]);
    let rels: Vec<&str> = ups.iter().map(|u| u.rel.as_str()).collect();
    assert_eq!(
        rels,
        ["a.txt", "site/css/main.css", "site/index.html"],
        "a folder's files under its name; a link left out"
    );
    assert_eq!(tops(&ups), ["a.txt", "site"]);

    let taken: HashSet<String> = ["a.txt", "a copy.txt", "notes"].map(String::from).into();
    assert_eq!(free_name(&taken, "a.txt"), "a copy 2.txt");
    assert_eq!(free_name(&taken, "notes"), "notes copy");
    assert_eq!(free_name(&taken, "b.txt"), "b.txt");
    let _ = std::fs::remove_dir_all(d);
}

/// A Code service whose /home/dev/app has a.txt, which says each fs call
/// with its parameters; a zip of anything reads as ZIPDATA.
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
                    let (method, params) = (req["method"].as_str().unwrap(), req["params"].clone());
                    let result = match method {
                        "hello" => json!({ "root": params["root"], "home": "/home/dev" }),
                        "fs.list" => json!([{ "name": "a.txt", "dir": false }]),
                        "fs.uploadStart" => {
                            json!(format!("token:{}", params["path"].as_str().unwrap()))
                        }
                        "fs.uploadEnd" => json!("done"),
                        "fs.zip" => json!({ "path": "/tmp/hangar-zip-1.zip", "size": 7 }),
                        "fs.bytes" => {
                            json!({ "size": 7, "offset": 0, "data": BASE64.encode("ZIPDATA"), "version": "" })
                        }
                        _ => json!(null),
                    };
                    if method.starts_with("fs.") {
                        tx.send((method.to_string(), params)).unwrap();
                    }
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
    (url, asked)
}

struct Blank;

impl Render for Blank {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

fn next(cx: &mut VisualTestContext, asked: &Receiver<(String, Value)>) -> (String, Value) {
    let start = Instant::now();
    loop {
        cx.run_until_parked();
        if let Ok(m) = asked.recv_timeout(Duration::from_millis(20)) {
            return m;
        }
        assert!(start.elapsed() < Duration::from_secs(5), "a call");
    }
}

#[gpui::test]
fn files_are_uploaded_and_a_folder_downloaded(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(gpui_component::init);
    let (url, asked) = service();
    let code = Code::connect(&Client::new(&url, "hgr_t"), "e1", "/home/dev/app", |_| true);
    let (_, cx) = cx.add_window_view(|window, cx| Root::new(cx.new(|_| Blank), window, cx));
    let d = scratch("up");
    std::fs::write(d.join("a.txt"), "hello").unwrap();
    std::fs::write(d.join("b.md"), "# b").unwrap();

    let made = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let seen = made.clone();
    let c = code.clone();
    cx.update(move |window, cx| {
        hangar_app::updown::choose_and_upload(
            c,
            "/home/dev/app".into(),
            false,
            window,
            cx,
            move |m, _, _| *seen.borrow_mut() = m,
        )
    });
    let (a, b) = (d.join("a.txt"), d.join("b.md"));
    cx.simulate_path_prompt_response(move |o| {
        assert!(o.files && !o.directories && o.multiple);
        Some(vec![a, b])
    });
    assert_eq!(next(cx, &asked).0, "fs.list");
    // a.txt is there: both kept, the new one renamed.
    let start = Instant::now();
    while cx.debug_bounds("clash-both").is_none() {
        assert!(start.elapsed() < Duration::from_secs(5), "the question");
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        thread::sleep(Duration::from_millis(10));
    }
    let both = cx.debug_bounds("clash-both").unwrap().center();
    cx.simulate_click(both, Modifiers::none());
    let (m, p) = next(cx, &asked);
    assert_eq!(
        (m.as_str(), p["path"].as_str()),
        ("fs.uploadStart", Some("/home/dev/app/a copy.txt"))
    );
    let (m, p) = next(cx, &asked);
    assert_eq!(m, "fs.uploadWrite");
    assert_eq!(p["data"], BASE64.encode("hello"));
    assert_eq!(next(cx, &asked).0, "fs.uploadEnd");
    let (_, p) = next(cx, &asked);
    assert_eq!(p["path"], "/home/dev/app/b.md", "no clash: its own name");
    next(cx, &asked);
    next(cx, &asked);
    let start = Instant::now();
    while made.borrow().is_empty() {
        assert!(start.elapsed() < Duration::from_secs(5), "done");
        cx.run_until_parked();
    }
    assert_eq!(
        *made.borrow(),
        ["/home/dev/app/a copy.txt", "/home/dev/app/b.md"]
    );

    // A folder downloads as a zip, removed from the environment after.
    let out = d.join("src.zip");
    let c = code.clone();
    cx.update(move |window, cx| {
        hangar_app::updown::download(
            c,
            vec![("/home/dev/app/src".into(), true)],
            "app",
            window,
            cx,
        )
    });
    let to = out.clone();
    cx.simulate_new_path_selection(move |_| Some(to));
    let (m, p) = next(cx, &asked);
    assert_eq!(
        (m.as_str(), p["paths"].clone()),
        ("fs.zip", json!(["/home/dev/app/src"]))
    );
    let (m, p) = next(cx, &asked);
    assert_eq!(
        (m.as_str(), p["path"].as_str()),
        ("fs.bytes", Some("/tmp/hangar-zip-1.zip"))
    );
    let (m, _) = next(cx, &asked);
    assert_eq!(m, "fs.unzip");
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "ZIPDATA");
    let _ = std::fs::remove_dir_all(d);
}
