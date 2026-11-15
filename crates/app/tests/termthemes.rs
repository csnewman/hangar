//! Terminal themes, and text kept readable against its background.

use alacritty_terminal::vte::ansi::Rgb;
use hangar_app::termpaint::{contrast, contrasting};
use hangar_app::termthemes::{THEMES, find, rgb};

#[test]
fn themes_are_found_by_name_and_say_whether_they_are_light() {
    let names: Vec<&str> = THEMES.iter().map(|t| t.name).collect();
    let mut unique = names.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), names.len(), "{names:?}");
    assert!(find("Solarized Light").unwrap().is_light());
    assert!(!find("Dracula").unwrap().is_light());
    assert!(find("Nothing Such").is_none());
    assert_eq!(
        rgb(0x123456),
        Rgb {
            r: 0x12,
            g: 0x34,
            b: 0x56
        }
    );
    // Each theme's text stands out from its background.
    for t in THEMES {
        assert!(contrast(rgb(t.fg), rgb(t.bg)) > 3., "{}", t.name);
    }
}

#[test]
fn text_too_close_to_its_background_turns_white_or_black() {
    let grey = Rgb {
        r: 0x40,
        g: 0x40,
        b: 0x40,
    };
    let dark = Rgb {
        r: 0x30,
        g: 0x30,
        b: 0x30,
    };
    assert!((contrast(grey, grey) - 1.).abs() < 1e-4);
    assert_eq!(contrasting(grey, dark, 1.), grey, "off at 1");
    assert_eq!(
        contrasting(grey, dark, 3.),
        Rgb {
            r: 255,
            g: 255,
            b: 255
        }
    );
    let pale = Rgb {
        r: 0xee,
        g: 0xee,
        b: 0xee,
    };
    assert_eq!(
        contrasting(
            pale,
            Rgb {
                r: 255,
                g: 255,
                b: 255
            },
            3.
        ),
        Rgb { r: 0, g: 0, b: 0 }
    );
    let black = Rgb { r: 0, g: 0, b: 0 };
    assert_eq!(contrasting(black, pale, 3.), black, "enough already");
}
