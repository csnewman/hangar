//! Terminal colour themes, under the names Ghostty knows them by: a
//! background, a foreground, the sixteen named colours, and the cursor's
//! and the selection's colours. A theme for the app's light appearance and
//! one for its dark are chosen in the settings; none is the app's own
//! colours.

use alacritty_terminal::vte::ansi::Rgb;
use gpui::{App, Hsla};

use crate::settings::Settings;
use crate::terminal::{Look, is_light};

pub struct Theme {
    pub name: &'static str,
    pub bg: u32,
    pub fg: u32,
    pub cursor: u32,
    pub selection: u32,
    pub palette: [u32; 16],
}

const SOLARIZED: [u32; 16] = [
    0x073642, 0xdc322f, 0x859900, 0xb58900, 0x268bd2, 0xd33682, 0x2aa198, 0xeee8d5, 0x002b36,
    0xcb4b16, 0x586e75, 0x657b83, 0x839496, 0x6c71c4, 0x93a1a1, 0xfdf6e3,
];

/// The themes, light ones first.
pub const THEMES: &[Theme] = &[
    Theme {
        name: "Catppuccin Latte",
        bg: 0xeff1f5,
        fg: 0x4c4f69,
        cursor: 0xdc8a78,
        selection: 0xacb0be,
        palette: [
            0x5c5f77, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xacb0be,
            0x6c6f85, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xbcc0cc,
        ],
    },
    Theme {
        name: "Gruvbox Light",
        bg: 0xfbf1c7,
        fg: 0x3c3836,
        cursor: 0x3c3836,
        selection: 0xd5c4a1,
        palette: [
            0xfbf1c7, 0xcc241d, 0x98971a, 0xd79921, 0x458588, 0xb16286, 0x689d6a, 0x7c6f64,
            0x928374, 0x9d0006, 0x79740e, 0xb57614, 0x076678, 0x8f3f71, 0x427b58, 0x3c3836,
        ],
    },
    Theme {
        name: "Solarized Light",
        bg: 0xfdf6e3,
        fg: 0x657b83,
        cursor: 0x586e75,
        selection: 0xeee8d5,
        palette: SOLARIZED,
    },
    Theme {
        name: "Catppuccin Mocha",
        bg: 0x1e1e2e,
        fg: 0xcdd6f4,
        cursor: 0xf5e0dc,
        selection: 0x585b70,
        palette: [
            0x45475a, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xbac2de,
            0x585b70, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xa6adc8,
        ],
    },
    Theme {
        name: "Dracula",
        bg: 0x282a36,
        fg: 0xf8f8f2,
        cursor: 0xf8f8f2,
        selection: 0x44475a,
        palette: [
            0x21222c, 0xff5555, 0x50fa7b, 0xf1fa8c, 0xbd93f9, 0xff79c6, 0x8be9fd, 0xf8f8f2,
            0x6272a4, 0xff6e6e, 0x69ff94, 0xffffa5, 0xd6acff, 0xff92df, 0xa4ffff, 0xffffff,
        ],
    },
    Theme {
        name: "Gruvbox Dark",
        bg: 0x282828,
        fg: 0xebdbb2,
        cursor: 0xebdbb2,
        selection: 0x504945,
        palette: [
            0x282828, 0xcc241d, 0x98971a, 0xd79921, 0x458588, 0xb16286, 0x689d6a, 0xa89984,
            0x928374, 0xfb4934, 0xb8bb26, 0xfabd2f, 0x83a598, 0xd3869b, 0x8ec07c, 0xebdbb2,
        ],
    },
    Theme {
        name: "Nord",
        bg: 0x2e3440,
        fg: 0xd8dee9,
        cursor: 0xeceff4,
        selection: 0x434c5e,
        palette: [
            0x3b4252, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x88c0d0, 0xe5e9f0,
            0x4c566a, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x8fbcbb, 0xeceff4,
        ],
    },
    Theme {
        name: "Solarized Dark",
        bg: 0x002b36,
        fg: 0x839496,
        cursor: 0x93a1a1,
        selection: 0x073642,
        palette: SOLARIZED,
    },
    Theme {
        name: "TokyoNight",
        bg: 0x1a1b26,
        fg: 0xc0caf5,
        cursor: 0xc0caf5,
        selection: 0x283457,
        palette: [
            0x15161e, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xa9b1d6,
            0x414868, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xc0caf5,
        ],
    },
];

pub fn rgb(hex: u32) -> Rgb {
    Rgb {
        r: (hex >> 16) as u8,
        g: (hex >> 8) as u8,
        b: hex as u8,
    }
}

pub fn find(name: &str) -> Option<&'static Theme> {
    THEMES.iter().find(|t| t.name == name)
}

impl Theme {
    pub fn is_light(&self) -> bool {
        is_light(rgb(self.bg))
    }
}

/// The theme the settings choose for the app's appearance, if any.
pub fn chosen(cx: &App) -> Option<&'static Theme> {
    let t = Settings::get(cx).terminal;
    let name = if crate::palette::is_dark(cx) {
        t.theme_dark
    } else {
        t.theme_light
    };
    find(&name)
}

/// How a terminal draws its colours, as the settings and the appearance
/// say: a theme's, else the app's.
pub fn look(cx: &App) -> Look {
    let p = crate::palette::of(cx);
    let t = Settings::get(cx).terminal;
    let theme = chosen(cx);
    Look {
        fg: theme.map_or_else(|| from_hsla(p.term_fg), |t| rgb(t.fg)),
        bg: theme.map_or_else(|| from_hsla(p.term_tab), |t| rgb(t.bg)),
        palette: theme.map(|t| t.palette.map(rgb)),
        bold_is_bright: t.bold_is_bright,
        faint: t.faint_opacity(),
    }
}

/// The colours a terminal's tab and screen are drawn in: its text's and
/// its background's.
pub fn screen_colours(cx: &App) -> (Hsla, Hsla) {
    let l = look(cx);
    (crate::termpaint::hsla(l.fg), crate::termpaint::hsla(l.bg))
}

pub fn from_hsla(c: Hsla) -> Rgb {
    let c = gpui::Rgba::from(c);
    Rgb {
        r: (c.r * 255.) as u8,
        g: (c.g * 255.) as u8,
        b: (c.b * 255.) as u8,
    }
}
