//! A port the dev build is driven through by a script (`dev/appctl`): to
//! open an environment and a terminal tab, click, press keys, type,
//! dispatch an action by name, change the settings, take a picture of the
//! window, and count how often its views draw. Built only with the
//! `control` feature (dev/app.sh's builds), and listening only in the dev
//! channel, on the loopback, behind a token: `control.json` in the app's
//! config folder, readable by its user alone, says the port and the token.
//!
//! The protocol is a line of JSON a request, `{"token": …, "cmd": …, …}`,
//! answered by a line of JSON, `{"ok": …}` or `{"error": …}`.

use std::hash::{BuildHasher, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};

use gpui::{
    App, AppContext as _, AsyncApp, Keystroke, Modifiers, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PlatformInput, Window, point, px,
};
use gpui_component::Root;
use serde_json::{Value, json};

use crate::envtab::EnvTab;
use crate::shell::Shell;

type Request = (Value, async_channel::Sender<Value>);

/// Listens for requests, if this is the dev channel's build.
pub fn start(cx: &mut App) {
    if crate::channel::Channel::current() != crate::channel::Channel::Dev {
        return;
    }
    let Some(dirs) = crate::store::Dirs::user() else {
        return;
    };
    let Ok(listener) = TcpListener::bind("127.0.0.1:0") else {
        return;
    };
    let Ok(addr) = listener.local_addr() else {
        return;
    };
    let token = token();
    let file = dirs.config.join("control.json");
    let _ = std::fs::create_dir_all(&dirs.config);
    let info = json!({ "port": addr.port(), "token": token, "pid": std::process::id() });
    if write_private(&file, &info.to_string()).is_err() {
        return;
    }
    let (tx, rx) = async_channel::unbounded::<Request>();
    std::thread::Builder::new()
        .name("control".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let (tx, token) = (tx.clone(), token.clone());
                std::thread::spawn(move || serve(stream, &token, tx));
            }
        })
        .ok();
    cx.spawn(async move |cx: &mut AsyncApp| {
        while let Ok((req, reply)) = rx.recv().await {
            let out = match cx.update(|cx| handle(&req, cx)) {
                Ok(v) => json!({ "ok": v }),
                Err(e) => json!({ "error": e }),
            };
            let _ = reply.send(out).await;
        }
    })
    .detach();
}

/// 128 bits from the OS's randomness, as std's hash keys take it.
fn token() -> String {
    let half = || {
        std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish()
    };
    format!("{:016x}{:016x}", half(), half())
}

fn write_private(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    f.write_all(text.as_bytes())
}

/// Serves one connection: a request a line, each answered once the main
/// thread has run it.
fn serve(stream: TcpStream, token: &str, tx: async_channel::Sender<Request>) {
    let Ok(mut out) = stream.try_clone() else {
        return;
    };
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { return };
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(req) if req["token"].as_str() == Some(token) => {
                let (rtx, rrx) = async_channel::bounded(1);
                if tx.send_blocking((req, rtx)).is_err() {
                    return;
                }
                rrx.recv_blocking()
                    .unwrap_or_else(|_| json!({ "error": "the app stopped" }))
            }
            Ok(_) => json!({ "error": "a wrong token" }),
            Err(e) => json!({ "error": format!("not JSON: {e}") }),
        };
        if writeln!(out, "{reply}").is_err() {
            return;
        }
    }
}

/// The window's shell, if it has one.
fn shell(window: &mut Window, cx: &mut App) -> Option<gpui::Entity<Shell>> {
    let root = window.root::<Root>().flatten()?;
    root.read(cx).view().clone().downcast::<Shell>().ok()
}

/// Runs a request in the first window, the main one.
fn handle(req: &Value, cx: &mut App) -> Result<Value, String> {
    let cmd = req["cmd"].as_str().ok_or("no cmd")?;
    if cmd == "stats" {
        return Ok(crate::renderstats::take());
    }
    if cmd == "settings" {
        let over = req["settings"].clone();
        let mut out = Ok(Value::Null);
        crate::settings::Settings::update(cx, |v| {
            let mut all = serde_json::to_value(&*v).unwrap_or_default();
            merge(&mut all, over);
            match serde_json::from_value(all) {
                Ok(new) => *v = new,
                Err(e) => out = Err(e.to_string()),
            }
        });
        return out;
    }
    let handle = *cx.windows().first().ok_or("no window")?;
    cx.update_window(handle, |_, window, cx| run(cmd, req, window, cx))
        .map_err(|e| e.to_string())?
}

