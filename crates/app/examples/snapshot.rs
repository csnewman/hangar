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
//! SNAPSHOT_ADD=kinds (or address) opens Add a Place at that step;
//! SNAPSHOT_PREFS=JSON sets settings over the copy of the user's, objects
//! merged ({"terminal": {"theme_light": "Nord"}});
//! SNAPSHOT_SETTINGS=page opens Settings at a page (appearance, keymap,
//! editor, terminal, notifications, places, linked, packs, or account,
//! templates, place-packs, teams, images, users, workers, audit and browser
//! for the first place);
//! SNAPSHOT_ENV_SETTINGS=page opens the opened environment's settings at a
//! page (general, resources, files, access, ports, danger);
//! SNAPSHOT_NEW_ENV=1 opens the New environment sheet (SNAPSHOT_PRESS
//! types into it);
//! SNAPSHOT_ASK=METHOD:LINE:COLUMN asks the open file's language server
//! (textDocument/hover, textDocument/definition) and prints its answer;
//! SNAPSHOT_FIND=text searches in files in Find in Files' pop-up,
//! SNAPSHOT_FIND_SELECT=N chooses its Nth match, SNAPSHOT_FIND_OPEN=N opens it,
//! and SNAPSHOT_FIND_WINDOW=1 keeps the matches in the Find window;
//! SNAPSHOT_REPLACE=1 shows the pop-up as Replace in Files;
//! SNAPSHOT_CARET=LINE:COL puts the open file's caret there, then
//! SNAPSHOT_USAGES=1 finds its usages (=popup: in Show Usages' pop-up),
//! SNAPSHOT_RENAME=name renames it, and
//! SNAPSHOT_SYMBOL=text goes to a symbol;
//! SNAPSHOT_DIFF=ROOT:PATH shows a file's changes; SNAPSHOT_GIT=N chooses the
//! Git window's Nth commit and SNAPSHOT_GIT_FILE=N shows its Nth file's diff;
//! SNAPSHOT_HOVER=X,Y rests the pointer there; SNAPSHOT_CLICK=X,Y clicks there
//! (X,Y;X,Y... in turn);
//! SNAPSHOT_DRAG=X1,Y1,X2,Y2 drags from one place to the other;
//! SNAPSHOT_DRAG=X1,Y1;X2,Y2 drags from one point to the other;
//! SNAPSHOT_RIGHT_CLICK=X,Y right-clicks there, as the pointer would, and
//! SNAPSHOT_PRESS="down enter |text| enter" then presses keys, typing what
//! is between bars, and SNAPSHOT_THEN_CLICK="X,Y;..." clicks after;
//! SNAPSHOT_PLACE=JSON adds a place for the run (a `PlaceConfig`, such as
//! one made on a Proxmox, whose This Hangar page SNAPSHOT_SETTINGS=hangar
//! opens);
//! SNAPSHOT_STEPS="click X,Y; scroll X,Y,DY; press KEYS; type TEXT; wait S"
//! runs steps in turn after the clicks: a click, the wheel turned DY
//! pixels down over a point, keys pressed, text typed, a wait;
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
            hangar_app::prefs::bind_keys(cx);
            hangar_app::envsettings::bind_keys(cx);
            hangar_app::newenv::bind_keys(cx);
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
            // Settings of the snapshot's own, over the copy of the user's.
            if let Ok(json) = std::env::var("SNAPSHOT_PREFS") {
                let over: serde_json::Value =
                    serde_json::from_str(&json).expect("SNAPSHOT_PREFS is JSON");
                hangar_app::settings::Settings::update(cx, |v| {
                    let mut all = serde_json::to_value(&*v).unwrap();
                    merge(&mut all, over);
                    *v = serde_json::from_value(all).expect("SNAPSHOT_PREFS are settings");
                });
            }
            let places = places::from_env(cx);
            if let Ok(json) = std::env::var("SNAPSHOT_PLACE") {
                match serde_json::from_str::<hangar_app::place::PlaceConfig>(&json) {
                    Ok(config) => places.update(cx, |p, cx| p.add_for_run(config, cx)),
                    Err(e) => eprintln!("SNAPSHOT_PLACE: {e}"),
                }
            }
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
                if let Ok(step) = std::env::var("SNAPSHOT_ADD") {
                    let _ = handle.update(cx, |_, window, cx| {
                        shell.update(cx, |s, cx| s.open_add(window, cx));
                        if step == "address"
                            && let Some(v) = shell.read(cx).adding()
                        {
                            v.update(cx, |v, cx| v.go(hangar_app::addplace::Step::Address, cx));
                        }
                    });
                }
                if let Ok(page) = std::env::var("SNAPSHOT_SETTINGS") {
                    let _ = handle.update(cx, |_, window, cx| {
                        use hangar_app::prefs::Page;
                        let first = places
                            .read(cx)
                            .list()
                            .first()
                            .map(|(c, _)| c.id.clone())
                            .unwrap_or_default();
                        let page = match page.as_str() {
                            "keymap" => Page::Keymap,
                            "editor" => Page::Editor,
                            "terminal" => Page::Terminal,
                            "notifications" => Page::Notifications,
                            "places" => Page::Places,
                            "linked" => Page::Linked,
                            "packs" => Page::Packs,
                            "sync-templates" => Page::SyncTemplates,
                            "account" => Page::Account(first),
                            "templates" => Page::Templates(first),
                            "place-packs" => Page::PlacePacks(first),
                            "teams" => Page::Teams(first),
                            "images" => Page::Images(first),
                            "users" => Page::Users(first),
                            "workers" => Page::Workers(first),
                            "audit" => Page::Audit(first),
                            "browser" => Page::Browser(first),
                            "hangar" => Page::Hangar(
                                places
                                    .read(cx)
                                    .list()
                                    .iter()
                                    .find(|(c, _)| c.proxmox.is_some())
                                    .map(|(c, _)| c.id.clone())
                                    .unwrap_or(first),
                            ),
                            _ => Page::Appearance,
                        };
                        shell.update(cx, |s, cx| s.open_settings(Some(page), window, cx));
                    });
                }
                if std::env::var("SNAPSHOT_NEW_ENV").is_ok() {
                    let _ = handle.update(cx, |_, window, cx| {
                        shell.update(cx, |s, cx| s.open_new_env(None, window, cx));
                    });
                    settle(cx, handle.into(), Duration::from_secs(2)).await;
                }
                if let (Ok(page), Ok(name)) = (
                    std::env::var("SNAPSHOT_ENV_SETTINGS"),
                    std::env::var("SNAPSHOT_OPEN"),
                ) {
                    let _ = handle.update(cx, |_, window, cx| {
                        use hangar_app::envsettings::Page;
                        let page = match page.as_str() {
                            "resources" => Page::Resources,
                            "files" => Page::Files,
                            "access" => Page::Access,
                            "ports" => Page::Ports,
                            "danger" => Page::Danger,
                            _ => Page::General,
                        };
                        let found = places.read(cx).list().iter().find_map(|(p, s)| {
                            s.environments
                                .iter()
                                .find(|e| e.name == name)
                                .map(|e| (p.id.clone(), e.id.clone()))
                        });
                        if let Some((p, e)) = found {
                            shell.update(cx, |s, cx| s.open_env_settings(&p, &e, page, window, cx));
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
                if let Ok(how) = std::env::var("SNAPSHOT_USAGES") {
                    let popup = how == "popup";
                    let _ = handle.update(cx, |_, window, cx| {
                        if let Some(tab) = shell.read(cx).active_view() {
                            tab.update(cx, |t, cx| t.find_usages(popup, window, cx));
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
                if let Some(points) = std::env::var("SNAPSHOT_DRAG").ok().and_then(|e| {
                    let v: Vec<f32> = e.split(',').filter_map(|n| n.trim().parse().ok()).collect();
                    (v.len() == 4).then_some(v)
                }) {
                    use gpui::{
                        Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
                        PlatformInput,
                    };
                    let (from, to) = (
                        gpui::point(px(points[0]), px(points[1])),
                        gpui::point(px(points[2]), px(points[3])),
                    );
                    let _ = cx.update_window(handle.into(), |_, window, cx| {
                        window.dispatch_event(
                            PlatformInput::MouseMove(MouseMoveEvent {
                                position: from,
                                pressed_button: None,
                                modifiers: Modifiers::none(),
                            }),
                            cx,
                        );
                        window.dispatch_event(
                            PlatformInput::MouseDown(MouseDownEvent {
                                button: MouseButton::Left,
                                position: from,
                                modifiers: Modifiers::none(),
                                click_count: 1,
                                first_mouse: false,
                            }),
                            cx,
                        );
                    });
                    for i in 1..=10 {
                        let f = i as f32 / 10.;
                        let at =
                            gpui::point(from.x + (to.x - from.x) * f, from.y + (to.y - from.y) * f);
                        let _ = cx.update_window(handle.into(), |_, window, cx| {
                            window.dispatch_event(
                                PlatformInput::MouseMove(MouseMoveEvent {
                                    position: at,
                                    pressed_button: Some(MouseButton::Left),
                                    modifiers: Modifiers::none(),
                                }),
                                cx,
                            );
                        });
                        settle(cx, handle.into(), Duration::from_millis(30)).await;
                    }
                    let _ = cx.update_window(handle.into(), |_, window, cx| {
                        window.dispatch_event(
                            PlatformInput::MouseUp(MouseUpEvent {
                                button: MouseButton::Left,
                                position: to,
                                modifiers: Modifiers::none(),
                                click_count: 1,
                            }),
                            cx,
                        );
                    });
                    settle(cx, handle.into(), Duration::from_secs(1)).await;
                }
                click_all(cx, handle.into(), "SNAPSHOT_CLICK").await;
                steps(cx, handle.into()).await;
                // A drag with the left button, from one point to another.
                let drag: Vec<(f32, f32)> = std::env::var("SNAPSHOT_DRAG")
                    .unwrap_or_default()
                    .split(';')
                    .filter_map(|e| {
                        let (x, y) = e.split_once(',')?;
                        Some((x.trim().parse::<f32>().ok()?, y.trim().parse::<f32>().ok()?))
                    })
                    .collect();
                if let [(x1, y1), (x2, y2)] = drag[..] {
                    use gpui::{
                        Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
                        PlatformInput,
                    };
                    let (from, to) = (gpui::point(px(x1), px(y1)), gpui::point(px(x2), px(y2)));
                    let _ = cx.update_window(handle.into(), |_, window, cx| {
                        window.dispatch_event(
                            PlatformInput::MouseDown(MouseDownEvent {
                                button: MouseButton::Left,
                                position: from,
                                modifiers: Modifiers::none(),
                                click_count: 1,
                                first_mouse: false,
                            }),
                            cx,
                        );
                        window.dispatch_event(
                            PlatformInput::MouseMove(MouseMoveEvent {
                                position: to,
                                pressed_button: Some(MouseButton::Left),
                                modifiers: Modifiers::none(),
                            }),
                            cx,
                        );
                        window.dispatch_event(
                            PlatformInput::MouseUp(MouseUpEvent {
                                button: MouseButton::Left,
                                position: to,
                                modifiers: Modifiers::none(),
                                click_count: 1,
                            }),
                            cx,
                        );
                    });
                    settle(cx, handle.into(), Duration::from_secs(1)).await;
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
                click_all(cx, handle.into(), "SNAPSHOT_THEN_CLICK").await;
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
/// Clicks at each "X,Y" of a variable's list, split by semicolons,
/// letting the window settle after each.
async fn click_all(cx: &mut gpui::AsyncApp, handle: gpui::AnyWindowHandle, var: &str) {
    use gpui::{Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, PlatformInput};
    let clicks: Vec<(f32, f32)> = std::env::var(var)
        .unwrap_or_default()
        .split(';')
        .filter_map(|e| {
            let (x, y) = e.split_once(',')?;
            Some((x.trim().parse::<f32>().ok()?, y.trim().parse::<f32>().ok()?))
        })
        .collect();
    for (x, y) in clicks {
        let position = gpui::point(px(x), px(y));
        let _ = cx.update_window(handle, |_, window, cx| {
            window.dispatch_event(
                PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers: Modifiers::none(),
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers: Modifiers::none(),
                    click_count: 1,
                }),
                cx,
            );
        });
        settle(cx, handle, Duration::from_secs(1)).await;
    }
}

/// SNAPSHOT_STEPS, in turn.
async fn steps(cx: &mut gpui::AsyncApp, handle: gpui::AnyWindowHandle) {
    use gpui::{
        Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, PlatformInput, ScrollDelta,
        ScrollEvent, ScrollSource, TouchPhase,
    };
    let all = std::env::var("SNAPSHOT_STEPS").unwrap_or_default();
    for step in all.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        let (what, rest) = step.split_once(' ').unwrap_or((step, ""));
        let n: Vec<f32> = rest
            .split(',')
            .filter_map(|x| x.trim().parse().ok())
            .collect();
        match (what, n.as_slice()) {
            ("click", [x, y]) => {
                let position = gpui::point(px(*x), px(*y));
                let _ = cx.update_window(handle, |_, window, cx| {
                    window.dispatch_event(
                        PlatformInput::MouseDown(MouseDownEvent {
                            button: MouseButton::Left,
                            position,
                            modifiers: Modifiers::none(),
                            click_count: 1,
                            first_mouse: false,
                        }),
                        cx,
                    );
                    window.dispatch_event(
                        PlatformInput::MouseUp(MouseUpEvent {
                            button: MouseButton::Left,
                            position,
                            modifiers: Modifiers::none(),
                            click_count: 1,
                        }),
                        cx,
                    );
                });
            }
            ("scroll", [x, y, dy]) => {
                let _ = cx.update_window(handle, |_, window, cx| {
                    window.dispatch_event(
                        PlatformInput::Scroll(ScrollEvent {
                            position: gpui::point(px(*x), px(*y)),
                            delta: ScrollDelta::Pixels(gpui::point(px(0.), px(-*dy))),
                            modifiers: Modifiers::none(),
                            touch_phase: TouchPhase::Moved,
                            source: ScrollSource::default(),
                        }),
                        cx,
                    );
                });
            }
            ("press" | "type", _) => {
                let keys: Vec<String> = if what == "type" {
                    rest.chars()
                        .map(|c| {
                            if c == ' ' {
                                "space".into()
                            } else {
                                c.to_string()
                            }
                        })
                        .collect()
                } else {
                    rest.split_whitespace().map(str::to_string).collect()
                };
                for k in keys {
                    let k: &'static str = Box::leak(k.into_boxed_str());
                    let Some(stroke) = gpui::Keystroke::parse(k).ok() else {
                        continue;
                    };
                    let _ = cx.update_window(handle, |_, window, cx| {
                        window.dispatch_keystroke(stroke, cx);
                    });
                    settle(cx, handle, Duration::from_millis(80)).await;
                }
            }
            ("wait", [secs]) => settle(cx, handle, Duration::from_secs_f32(*secs)).await,
            _ => eprintln!("snapshot: a step it does not know: {step}"),
        }
        settle(cx, handle, Duration::from_millis(800)).await;
    }
}

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

/// Sets what `over` has in `all`, objects merged key by key.
fn merge(all: &mut serde_json::Value, over: serde_json::Value) {
    match (all, over) {
        (serde_json::Value::Object(a), serde_json::Value::Object(o)) => {
            for (k, v) in o {
                merge(a.entry(k).or_insert(serde_json::Value::Null), v);
            }
        }
        (a, o) => *a = o,
    }
}
