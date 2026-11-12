//! The app's settings: what an older settings file keeps, and the Settings
//! window, which changes nothing until Apply or OK.

use std::sync::Arc;

use gpui::{AppContext, TestAppContext};
use hangar_app::places::Places;
use hangar_app::prefs::{Page, humanise};
use hangar_app::settings::{Event, Settings, ThemeChoice, Values};
use hangar_app::shell::Shell;
use hangar_app::store::{MemoryTokens, Tokens};

#[test]
fn an_older_settings_file_keeps_its_values_and_takes_the_rest_as_default() {
    let v: Values = serde_json::from_str(r#"{"soft_wrap": false, "double_shift": false}"#).unwrap();
    assert!(!v.soft_wrap && !v.double_shift);
    assert!(v.select_opened && v.show_hidden);
    assert_eq!(v.theme, ThemeChoice::System);
    assert_eq!(v.editor_font(), 13.);
    let asking = v.notify.ways(Event::Asking);
    assert!(asking.app && asking.system && asking.badge && asking.sound);
    let ready = v.notify.ways(Event::Ready);
    assert!(ready.app && !ready.system && !ready.badge && !ready.sound);
    assert!(v.notify.while_looking);
}

#[test]
fn font_sizes_stay_within_reason() {
    let v = Values {
        editor_font_size: 100.,
        terminal_font_size: f32::NAN,
        ..Default::default()
    };
    assert_eq!(v.editor_font(), 32.);
    assert_eq!(v.terminal_font(), 12.5);
}

#[test]
fn quiet_places_raise_only_the_list() {
    let mut v = Values::default();
    v.notify.quiet_places.push("lab".into());
    assert!(!v.notify.from("lab"));
    assert!(v.notify.from("acme"));
}

#[test]
fn actions_read_as_words() {
    assert_eq!(humanise("envtab::ToggleProject"), "Toggle Project");
    assert_eq!(humanise("shell::OpenSettings"), "Open Settings");
    assert_eq!(
        humanise("docedit::SelectAllOccurrences"),
        "Select All Occurrences"
    );
    assert_eq!(humanise("Cancel"), "Cancel");
}

/// Renaming, reordering and removing places: a removed one's token goes
/// with it.
#[gpui::test]
fn places_are_arranged(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let tokens = Arc::new(MemoryTokens::default());
    let places = cx.new(|cx| Places::offline(tokens.clone(), cx));
    places.update(cx, |p, cx| {
        let a = p.add_server("http://127.0.0.1:9", "hgr_a", cx).unwrap();
        let b = p.add_server("http://127.0.0.1:10", "hgr_b", cx).unwrap();
        let c = p.add_server("http://127.0.0.1:11", "hgr_c", cx).unwrap();
        p.arrange(
            &[(c.clone(), String::new()), (a.clone(), "Devbox".into())],
            cx,
        );
        let listed: Vec<(&str, &str)> = p
            .list()
            .iter()
            .map(|(c, _)| (c.id.as_str(), c.name.as_str()))
            .collect();
        assert_eq!(listed, [(c.as_str(), "127.0.0.1"), (a.as_str(), "Devbox")]);
        assert!(p.get(&b).is_none());
    });
    assert_eq!(
        tokens.get("127.0.0.1:10"),
        None,
        "the removed place's token"
    );
    assert_eq!(tokens.get("127.0.0.1:9").as_deref(), Some("hgr_a"));
}

/// ⌘, opens Settings; what is changed in it is kept on Apply, and dropped
/// by Cancel or Escape.
#[gpui::test]
fn settings_change_only_when_applied(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(hangar_app::shell::bind_keys);
    cx.update(hangar_app::prefs::bind_keys);
    let tokens = Arc::new(MemoryTokens::default());
    let places = cx.new(|cx| Places::offline(tokens.clone(), cx));
    places.update(cx, |p, cx| {
        p.add_server("http://127.0.0.1:9", "hgr_a", cx).unwrap();
    });
    let (shell, cx) = cx.add_window_view(|window, cx| Shell::new(places.clone(), window, cx));

    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-,"
    } else {
        "ctrl-alt-s"
    });
    let view = shell
        .read_with(cx, |s, _| s.settings())
        .expect("Settings open");
    view.update(cx, |v, cx| {
        assert_eq!(v.page(), &Page::Appearance);
        v.edit(|s| s.soft_wrap = false, cx);
        assert!(v.changed(cx));
    });
    cx.simulate_keystrokes("escape");
    assert!(
        shell.read_with(cx, |s, _| s.settings()).is_none(),
        "Escape closes it"
    );
    assert!(
        cx.update(|_, cx| Settings::get(cx).soft_wrap),
        "nothing kept"
    );

    cx.update(|window, cx| {
        shell.update(cx, |s, cx| s.open_settings(Some(Page::Places), window, cx))
    });
    let view = shell.read_with(cx, |s, _| s.settings()).unwrap();
    cx.update(|window, cx| {
        view.update(cx, |v, cx| {
            v.edit(|s| s.soft_wrap = false, cx);
            v.remove_place("127.0.0.1:9", cx);
            v.apply(window, cx);
            assert!(!v.changed(cx));
        })
    });
    assert!(
        !cx.update(|_, cx| Settings::get(cx).soft_wrap),
        "kept on Apply"
    );
    places.read_with(cx, |p, _| assert!(p.list().is_empty(), "the place removed"));
    assert_eq!(tokens.get("127.0.0.1:9"), None);
}
