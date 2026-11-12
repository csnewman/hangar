//! Add a Place: a server signed in through the browser, against a small
//! server speaking just enough HTTP, and one added with a token.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use gpui::{AppContext, TestAppContext};
use hangar_app::addplace::{AddPlaceEvent, AddPlaceView, Step};
use hangar_app::places::Places;
use hangar_app::store::{MemoryTokens, Tokens};

/// The requests a server saw: method, path and body.
type Seen = Arc<Mutex<Vec<(String, String, String)>>>;

/// A server whose sign-in is approved on the third ask, or refused. It
/// returns its address and the requests it saw.
fn server(refuse: bool) -> (String, Seen) {
    let ln = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = ln.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let asks = Arc::new(AtomicUsize::new(0));
    let log = seen.clone();
    thread::spawn(move || {
        for conn in ln.incoming() {
            let Ok(mut conn) = conn else { return };
            let (log, asks) = (log.clone(), asks.clone());
            thread::spawn(move || {
                let mut r = BufReader::new(conn.try_clone().unwrap());
                loop {
                    let mut first = String::new();
                    if r.read_line(&mut first).unwrap_or(0) == 0 {
                        return;
                    }
                    let mut length = 0;
                    loop {
                        let mut h = String::new();
                        r.read_line(&mut h).unwrap();
                        if h.trim().is_empty() {
                            break;
                        }
                        if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                            length = v.trim().parse().unwrap_or(0);
                        }
                    }
                    let mut body = vec![0; length];
                    r.read_exact(&mut body).unwrap();
                    let mut parts = first.split(' ');
                    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
                    log.lock().unwrap().push((
                        method.to_string(),
                        path.to_string(),
                        String::from_utf8_lossy(&body).to_string(),
                    ));
                    let (status, reply) = match path {
                        "/api/v1/version" => ("200 OK", r#"{"versions":["v1"],"server":"0.42"}"#.to_string()),
                        "/api/v1/sign-in" => (
                            "201 Created",
                            r#"{"device_code":"dev-secret","user_code":"QWRT-7H2K","verification_path":"/connect/desktop?code=QWRT-7H2K","expires_at":"2099-01-01T00:00:00Z","interval":1}"#.to_string(),
                        ),
                        "/api/v1/sign-in/token" if refuse => {
                            ("403 Forbidden", r#"{"error":"refused"}"#.to_string())
                        }
                        "/api/v1/sign-in/token" if asks.fetch_add(1, Ordering::SeqCst) < 2 => {
                            ("202 Accepted", r#"{"error":"waiting"}"#.to_string())
                        }
                        "/api/v1/sign-in/token" => (
                            "200 OK",
                            r#"{"token":"hgr_from_browser","name":"Hangar Desktop on test"}"#.to_string(),
                        ),
                        _ => ("404 Not Found", r#"{"error":"no"}"#.to_string()),
                    };
                    write!(
                        conn,
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{reply}",
                        reply.len()
                    )
                    .unwrap();
                }
            });
        }
    });
    (format!("http://{addr}"), seen)
}

fn wait_for(
    cx: &mut gpui::VisualTestContext,
    what: &str,
    ready: impl Fn(&mut gpui::VisualTestContext) -> bool,
) {
    let start = Instant::now();
    while !ready(cx) {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "waiting for {what}"
        );
        // The app asks again on a timer, which a test's clock runs only
        // when told.
        cx.executor().advance_clock(Duration::from_millis(500));
        cx.run_until_parked();
        thread::sleep(Duration::from_millis(20));
    }
}

/// The address is checked, the browser opened at the server's approval
/// page, the code shown, and once approved the place is added with the
/// token the server made, under the name given.
#[gpui::test]
fn a_server_signs_in_through_the_browser(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(gpui_component::init);
    let (url, seen) = server(false);
    let tokens = Arc::new(MemoryTokens::default());
    let places = cx.new(|cx| Places::offline(tokens.clone(), cx));
    let (view, vcx) =
        cx.add_window_view(|window, cx| AddPlaceView::new(places.clone(), window, cx));
    let done = Arc::new(Mutex::new(None));
    let got = done.clone();
    vcx.update(|_, cx| {
        cx.subscribe(&view, move |_, e: &AddPlaceEvent, _| {
            let AddPlaceEvent::Done(id) = e;
            *got.lock().unwrap() = Some(id.clone());
        })
        .detach()
    });
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            v.go(Step::Address, cx);
            let (input, _) = v.inputs();
            input.update(cx, |s, cx| s.set_value(url.clone(), window, cx));
            v.check_address(window, cx);
        })
    });
    wait_for(vcx, "the browser step", |cx| {
        view.read_with(cx, |v, _| v.step() == &Step::Browser)
    });
    assert_eq!(
        view.read_with(vcx, |v, _| v.code().map(str::to_string))
            .as_deref(),
        Some("QWRT-7H2K")
    );
    assert_eq!(
        vcx.opened_url().as_deref(),
        Some(format!("{url}/connect/desktop?code=QWRT-7H2K").as_str())
    );
    wait_for(vcx, "the place added", |_| done.lock().unwrap().is_some());
    let id = done.lock().unwrap().clone().unwrap().expect("a place");
    assert_eq!(tokens.get(&id).as_deref(), Some("hgr_from_browser"));
    places.read_with(vcx, |p, _| {
        assert_eq!(p.get(&id).unwrap().0.name, "127.0.0.1", "named as offered");
    });
    let seen = seen.lock().unwrap();
    let start = seen
        .iter()
        .find(|(_, path, _)| path == "/api/v1/sign-in")
        .unwrap();
    assert!(
        start.2.contains("Hangar Desktop on "),
        "the app named: {}",
        start.2
    );
    let asks = seen
        .iter()
        .filter(|(_, path, _)| path == "/api/v1/sign-in/token")
        .count();
    assert_eq!(asks, 3, "asked until approved");
}

/// A refusal in the browser comes back to the address, saying so.
#[gpui::test]
fn a_refused_sign_in_says_so(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(gpui_component::init);
    let (url, _) = server(true);
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let (view, vcx) =
        cx.add_window_view(|window, cx| AddPlaceView::new(places.clone(), window, cx));
    vcx.update(|window, cx| view.update(cx, |v, cx| v.sign_in(&url, window, cx)));
    wait_for(vcx, "the refusal", |cx| {
        view.read_with(cx, |v, _| v.step() == &Step::Address && v.error().is_some())
    });
    assert_eq!(
        view.read_with(vcx, |v, _| v.error().map(str::to_string))
            .as_deref(),
        Some("The sign-in was refused in the browser.")
    );
    places.read_with(vcx, |p, _| assert!(p.list().is_empty()));
}

/// An address that is not one is said so, before anything is asked.
#[gpui::test]
fn a_bad_address_is_said_so(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let (view, vcx) =
        cx.add_window_view(|window, cx| AddPlaceView::new(places.clone(), window, cx));
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            let (input, _) = v.inputs();
            input.update(cx, |s, cx| s.set_value("not an address", window, cx));
            v.check_address(window, cx);
            assert_eq!(v.error(), Some("That is not a server's address."));
        })
    });
}
