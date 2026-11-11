//! The Log tool window: an environment's log from its worker, its end
//! first and then on from there; another log chosen starts again at its
//! end, and a log not yet written says so.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use gpui::{AppContext, Modifiers, TestAppContext, VisualTestContext};
use gpui_component::Root;
use hangar_app::api::Client;
use hangar_app::envlogs::LogsView;

/// A server whose console log is ten bytes gone, then "boot", "ready",
/// and "more" once read on from 20; the GPU's log is missing. It says each
/// path asked for.
fn server() -> (String, Receiver<String>) {
    let ln = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", ln.local_addr().unwrap());
    let (tx, asked) = channel();
    std::thread::spawn(move || {
        for tcp in ln.incoming() {
            let Ok(mut tcp) = tcp else { return };
            let mut r = BufReader::new(tcp.try_clone().unwrap());
            let mut first = String::new();
            r.read_line(&mut first).unwrap();
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                    break;
                }
            }
            let path = first.split_whitespace().nth(1).unwrap_or("").to_string();
            let body = match path.as_str() {
                "/api/v1/environments/e1/logs/combined" => {
                    r#"{"start":10,"next":21,"size":21,"text":"boot\nready\n"}"#
                }
                "/api/v1/environments/e1/logs/combined?offset=21" => {
                    r#"{"start":21,"next":26,"size":26,"text":"more\n"}"#
                }
                "/api/v1/environments/e1/logs/gpu" => {
                    r#"{"start":0,"next":0,"size":0,"text":"","missing":true}"#
                }
                _ => r#"{"start":0,"next":0,"size":0,"text":""}"#,
            };
            tx.send(path).unwrap();
            let _ = write!(
                tcp,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (url, asked)
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
fn a_log_is_read_from_its_end_and_on(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(gpui_component::init);
    let (url, asked) = server();
    let client = Client::new(&url, "hgr_t");
    let mut view = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let v = cx.new(|cx| LogsView::new(client, "e1", cx));
        view = Some(v.clone());
        Root::new(v, window, cx)
    });
    let view = view.unwrap();
    let text =
        |cx: &mut VisualTestContext| view.read_with(cx, |v, cx| v.pane().read(cx).lines().text());
    // The test's executor runs the reads only while it is let run.
    let next = |cx: &mut VisualTestContext| {
        let start = Instant::now();
        loop {
            cx.run_until_parked();
            if let Ok(path) = asked.recv_timeout(Duration::from_millis(20)) {
                return path;
            }
            assert!(start.elapsed() < Duration::from_secs(5), "a request");
        }
    };

    assert_eq!(next(cx), "/api/v1/environments/e1/logs/combined", "the end");
    until(cx, "its end", |cx| text(cx) == "boot\nready\n");
    assert!(view.read_with(cx, |v, _| v.earlier()), "its start is gone");

    view.update(cx, |v, cx| v.read(cx));
    assert_eq!(next(cx), "/api/v1/environments/e1/logs/combined?offset=21");
    until(cx, "what followed", |cx| text(cx) == "boot\nready\nmore\n");

    let gpu = cx.debug_bounds("log-gpu").unwrap().center();
    cx.simulate_click(gpu, Modifiers::none());
    assert_eq!(next(cx), "/api/v1/environments/e1/logs/gpu", "from its end");
    until(cx, "no such log", |cx| {
        view.read_with(cx, |v, cx| {
            v.chosen() == "gpu"
                && v.pane().read(cx).lines().is_empty()
                && v.pane().read(cx).empty() == "There is no such log here yet."
        })
    });
}
