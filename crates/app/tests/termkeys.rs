//! What the terminal sends programs for keys and the mouse: xterm's
//! encodings, the Kitty keyboard protocol when asked, and mouse reports.

use hangar_app::termkeys::{Button, Key, KeyModes, MouseModes, Press, encode, mouse};

fn key(name: &str) -> Key<'_> {
    Key {
        name,
        text: (name.chars().count() == 1).then_some(name),
        ..Key::default()
    }
}

fn sent(k: Key, m: &KeyModes) -> String {
    String::from_utf8(encode(&k, m).unwrap()).unwrap()
}

#[test]
fn legacy_keys_as_xterm_sends_them() {
    let m = KeyModes::default();
    assert_eq!(sent(key("enter"), &m), "\r");
    assert_eq!(
        sent(
            Key {
                shift: true,
                ..key("enter")
            },
            &m
        ),
        "\r"
    );
    assert_eq!(sent(key("a"), &m), "a");
    assert_eq!(
        sent(
            Key {
                ctrl: true,
                ..key("c")
            },
            &m
        ),
        "\x03"
    );
    assert_eq!(
        sent(
            Key {
                ctrl: true,
                ..key("[")
            },
            &m
        ),
        "\x1b"
    );
    assert_eq!(sent(key("escape"), &m), "\x1b");
    assert_eq!(
        sent(
            Key {
                shift: true,
                ..key("tab")
            },
            &m
        ),
        "\x1b[Z"
    );
    assert_eq!(
        sent(
            Key {
                alt: true,
                ..key("backspace")
            },
            &m
        ),
        "\x1b\x7f"
    );
    // The cursor keys, with modifiers and in application mode.
    assert_eq!(sent(key("up"), &m), "\x1b[A");
    let app = KeyModes {
        app_cursor: true,
        ..m
    };
    assert_eq!(sent(key("up"), &app), "\x1bOA");
    assert_eq!(
        sent(
            Key {
                shift: true,
                ..key("up")
            },
            &app
        ),
        "\x1b[1;2A"
    );
    assert_eq!(
        sent(
            Key {
                ctrl: true,
                ..key("right")
            },
            &m
        ),
        "\x1b[1;5C"
    );
    // Function keys and the editing keys.
    assert_eq!(sent(key("f1"), &m), "\x1bOP");
    assert_eq!(sent(key("f5"), &m), "\x1b[15~");
    assert_eq!(
        sent(
            Key {
                shift: true,
                ..key("f5")
            },
            &m
        ),
        "\x1b[15;2~"
    );
    assert_eq!(sent(key("pageup"), &m), "\x1b[5~");
    assert_eq!(sent(key("delete"), &m), "\x1b[3~");
    // Alt as Meta, where it is: ESC then the key.
    let meta = KeyModes {
        alt_is_meta: true,
        ..m
    };
    assert_eq!(
        sent(
            Key {
                alt: true,
                ..key("b")
            },
            &meta
        ),
        "\x1bb"
    );
    assert_eq!(
        sent(
            Key {
                alt: true,
                text: Some("∫"),
                ..key("b")
            },
            &m
        ),
        "∫",
        "Option's own character"
    );
}

#[test]
fn kitty_escape_codes_when_the_program_asks() {
    let m = KeyModes {
        disambiguate: true,
        ..KeyModes::default()
    };
    // Shift-Enter says itself, so a program can tell it from Enter.
    assert_eq!(
        sent(
            Key {
                shift: true,
                ..key("enter")
            },
            &m
        ),
        "\x1b[13;2u"
    );
    assert_eq!(sent(key("enter"), &m), "\r", "Enter alone stays");
    assert_eq!(sent(key("escape"), &m), "\x1b[27u");
    assert_eq!(
        sent(
            Key {
                ctrl: true,
                ..key("c")
            },
            &m
        ),
        "\x1b[99;5u"
    );
    assert_eq!(
        sent(
            Key {
                alt: true,
                ..key("x")
            },
            &m
        ),
        "\x1b[120;3u"
    );
    assert_eq!(sent(key("a"), &m), "a", "text stays text");
    assert_eq!(
        sent(
            Key {
                shift: true,
                ..key("up")
            },
            &m
        ),
        "\x1b[1;2A"
    );
    let all = KeyModes {
        all_as_escapes: true,
        ..m
    };
    assert_eq!(sent(key("a"), &all), "\x1b[97u");
    assert_eq!(
        sent(
            Key {
                shift: true,
                text: Some("A"),
                ..key("a")
            },
            &all
        ),
        "\x1b[97;2u"
    );
    assert_eq!(sent(key("enter"), &all), "\x1b[13u");
}

fn report(b: Button, p: Press, col: usize, row: usize, m: &MouseModes) -> Option<String> {
    mouse(b, p, col, row, false, false, false, m).map(|v| String::from_utf8_lossy(&v).into_owned())
}

#[test]
fn the_mouse_is_reported_as_asked() {
    let none = MouseModes::default();
    assert_eq!(
        report(Button::Left, Press::Down, 0, 0, &none),
        None,
        "not asked"
    );

    let sgr = MouseModes {
        clicks: true,
        sgr: true,
        ..MouseModes::default()
    };
    assert_eq!(
        report(Button::Left, Press::Down, 0, 0, &sgr).as_deref(),
        Some("\x1b[<0;1;1M")
    );
    assert_eq!(
        report(Button::Left, Press::Up, 0, 0, &sgr).as_deref(),
        Some("\x1b[<0;1;1m")
    );
    assert_eq!(
        report(Button::Left, Press::Moved, 2, 1, &sgr),
        None,
        "clicks only"
    );
    assert_eq!(
        report(Button::WheelUp, Press::Down, 4, 4, &sgr).as_deref(),
        Some("\x1b[<64;5;5M")
    );
    let ctrl = mouse(Button::Right, Press::Down, 0, 0, false, false, true, &sgr).unwrap();
    assert_eq!(ctrl, b"\x1b[<18;1;1M");

    let drags = MouseModes { drags: true, ..sgr };
    assert_eq!(
        report(Button::Left, Press::Moved, 2, 1, &drags).as_deref(),
        Some("\x1b[<32;3;2M")
    );
    assert_eq!(
        report(Button::None, Press::Moved, 2, 1, &drags),
        None,
        "no button held"
    );
    let motion = MouseModes {
        motion: true,
        ..sgr
    };
    assert_eq!(
        report(Button::None, Press::Moved, 2, 1, &motion).as_deref(),
        Some("\x1b[<35;3;2M")
    );

    // The 8-bit form: each number plus 32, a release as button 3.
    let normal = MouseModes {
        clicks: true,
        ..MouseModes::default()
    };
    let down = mouse(
        Button::Left,
        Press::Down,
        0,
        0,
        false,
        false,
        false,
        &normal,
    )
    .unwrap();
    assert_eq!(down, [0x1b, b'[', b'M', 32, 33, 33]);
    let up = mouse(Button::Left, Press::Up, 0, 0, false, false, false, &normal).unwrap();
    assert_eq!(up, [0x1b, b'[', b'M', 35, 33, 33]);
    assert_eq!(
        report(Button::Left, Press::Down, 300, 0, &normal),
        None,
        "beyond its reach"
    );
}