fn run(cmd: &str, req: &Value, window: &mut Window, cx: &mut App) -> Result<Value, String> {
    let num = |k: &str| req[k].as_f64().map(|v| v as f32);
    match cmd {
        "state" => Ok(state(window, cx)),
        "open" => {
            let shell = shell(window, cx).ok_or("no shell")?;
            let env = req["env"].as_str().ok_or("no env")?;
            let (place, id) = find_env(&shell, env, req["place"].as_str(), cx)
                .ok_or_else(|| format!("no environment {env}"))?;
            shell.update(cx, |s, cx| s.open(&place, &id, window, cx));
            Ok(Value::Null)
        }
        "tool" => {
            let tab = active_tab(window, cx)?;
            let name = req["tool"].as_str().ok_or("no tool")?;
            let tool = crate::tools::Tool::ALL
                .into_iter()
                .find(|t| t.name().eq_ignore_ascii_case(name))
                .ok_or_else(|| format!("no tool {name}"))?;
            tab.update(cx, |t, cx| {
                if !t.is_open(tool) {
                    t.toggle(tool, cx)
                }
            });
            Ok(Value::Null)
        }
        "terminal" => {
            let tab = active_tab(window, cx)?;
            let i = req["tab"].as_u64().ok_or("no tab")? as usize;
            tab.update(cx, |t, cx| t.show_terminal_tab(i, window, cx))?;
            Ok(Value::Null)
        }
        "action" => {
            let name = req["name"].as_str().ok_or("no name")?;
            let data = (!req["data"].is_null()).then(|| req["data"].clone());
            let action = cx.build_action(name, data).map_err(|e| e.to_string())?;
            window.dispatch_action(action, cx);
            Ok(Value::Null)
        }
        "click" | "move" => {
            let position = point(px(num("x").ok_or("no x")?), px(num("y").ok_or("no y")?));
            let modifiers = modifiers(req["modifiers"].as_str().unwrap_or(""));
            if cmd == "move" {
                window.dispatch_event(
                    PlatformInput::MouseMove(MouseMoveEvent {
                        position,
                        pressed_button: None,
                        modifiers,
                    }),
                    cx,
                );
                return Ok(Value::Null);
            }
            let button = match req["button"].as_str() {
                Some("right") => MouseButton::Right,
                Some("middle") => MouseButton::Middle,
                _ => MouseButton::Left,
            };
            let count = req["count"].as_u64().unwrap_or(1) as usize;
            for click_count in 1..=count {
                window.dispatch_event(
                    PlatformInput::MouseDown(MouseDownEvent {
                        button,
                        position,
                        modifiers,
                        click_count,
                        first_mouse: false,
                    }),
                    cx,
                );
                window.dispatch_event(
                    PlatformInput::MouseUp(MouseUpEvent {
                        button,
                        position,
                        modifiers,
                        click_count,
                    }),
                    cx,
                );
            }
            Ok(Value::Null)
        }
        "keys" => {
            // As the tests type them: "cmd-k enter".
            for k in req["keys"].as_str().ok_or("no keys")?.split_whitespace() {
                let ks = keystroke(k)?;
                window.dispatch_keystroke(ks, cx);
            }
            Ok(Value::Null)
        }
        "type" => {
            for c in req["text"].as_str().ok_or("no text")?.chars() {
                let key = match c {
                    '\n' => "enter".to_string(),
                    '\t' => "tab".to_string(),
                    ' ' => "space".to_string(),
                    c => c.to_string(),
                };
                let mut ks = keystroke(&key)?;
                if !matches!(c, '\n' | '\t') {
                    ks.key_char = Some(c.to_string());
                }
                window.dispatch_keystroke(ks, cx);
            }
            Ok(Value::Null)
        }
        "screenshot" => {
            let path = req["path"].as_str().ok_or("no path")?;
            window.refresh();
            let _ = window.draw(cx);
            let image = window.render_to_image().map_err(|e| e.to_string())?;
            image.save(path).map_err(|e| e.to_string())?;
            Ok(json!({ "path": path, "width": image.width(), "height": image.height() }))
        }
        "bench" => {
            // Frames drawn on demand, timed: each after the shown terminal
            // asks for one, as its output does ("terminal"), or after the
            // whole window is refreshed, every view drawn afresh ("full").
            let frames = req["frames"].as_u64().unwrap_or(60).clamp(1, 1000) as u32;
            let full = req["mode"].as_str() == Some("full");
            let term = active_tab(window, cx)?.read(cx).active_terminal();
            if !full && term.is_none() {
                return Err("no terminal shown".into());
            }
            let (start, cpu) = (std::time::Instant::now(), thread_cpu());
            for _ in 0..frames {
                match &term {
                    Some(t) if !full => t.update(cx, |_, cx| cx.notify()),
                    _ => window.refresh(),
                }
                let _ = window.draw(cx);
            }
            let ms = start.elapsed().as_secs_f64() * 1000. / frames as f64;
            let cpu_ms = (thread_cpu() - cpu) * 1000. / frames as f64;
            Ok(json!({
                "frames": frames,
                "ms_per_frame": ms,
                "cpu_ms_per_frame": cpu_ms,
                "stats": crate::renderstats::take(),
            }))
        }
        "quit" => {
            cx.quit();
            Ok(Value::Null)
        }
        other => Err(format!("no command {other}")),
    }
}

