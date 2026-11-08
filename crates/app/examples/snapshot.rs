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
//! opens a project's folders; SNAPSHOT_FILE=ROOT:PATH opens a file, and
//! SNAPSHOT_EDIT types at its start; SNAPSHOT_STAGE=ROOT:PATH stages a file
//! from the Commit window and SNAPSHOT_COMMIT=message commits what is staged;
//! SNAPSHOT_BRANCH=1 shows the Commit window's Branch view;
//! SNAPSHOT_FIND=text searches in files and SNAPSHOT_FIND_OPEN=N opens the Nth
//! result row;
//! SNAPSHOT_DIFF=ROOT:PATH shows a file's changes; SNAPSHOT_GIT=N chooses the
//! Git window's Nth commit and SNAPSHOT_GIT_FILE=N shows its Nth file's diff;
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
            hangar_app::layout::Layout::init(
                hangar_app::store::Dirs::user().map(|d| d.layout_file()),
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
                            tab.update(cx, |t, cx| t.open_file(&root, &path, window, cx));
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
                    let _ = handle.update(cx, |_, window, cx| {
                        let find = shell
                            .read(cx)
                            .active_view()
                            .and_then(|t| t.read(cx).find_view());
                        if let Some(f) = find {
                            let query = f.read(cx).query().clone();
                            query.update(cx, |q, cx| q.set_value(text, window, cx));
                            f.update(cx, |f, cx| f.search(cx));
                        }
                    });
                    settle(cx, handle.into(), Duration::from_secs(3)).await;
                    if let Some(n) = std::env::var("SNAPSHOT_FIND_OPEN")
                        .ok()
                        .and_then(|n| n.parse::<usize>().ok())
                    {
                        let _ = handle.update(cx, |_, _, cx| {
                            let find = shell
                                .read(cx)
                                .active_view()
                                .and_then(|t| t.read(cx).find_view());
                            if let Some(f) = find {
                                f.update(cx, |f, cx| f.open(n, cx));
                            }
                        });
                        settle(cx, handle.into(), Duration::from_secs(3)).await;
                    }
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
