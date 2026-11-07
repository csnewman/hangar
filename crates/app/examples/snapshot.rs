//! A picture of the app: opens its window on the places it would open, waits
//! for them, renders the window to a PNG (through the GPU, without the
//! screen) and exits. For checking how it looks where a screen cannot be
//! recorded.
//!
//!   HANGAR_URL=… HANGAR_TOKEN=… cargo run -p hangar-app --example snapshot -- OUT.png
//!
//! SNAPSHOT_THEME=dark draws it dark; SNAPSHOT_OPEN=<environment name> opens
//! that environment's tab; SNAPSHOT_HOME=place groups Home by place;
//! SNAPSHOT_SIZE=WxH sets the window's size.

use std::time::Duration;

use gpui::{AppContext, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_component::{Root, ThemeMode, TitleBar};
use gpui_platform::application;
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
                cx.background_executor().timer(Duration::from_secs(3)).await;
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
                let _ = handle.update(cx, |_, window, _| window.refresh());
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
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
