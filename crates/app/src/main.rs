//! Hangar's desktop app.
//!
//! HANGAR_URL and HANGAR_TOKEN add a server place for one run, its token
//! kept in memory, beside the places the user keeps.

use gpui::{AppContext, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_component::{Root, TitleBar};
use gpui_platform::application;
use hangar_app::{places, shell::Shell};

fn main() {
    application().run(|cx| {
        gpui_component::init(cx);
        let places = places::from_env(cx);
        let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..TitleBar::window_options()
            },
            |window, cx| {
                let shell = cx.new(|cx| Shell::new(places, window, cx));
                cx.new(|cx| Root::new(shell, window, cx))
            },
        )
        .expect("opening the window");
        cx.activate(true);
    });
}