/// The CPU time this thread has used, in seconds: a frame's cost whatever
/// else the machine is doing.
#[cfg(unix)]
fn thread_cpu() -> f64 {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime writes the timespec given.
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut t) };
    t.tv_sec as f64 + t.tv_nsec as f64 / 1e9
}

/// Elsewhere the thread's CPU time is not read, and frames are timed by
/// the clock alone.
#[cfg(not(unix))]
fn thread_cpu() -> f64 {
    0.
}

/// The environment named (or with the ID) in a place, the one named or
/// the first that has it.
fn find_env(
    shell: &gpui::Entity<Shell>,
    env: &str,
    place: Option<&str>,
    cx: &App,
) -> Option<(String, String)> {
    let places = shell.read(cx).places().read(cx);
    places.list().iter().find_map(|(config, state)| {
        if place.is_some_and(|p| p != config.id && p != config.name) {
            return None;
        }
        let e = state
            .environments
            .iter()
            .find(|e| e.name == env || e.id == env || e.id.starts_with(env))?;
        Some((config.id.clone(), e.id.clone()))
    })
}

fn active_tab(window: &mut Window, cx: &mut App) -> Result<gpui::Entity<EnvTab>, String> {
    let shell = shell(window, cx).ok_or("no shell")?;
    shell
        .read(cx)
        .active_view()
        .ok_or_else(|| "no environment shown".to_string())
}

/// What the window shows: its size, the tabs, and the environment's
/// terminals.
fn state(window: &mut Window, cx: &mut App) -> Value {
    let size = window.viewport_size();
    let mut out = json!({
        "window": { "width": f32::from(size.width), "height": f32::from(size.height),
                    "scale": window.scale_factor(), "active": window.is_window_active() },
    });
    if let Some(shell) = shell(window, cx) {
        let s = shell.read(cx);
        let (tabs, active) = s.tabs();
        out["tabs"] = json!(
            tabs.iter()
                .map(|o| json!({ "place": o.place, "env": o.env }))
                .collect::<Vec<_>>()
        );
        out["active"] = json!(active);
        if let Some(tab) = s.active_view() {
            let (terms, on) = tab.read(cx).terminal_tabs(cx);
            out["terminals"] = json!({ "tabs": terms, "active": on });
            out["tools"] = json!(
                crate::tools::Tool::ALL
                    .into_iter()
                    .filter(|t| tab.read(cx).is_open(*t))
                    .map(|t| t.name())
                    .collect::<Vec<_>>()
            );
        }
    }
    out
}

/// A keystroke as written ("cmd-k"). gpui parses only a string that lives
/// for ever: each one asked for is kept, once.
fn keystroke(s: &str) -> Result<Keystroke, String> {
    static KEPT: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
    let mut kept = KEPT.lock().unwrap();
    let s: &'static str = match kept.iter().find(|k| **k == s) {
        Some(k) => k,
        None => {
            let k: &'static str = Box::leak(s.to_string().into_boxed_str());
            kept.push(k);
            k
        }
    };
    Keystroke::parse(s).map_err(|e| e.to_string())
}

fn modifiers(s: &str) -> Modifiers {
    let mut m = Modifiers::none();
    for part in s.split(['-', '+', ',']) {
        match part {
            "cmd" | "platform" => m.platform = true,
            "ctrl" | "control" => m.control = true,
            "alt" | "option" => m.alt = true,
            "shift" => m.shift = true,
            _ => {}
        }
    }
    m
}

/// Sets what `over` has in `all`, objects merged key by key.
fn merge(all: &mut Value, over: Value) {
    match (all, over) {
        (Value::Object(a), Value::Object(o)) => {
            for (k, v) in o {
                merge(a.entry(k).or_insert(Value::Null), v);
            }
        }
        (a, o) => *a = o,
    }
}
