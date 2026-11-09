//! A picture of the app: opens its window on the places it would open, waits
//! for them, renders the window to a PNG (through the GPU, without the
//! screen) and exits. For checking how it looks where a screen cannot be
//! recorded.
//!
//!   HANGAR_URL=… HANGAR_TOKEN=… cargo run -p hangar-app --example snapshot -- OUT.png
//!
//! SNAPSHOT_THEME=dark draws it dark; SNAPSHOT_OPEN=<environment name> opens
//! that environment's tab; SNAPSHOT_HOME=place groups Home by place;
//! SNAPSHOT_SIZE=WxH sets the window's size; SNAPSHOT_WAIT=seconds waits
//! after opening, SNAPSHOT_TYPE types into its terminal (\\n for Enter), and
//! SNAPSHOT_ANSWER=AGENT:KEY answers an asking agent session;
//! SNAPSHOT_TOOLS=structure,ports toggles tool windows; SNAPSHOT_EXPAND=ROOT:PATH
//! opens a project's folders; SNAPSHOT_FILE=ROOT:PATH opens a file (PATH,PATH... several), and
//! SNAPSHOT_EDIT types at its start, then SNAPSHOT_KEYS a key at a time; SNAPSHOT_STAGE=ROOT:PATH stages a file
//! from the Commit window and SNAPSHOT_COMMIT=message commits what is staged;
//! SNAPSHOT_BRANCH=1 shows the Commit window's Branch view;
//! SNAPSHOT_SEARCH=text opens Search Everywhere and types it;
//! SNAPSHOT_NOTICES=1 opens the bell's notifications;
//! SNAPSHOT_ASK=METHOD:LINE:COLUMN asks the open file's language server
//! (textDocument/hover, textDocument/definition) and prints its answer;
//! SNAPSHOT_FIND=text searches in files in Find in Files' pop-up,
//! SNAPSHOT_FIND_SELECT=N chooses its Nth match, SNAPSHOT_FIND_OPEN=N opens it,
//! and SNAPSHOT_FIND_WINDOW=1 keeps the matches in the Find window;
//! SNAPSHOT_REPLACE=1 shows the pop-up as Replace in Files;
//! SNAPSHOT_CARET=LINE:COL puts the open file's caret there, then
//! SNAPSHOT_USAGES=1 finds its usages, SNAPSHOT_RENAME=name renames it, and
//! SNAPSHOT_SYMBOL=text goes to a symbol;
//! SNAPSHOT_DIFF=ROOT:PATH shows a file's changes; SNAPSHOT_GIT=N chooses the
//! Git window's Nth commit and SNAPSHOT_GIT_FILE=N shows its Nth file's diff;
//! SNAPSHOT_HOVER=X,Y rests the pointer there;
//! SNAPSHOT_RIGHT_CLICK=X,Y right-clicks there, as the pointer would, and
//! SNAPSHOT_PRESS="down enter |text| enter" then presses keys, typing what
//! is between bars;
//! SNAPSHOT_HOLD=seconds waits before the picture is taken.

use std::time::Duration;

