//! New environment: names it can have, a template's variables filled in,
//! and the sheet itself, from ⌘N to the made environment's tab, against a
//! small server speaking just enough HTTP.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;

use gpui::{AppContext, Modifiers, TestAppContext};
use hangar_app::api::Me;
use hangar_app::newenv::{fill, name_problem};
use hangar_app::place::{Connection, Update};
use hangar_app::places::Places;
use hangar_app::shell::Shell;
use hangar_app::store::MemoryTokens;

#[test]
fn a_name_is_a_hostname() {
    assert_eq!(name_problem(""), None);
    assert_eq!(name_problem("rate-limits"), None);
    assert_eq!(name_problem("Rate-1"), None, "either case");
    assert!(name_problem("-rate").is_some());
    assert!(name_problem("rate_1").is_some());
    assert!(name_problem(&"a".repeat(64)).is_some());
}

#[test]
fn a_templates_variables_are_filled_in() {
    assert_eq!(fill("feat/{name}", "Rate-1", "alice", "svc"), "feat/Rate-1");
    assert_eq!(
        fill(
            "{owner}/{name:lower}-{template:upper}",
            "Rate-1",
            "alice",
            "svc"
        ),
        "alice/rate-1-SVC"
    );
    assert_eq!(
        fill("env-{short_id}", "x", "a", "t"),
        "env-{short_id}",
        "the server's"
    );
}

type Seen = Arc<Mutex<Vec<(String, String, String)>>>;

const TEMPLATES: &str = r#"[
  {"id":"t-web","name":"Web app","description":"","owner":"bob","cpus":4,"memory_mib":8192,
   "display":"desktop","gpu":"virtual","image":"img","repos":[]},
  {"id":"t-svc","name":"Rust service","description":"","owner":"alice","cpus":2,"memory_mib":4096,
   "display":"none","gpu":"none","image":"img",
   "repos":[{"url":"https://example.com/acme/svc.git","path":"/workspace/svc","branch":"feat/{name}"}]}
]"#;

/// A server with two templates, which makes whatever it is asked to.
fn server() -> (String, Seen) {
    let ln = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = ln.local_addr().unwrap();
    let seen: Seen = Arc::new(Mutex::new(vec![]));
    let log = seen.clone();
    thread::spawn(move || {
        for conn in ln.incoming() {
            let Ok(mut conn) = conn else { return };
            let log = log.clone();
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
                    let body = String::from_utf8_lossy(&body).to_string();
                    log.lock()
                        .unwrap()
                        .push((method.into(), path.into(), body.clone()));
                    let (status, reply) = match (method, path) {
                        ("GET", "/api/v1/templates") => ("200 OK", TEMPLATES.to_string()),
                        ("POST", "/api/v1/environments") => {
                            let req: serde_json::Value = serde_json::from_str(&body).unwrap();
                            (
                                "201 Created",
                                serde_json::json!({
                                    "id": "e-new", "short_id": "new123", "name": req["name"],
                                    "owner_id": "u1", "owner": "alice", "template": "Rust service",
                                    "template_id": req["template_id"], "phase": "pending",
                                    "desired": "running", "created_at": "2026-11-12T10:00:00Z",
                                    "updated_at": "2026-11-12T10:00:00Z"
                                })
                                .to_string(),
                            )
                        }
                        _ => ("404 Not Found", r#"{"error":"no"}"#.to_string()),
                    };
                    let _ = write!(
                        conn,
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{reply}",
                        reply.len()
                    );
                }
            });
        }
    });
    (format!("http://{addr}"), seen)
}

#[gpui::test]
fn its_key_opens_it_and_create_opens_the_new_environment(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(hangar_app::envtab::bind_keys);
    cx.update(hangar_app::shell::bind_keys);
    cx.update(hangar_app::newenv::bind_keys);
    let (url, seen) = server();
    let places = cx.new(|cx| Places::offline(Arc::new(MemoryTokens::default()), cx));
    let id = places.update(cx, |p, cx| {
        let id = p.add_server(&url, "hgr_t", cx).unwrap();
        p.apply(&id, Update::Connection(Connection::Online), cx);
        p.apply(
            &id,
            Update::Me(Me {
                id: "u1".into(),
                username: "alice".into(),
                display_name: String::new(),
                admin: false,
                ssh: None,
                environment_host_suffix: None,
            }),
            cx,
        );
        p.apply(&id, Update::Snapshot(vec![]), cx);
        id
    });
    let (shell, cx) = cx.add_window_view(|window, cx| Shell::new(places.clone(), window, cx));
    cx.run_until_parked();

    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-n"
    } else {
        "ctrl-n"
    });
    cx.run_until_parked();
    let sheet = shell
        .read_with(cx, |s, _| s.new_env())
        .expect("the sheet open");
    assert_eq!(
        sheet.read_with(cx, |v, _| v.place().map(str::to_string)),
        Some(id.clone()),
        "the place online"
    );
    assert_eq!(
        sheet.read_with(cx, |v, _| v.template().map(str::to_string)),
        Some("t-svc".into()),
        "the user's own template first"
    );

    // The name, which fills in the branch.
    cx.simulate_input("rate-1");
    cx.run_until_parked();
    let create = cx.debug_bounds("new-env-create").expect("Create and Open");
    cx.simulate_click(create.center(), Modifiers::none());
    cx.run_until_parked();

    let posted = seen
        .lock()
        .unwrap()
        .iter()
        .find(|(m, p, _)| m == "POST" && p == "/api/v1/environments")
        .map(|(_, _, b)| b.clone())
        .expect("asked to make it");
    let body: serde_json::Value = serde_json::from_str(&posted).unwrap();
    assert_eq!(
        body,
        serde_json::json!({"template_id": "t-svc", "name": "rate-1"}),
        "the branch filled in from the name is left to the server"
    );
    assert!(shell.read_with(cx, |s, _| s.new_env()).is_none(), "closed");
    assert!(
        shell.read_with(cx, |s, _| s.active_view()).is_some(),
        "its tab open"
    );
}
