//! Hangar's desktop app.
//!
//! HANGAR_URL and HANGAR_TOKEN add a server place for one run, its token
//! kept in memory, beside the places the user keeps.

use gpui::{AppContext, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_component::{Root, TitleBar};
use gpui_platform::application;
use hangar_app::{places, shell::Shell};

fn main() {
    application()
        .with_assets(hangar_app::assets::Assets)
        .run(|cx| {
            gpui_component::init(cx);
            hangar_app::envtab::bind_keys(cx);
            hangar_app::project::bind_keys(cx);
            hangar_app::docedit::bind_keys(cx);
            hangar_app::shell::bind_keys(cx);
            hangar_app::prefs::bind_keys(cx);
            hangar_app::envsettings::bind_keys(cx);
            hangar_app::termview::bind_keys(cx);
            hangar_app::menus::init(cx);
            hangar_app::layout::Layout::init(
                hangar_app::store::Dirs::user().map(|d| d.layout_file()),
                cx,
            );
            hangar_app::settings::Settings::init(
                hangar_app::store::Dirs::user().map(|d| d.settings_file()),
                cx,
            );
            let places = places::from_env(cx);
            hangar_app::wake::watch(places.clone(), cx);
            let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..TitleBar::window_options()
                },
                |window, cx| {
                    hangar_app::palette::sync(Some(window), cx);
                    let shell = cx.new(|cx| Shell::new(places, window, cx));
                    cx.new(|cx| Root::new(shell, window, cx))
                },
            )
            .expect("opening the window");
            cx.activate(true);
        });
}