use gpui::{AppContext, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_component::{Root, ThemeMode, TitleBar};
use gpui_platform::application;
use hangar_app::envtab::Tool;
use hangar_app::places;
use hangar_app::shell::{HomeMode, Shell};

fn main() {
    let out = std::env::args().nth(1).expect("usage: snapshot OUT.png");
    let (w, h) = std::env::var("SNAPSHOT_SIZE")
        .ok()
        .and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        })
        .unwrap_or((1280., 800.));
    application()
        .with_assets(hangar_app::assets::Assets)
        .run(move |cx| {
            gpui_component::init(cx);
            hangar_app::envtab::bind_keys(cx);
            hangar_app::project::bind_keys(cx);
            hangar_app::docedit::bind_keys(cx);
            hangar_app::shell::bind_keys(cx);
            hangar_app::termview::bind_keys(cx);
            // A picture, not an app to switch to: no Dock icon or menu bar.
            cx.set_activation_policy(gpui::ActivationPolicy::Accessory);
            // The user's layout and settings, as the app has them, copied:
            // what a picture toggles stays out of the app's own.
            let scratch =
                std::env::temp_dir().join(format!("hangar-snapshot-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&scratch);
            let copy = |from: Option<std::path::PathBuf>, name: &str| {
                let to = scratch.join(name);
                if let Some(from) = from {
                    let _ = std::fs::copy(from, &to);
                }
                Some(to)
            };
            let dirs = hangar_app::store::Dirs::user();
            hangar_app::layout::Layout::init(
                copy(dirs.as_ref().map(|d| d.layout_file()), "layout.json"),
                cx,
            );
            hangar_app::settings::Settings::init(
                copy(dirs.as_ref().map(|d| d.settings_file()), "settings.json"),
                cx,
            );
            let places = places::from_env(cx);
            let bounds = Bounds::centered(None, size(px(w), px(h)), cx);
            let mut shell = None;
            let handle = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(bounds)),
                        ..TitleBar::window_options()
                    },
                    |window, cx| {
                        let mode = match std::env::var("SNAPSHOT_THEME").as_deref() {
                            Ok("dark") => ThemeMode::Dark,
                            _ => ThemeMode::Light,
                        };
                        hangar_app::palette::force(mode, cx);
                        let s = cx.new(|cx| Shell::new(places.clone(), window, cx));
                        shell = Some(s.clone());
                        cx.new(|cx| Root::new(s, window, cx))
                    },
                )
                .expect("opening the window");
            let shell = shell.unwrap();
            cx.spawn(async move |cx| {
                settle(cx, handle.into(), Duration::from_secs(3)).await;
                if std::env::var("SNAPSHOT_HOME").as_deref() == Ok("place") {
                    let _ = handle.update(cx, |_, _, cx| {
                        shell.update(cx, |s, cx| s.set_home_mode(HomeMode::Place, cx))
                    });
                }
                if let Ok(name) = std::env::var("SNAPSHOT_OPEN") {
                    let _ = handle.update(cx, |_, window, cx| {
                        let found = places.read(cx).list().iter().find_map(|(p, s)| {
                            s.environments
                                .iter()
                                .find(|e| e.name == name)
                                .map(|e| (p.id.clone(), e.id.clone()))
                        });
                        if let Some((p, e)) = found {
                            shell.update(cx, |s, cx| s.open(&p, &e, window, cx));
                        }
                    });
                }
                if let Ok(tools) = std::env::var("SNAPSHOT_TOOLS") {
                    let _ = handle.update(cx, |_, _, cx| {
                        if let Some(tab) = shell.read(cx).active_view() {
                            tab.update(cx, |t, cx| {
                                for name in tools.split(',') {
                                    if let Some(tool) = Tool::by_name(name.trim()) {
                                        t.toggle(tool, cx);
                                    }
                                }
                            });
                        }
                    });
                }
                // Time for what was opened to connect, then what to type.
                let wait = std::env::var("SNAPSHOT_WAIT")
                    .ok()
                    .and_then(|w| w.parse::<f64>().ok())
                    .unwrap_or(0.);
                settle(cx, handle.into(), Duration::from_secs_f64(wait)).await;
                if let Some((root, rel)) = std::env::var("SNAPSHOT_EXPAND").ok().and_then(|e| {
                    e.split_once(':')
                        .map(|(a, b)| (a.to_string(), b.to_string()))
                }) {
                    let _ = handle.update(cx, |_, _, cx| {
                        let project = shell
                            .read(cx)
                            .active_view()
                            .and_then(|t| t.read(cx).project());
                        if let Some(p) = project {
                            p.update(cx, |p, cx| p.expand(&root, &rel, cx));
                        }
                    });
                    settle(cx, handle.into(), Duration::from_secs(1)).await;
                }
                if let Some((root, path)) = std::env::var("SNAPSHOT_FILE").ok().and_then(|e| {
                    e.split_once(':')
                        .map(|(a, b)| (a.to_string(), b.to_string()))
                }) {
                    let _ = handle.update(cx, |_, window, cx| {
                        if let Some(tab) = shell.read(cx).active_view() {
                            tab.update(cx, |t, cx| {
                                for path in path.split(',') {
                                    t.open_file(&root, path, window, cx)
                                }
                            });
                        }
                    });
                    settle(cx, handle.into(), Duration::from_secs(2)).await;
                    if let Ok(text) = std::env::var("SNAPSHOT_EDIT") {
                        let _ = handle.update(cx, |_, window, cx| {
                            let file = shell
                                .read(cx)
                                .active_view()
                                .and_then(|t| t.read(cx).active_file());
                            if let Some(f) = file {
                                let editor = f.read(cx).editor().clone();
                                editor.update(cx, |e, cx| {
                                    use gpui::EntityInputHandler;
                                    e.replace_text_in_range(
                                        Some(0..0),
                                        &text.replace("\\n", "\n"),
                                        window,
                                        cx,
                                    )
                                });
                            }
                        });
                        settle(cx, handle.into(), Duration::from_secs(2)).await;
                    }
                    // Typed a character at a time at the cursor, as keys are.
                    if let Ok(keys) = std::env::var("SNAPSHOT_KEYS") {
                        for c in keys.chars() {
                            let _ = handle.update(cx, |_, window, cx| {
                                let file = shell
                                    .read(cx)
                                    .active_view()
                                    .and_then(|t| t.read(cx).active_file());
                                if let Some(f) = file {
                                    let editor = f.read(cx).editor().clone();
                                    editor.update(cx, |e, cx| {
                                        use gpui::EntityInputHandler;
                                        e.replace_text_in_range(None, &c.to_string(), window, cx)
                                    });
                                }
                            });
                            settle(cx, handle.into(), Duration::from_millis(300)).await;
                        }
                    }
                }
                if let Some((root, path)) = std::env::var("SNAPSHOT_STAGE").ok().and_then(|e| {
                    e.split_once(':')
                        .map(|(a, b)| (a.to_string(), b.to_string()))
                }) {
                    let _ = handle.update(cx, |_, _, cx| {
                        let commit = shell
                            .read(cx)
                            .active_view()
                            .and_then(|t| t.read(cx).commit_view());
                        if let Some(c) = commit {
                            c.update(cx, |c, cx| c.stage(&root, &path, cx));
                        }
                    });
                    settle(cx, handle.into(), Duration::from_secs(2)).await;
                }
                if let Ok(message) = std::env::var("SNAPSHOT_COMMIT") {
                    let _ = handle.update(cx, |_, window, cx| {
                        let commit = shell
                            .read(cx)
                            .active_view()
                            .and_then(|t| t.read(cx).commit_view());
                        if let Some(c) = commit {
                            c.update(cx, |c, cx| {
                                c.message()
                                    .clone()
                                    .update(cx, |m, cx| m.set_value(message, window, cx));
                                c.commit(false, window, cx);
                            });
                        }
                    });
                    settle(cx, handle.into(), Duration::from_secs(3)).await;
                }
                if let Some((root, path)) = std::env::var("SNAPSHOT_DIFF").ok().and_then(|e| {
                    e.split_once(':')
                        .map(|(a, b)| (a.to_string(), b.to_string()))
                }) {
                    let _ = handle.update(cx, |_, window, cx| {
                        if let Some(tab) = shell.read(cx).active_view() {
                            tab.update(cx, |t, cx| {
                                t.open_diff(
                                    hangar_app::diff::Spec::working(&root, &path),
                                    window,
                                    cx,
                                )
                            });
                        }
                    });
                    settle(cx, handle.into(), Duration::from_secs(2)).await;
                }
                if let Ok(text) = std::env::var("SNAPSHOT_FIND") {
                    let _ = handle.update(cx, |_, window, cx| {
                        if let Some(tab) = shell.read(cx).active_view() {
                            tab.update(cx, |t, cx| t.find_in_files(window, cx));
                        }
                    });
                    settle(cx, handle.into(), Duration::from_secs(1)).await;
                    let popup = |cx: &mut gpui::App| {
                        shell
                            .read(cx)
                            .active_view()
                            .and_then(|t| t.read(cx).find_popup())
                    };
                    let _ = handle.update(cx, |_, window, cx| {
                        if let Some(f) = popup(cx) {
                            f.update(cx, |f, cx| f.set_query(&text, window, cx));
                        }
                    });
                    settle(cx, handle.into(), Duration::from_secs(3)).await;
                    if std::env::var("SNAPSHOT_REPLACE").is_ok() {
                        let _ = handle.update(cx, |_, _, cx| {
                            if let Some(f) = popup(cx) {
                                f.update(cx, |f, cx| f.set_replacing(true, cx));
                            }
                        });
                    }
                    let n = |k| std::env::var(k).ok().and_then(|n| n.parse::<usize>().ok());
                    if let Some(n) = n("SNAPSHOT_FIND_SELECT") {
                        let _ = handle.update(cx, |_, _, cx| {
                            if let Some(f) = popup(cx) {
                                f.update(cx, |f, cx| f.select(n, cx));
                            }
                        });
                        settle(cx, handle.into(), Duration::from_secs(2)).await;
                    }
                    if let Some(n) = n("SNAPSHOT_FIND_OPEN") {
                        let _ = handle.update(cx, |_, _, cx| {
                            if let Some(f) = popup(cx) {
                                f.update(cx, |f, cx| f.open(n, cx));
                            }
                        });
                        settle(cx, handle.into(), Duration::from_secs(3)).await;
                    }
                    if std::env::var("SNAPSHOT_FIND_WINDOW").is_ok() {
                        let _ = handle.update(cx, |_, _, cx| {
                            if let Some(f) = popup(cx) {
                                f.update(cx, |f, cx| f.to_window(cx));
                            }
                        });
                        settle(cx, handle.into(), Duration::from_secs(3)).await;
                    }
                }
                // The language features, at a place in the file open.
                if let Ok(at) = std::env::var("SNAPSHOT_CARET") {
                    let (l, c) = hangar_app::docedit::line_column(&at).expect("LINE:COL");
                    let _ = handle.update(cx, |_, window, cx| {
                        if let Some(doc) = shell
                            .read(cx)
                            .active_view()
                            .and_then(|t| t.read(cx).active_file())
                        {
                            doc.update(cx, |d, cx| d.go_to(l - 1, c - 1, window, cx));
                        }
                    });
                    settle(cx, handle.into(), Duration::from_secs(1)).await;
                }
                if std::env::var("SNAPSHOT_USAGES").is_ok() {
                    let _ = handle.update(cx, |_, window, cx| {
                        if let Some(tab) = shell.read(cx).active_view() {
                            tab.update(cx, |t, cx| t.find_usages(window, cx));
                        }
                    });
                    settle(cx, handle.into(), Duration::from_secs(5)).await;
                }
                if let Ok(name) = std::env::var("SNAPSHOT_RENAME") {
                    let _ = handle.update(cx, |_, window, cx| {
                        if let Some(tab) = shell.read(cx).active_view() {
                            tab.update(cx, |t, cx| t.rename_to(name, window, cx));
                        }
                    });
                    settle(cx, handle.into(), Duration::from_secs(5)).await;
                }
                if let Ok(query) = std::env::var("SNAPSHOT_SYMBOL") {
                    let _ = handle.update(cx, |_, window, cx| {
                        shell.update(cx, |s, cx| s.go_to_symbol(window, cx));
                        if let Some(search) = shell.read(cx).search() {
                            search.update(cx, |s, cx| s.set_query(&query, window, cx));
                        }
                    });
                    settle(cx, handle.into(), Duration::from_secs(4)).await;
                }
                if std::env::var("SNAPSHOT_BRANCH").is_ok() {
                    let _ = handle.update(cx, |_, _, cx| {
                        let commit = shell
                            .read(cx)
                            .active_view()
                            .and_then(|t| t.read(cx).commit_view());
                        if let Some(c) = commit {
                            c.update(cx, |c, cx| c.set_branch_mode(true, cx));
                        }
                    });
                    settle(cx, handle.into(), Duration::from_secs(2)).await;
                }
                if let Some(n) = std::env::var("SNAPSHOT_GIT")
                    .ok()
                    .and_then(|n| n.parse::<usize>().ok())
                {
                    let _ = handle.update(cx, |_, _, cx| {
                        let git = shell
                            .read(cx)
                            .active_view()
                            .and_then(|t| t.read(cx).git_view());
                        if let Some(g) = git {
                            g.update(cx, |g, cx| {
                                if let Some(c) = g.commits().get(n) {
                                    let hash = c.hash.clone();
                                    g.select(hash, cx);
                                }
                            });
                        }
                    });
                    settle(cx, handle.into(), Duration::from_secs(1)).await;
                }
                if let Some(n) = std::env::var("SNAPSHOT_GIT_FILE")
                    .ok()
                    .and_then(|n| n.parse::<usize>().ok())
                {
                    let _ = handle.update(cx, |_, _, cx| {
                        let git = shell
                            .read(cx)
                            .active_view()
                            .and_then(|t| t.read(cx).git_view());
                        if let Some(g) = git {
                            g.update(cx, |g, cx| g.diff_file(n, cx));
                        }
                    });
                    settle(cx, handle.into(), Duration::from_secs(2)).await;
                }
                if let Some((method, line, column)) =
                    std::env::var("SNAPSHOT_ASK").ok().and_then(|a| {
                        let mut p = a.split(':');
                        Some((
                            p.next()?.to_string(),
                            p.next()?.parse::<u32>().ok()?,
                            p.next()?.parse::<u32>().ok()?,
                        ))
                    })
                {
                    let answer = handle.update(cx, |_, _, cx| {
                        let tab = shell.read(cx).active_view()?;
                        let abs = tab.read(cx).active_file()?.read(cx).path().to_string();
                        let langs = tab.read(cx).languages()?;
                        let params = serde_json::json!({
                            "textDocument": { "uri": hangar_app::languages::file_uri(&abs) },
                            "position": { "line": line, "character": column },
                        });
                        Some(langs.update(cx, |l, cx| l.request(&abs, &method, params, cx)))
                    });
                    match answer {
                        Ok(Some(answer)) => eprintln!("answer: {:?}", answer.await),
                        _ => eprintln!("answer: no open file, or no language servers"),
                    }
                }
                if let Ok(text) = std::env::var("SNAPSHOT_TYPE") {
                    let text = text.replace("\\n", "\r");
                    let _ = handle.update(cx, |_, _, cx| {
                        if let Some(tab) = shell.read(cx).active_view() {
                            tab.update(cx, |t, cx| t.type_in_terminal(&text, cx));
                        }
                    });
                    settle(cx, handle.into(), Duration::from_secs_f64(wait.max(1.))).await;
                }
                if let Some((agent, key)) = std::env::var("SNAPSHOT_ANSWER").ok().and_then(|a| {
                    a.split_once(':')
                        .map(|(a, k)| (a.to_string(), k.to_string()))
                }) {
                    let _ = handle.update(cx, |_, window, cx| {
                        if let Some(tab) = shell.read(cx).active_view() {
                            tab.update(cx, |t, cx| t.answer(&agent, &key, window, cx));
                        }
                    });
                    settle(cx, handle.into(), Duration::from_secs(2)).await;
                }
                if std::env::var("SNAPSHOT_NOTICES").is_ok() {
                    let _ = handle.update(cx, |_, _, cx| {
                        shell.update(cx, |s, cx| s.toggle_notices(cx));
                    });
                    settle(cx, handle.into(), Duration::from_secs(1)).await;
                }
                if let Ok(text) = std::env::var("SNAPSHOT_SEARCH") {
                    let _ = handle.update(cx, |_, window, cx| {
                        shell.update(cx, |s, cx| s.search_everywhere(window, cx));
                        if let Some(v) = shell.read(cx).search() {
                            v.update(cx, |v, cx| v.set_query(&text, window, cx));
                        }
                    });
                    settle(cx, handle.into(), Duration::from_secs(2)).await;
                }
                if let Some((x, y)) = std::env::var("SNAPSHOT_HOVER").ok().and_then(|e| {
                    let (x, y) = e.split_once(',')?;
                    Some((x.trim().parse::<f32>().ok()?, y.trim().parse::<f32>().ok()?))
                }) {
                    // Moved again a moment later, for a language server
                    // still starting to answer.
                    for (dx, wait) in [(0., 10), (1., 3)] {
                        let position = gpui::point(px(x + dx), px(y));
                        let _ = cx.update_window(handle.into(), |_, window, cx| {
                            window.dispatch_event(
                                gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                                    position,
                                    pressed_button: None,
                                    modifiers: gpui::Modifiers::none(),
                                }),
                                cx,
                            );
                        });
                        settle(cx, handle.into(), Duration::from_secs(wait)).await;
                    }
                }
                if let Some((x, y)) = std::env::var("SNAPSHOT_RIGHT_CLICK").ok().and_then(|e| {
                    let (x, y) = e.split_once(',')?;
                    Some((x.trim().parse::<f32>().ok()?, y.trim().parse::<f32>().ok()?))
                }) {
                    use gpui::{
                        Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
                        PlatformInput, point,
                    };
                    let position = point(px(x), px(y));
                    let _ = cx.update_window(handle.into(), |_, window, cx| {
                        window.dispatch_event(
                            PlatformInput::MouseMove(MouseMoveEvent {
                                position,
                                pressed_button: None,
                                modifiers: Modifiers::none(),
                            }),
                            cx,
                        );
                        window.dispatch_event(
                            PlatformInput::MouseDown(MouseDownEvent {
                                button: MouseButton::Right,
                                position,
                                modifiers: Modifiers::none(),
                                click_count: 1,
                                first_mouse: false,
                            }),
                            cx,
                        );
                        window.dispatch_event(
                            PlatformInput::MouseUp(MouseUpEvent {
                                button: MouseButton::Right,
                                position,
                                modifiers: Modifiers::none(),
                                click_count: 1,
                            }),
                            cx,
                        );
                    });
                    settle(cx, handle.into(), Duration::from_secs(1)).await;
                }
                if let Ok(press) = std::env::var("SNAPSHOT_PRESS") {
                    for (i, part) in press.split('|').enumerate() {
                        let keys: Vec<String> = if i % 2 == 1 {
                            part.chars().map(|c| c.to_string()).collect()
                        } else {
                            part.split_whitespace().map(str::to_string).collect()
                        };
                        for k in keys {
                            // Keystroke::parse keeps its source.
                            let k: &'static str = Box::leak(k.into_boxed_str());
                            let Some(stroke) = gpui::Keystroke::parse(k).ok() else {
                                continue;
                            };
                            // As the platform does: not while the root is
                            // being updated, which the pop-ups update.
                            let _ = cx.update_window(handle.into(), |_, window, cx| {
                                window.dispatch_keystroke(stroke, cx);
                            });
                            settle(cx, handle.into(), Duration::from_millis(150)).await;
                        }
                    }
                    settle(cx, handle.into(), Duration::from_secs(1)).await;
                }
                if let Some(hold) = std::env::var("SNAPSHOT_HOLD")
                    .ok()
                    .and_then(|h| h.parse::<f64>().ok())
                {
                    settle(cx, handle.into(), Duration::from_secs_f64(hold)).await;
                }
                let _ = handle.update(cx, |_, window, _| window.refresh());
                settle(cx, handle.into(), Duration::from_millis(500)).await;
                // A window off the screen is not drawn by itself: draw the
                // frame to be captured.
                let image = cx.update_window(handle.into(), |_, window, cx| {
                    window.refresh();
                    let _ = window.draw(cx);
                    window.render_to_image()
                });
                match image {
                    Ok(Ok(img)) => {
                        img.save(&out).expect("writing the image");
                        eprintln!("wrote {out}");
                    }
                    e => eprintln!("rendering the window: {e:?}"),
                }
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
}

/// Waits, drawing the window every so often meanwhile: a window off the
/// screen, or on a screen asleep, is not drawn by itself, and what its
/// views make as they are first drawn would not be made.
async fn settle(cx: &mut gpui::AsyncApp, window: gpui::AnyWindowHandle, d: Duration) {
    let until = std::time::Instant::now() + d;
    while std::time::Instant::now() < until {
        let _ = cx.update_window(window, |_, window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        cx.background_executor()
            .timer(Duration::from_millis(100))
            .await;
    }
}
