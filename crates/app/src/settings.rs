//! The app's own settings, kept in settings.json in its config folder.

use std::path::PathBuf;

use gpui::{App, Global};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Values {
    /// Whether pressing Shift twice opens Search Everywhere, as in
    /// IntelliJ.
    pub double_shift: bool,
    /// Whether the file shown is selected in the Project window as it is
    /// shown, as IntelliJ's Always Select Opened File.
    pub select_opened: bool,
    /// Whether long lines wrap at the editor's edge.
    pub soft_wrap: bool,
    /// Whether the Project and Filesystem windows show files whose names
    /// start with a dot, and those git ignores.
    pub show_hidden: bool,
    pub show_ignored: bool,
    /// Light, dark, or as the system is.
    pub theme: ThemeChoice,
    /// The editor's text, in points.
    pub editor_font_size: f32,
    /// The terminal's text, in points.
    pub terminal_font_size: f32,
    /// What keeps a terminal's screen, for terminals opened after it is
    /// chosen.
    pub terminal_emulator: TerminalEmulator,
    /// How terminals look and behave.
    pub terminal: Terminal,
    /// Whether the pointer hides while typing, anywhere in the app, until
    /// the mouse moves.
    pub hide_pointer: bool,
    pub notify: Notify,
}

impl Default for Values {
    fn default() -> Self {
        Values {
            double_shift: true,
            select_opened: true,
            soft_wrap: true,
            show_hidden: true,
            show_ignored: true,
            theme: ThemeChoice::System,
            editor_font_size: EDITOR_FONT_SIZE,
            terminal_font_size: TERMINAL_FONT_SIZE,
            terminal_emulator: TerminalEmulator::default(),
            terminal: Terminal::default(),
            hide_pointer: true,
            notify: Notify::default(),
        }
    }
}

/// How terminals look and behave: each as Ghostty's option of the name in
/// brackets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Terminal {
    /// The font's family, empty for the platform's monospaced font
    /// (`font-family`).
    pub font_family: String,
    /// A line's height, in the font's size (`adjust-cell-height`).
    pub line_height: f32,
    /// Whether Option is Alt, as a terminal's programs know it, rather than
    /// typing the characters it makes on a Mac (`macos-option-as-alt`).
    pub option_as_alt: bool,
    /// The cursor until a program asks for another (`cursor-style`), and
    /// whether it blinks (`cursor-style-blink`).
    pub cursor: CursorShape,
    pub cursor_blink: bool,
    /// Whether selecting text copies it (`copy-on-select`).
    pub copy_on_select: bool,
    /// What a right-click does where the program has not asked for the
    /// mouse (`right-click-action`).
    pub right_click: RightClick,
    /// What the bell does (`bell-features`): mark the terminal's tab and
    /// the window's, bounce the Dock icon while the app is behind others,
    /// play the system's sound.
    pub bell_mark: bool,
    pub bell_bounce: bool,
    pub bell_sound: bool,
    /// Whether the progress a program reports (OSC 9;4) shows along the
    /// terminal's top.
    pub progress: bool,
    /// The colours in a light and a dark appearance: a theme's name, empty
    /// for the app's own (`theme`).
    pub theme_light: String,
    pub theme_dark: String,
    /// The least contrast text keeps against its background, 1 to 21: below
    /// it, text is drawn white or black (`minimum-contrast`).
    pub minimum_contrast: f32,
    /// Whether bold text in the first eight colours takes their bright
    /// ones (`bold-color = bright`).
    pub bold_is_bright: bool,
    /// How much of its colour faint text keeps (`faint-opacity`).
    pub faint_opacity: f32,
    /// How many megabytes of scrollback a terminal keeps
    /// (`scrollback-limit-bytes`).
    pub scrollback_mb: u32,
    /// Whether a program may copy to the clipboard and read it (OSC 52;
    /// `clipboard-write`, `clipboard-read`).
    pub clipboard_write: Access,
    pub clipboard_read: Access,
    /// How far the wheel scrolls, times (`mouse-scroll-multiplier`).
    pub scroll_speed: f32,
}

impl Default for Terminal {
    fn default() -> Self {
        Terminal {
            font_family: String::new(),
            line_height: LINE_HEIGHT,
            option_as_alt: false,
            cursor: CursorShape::Block,
            cursor_blink: false,
            copy_on_select: false,
            right_click: RightClick::Menu,
            bell_mark: true,
            bell_bounce: true,
            bell_sound: false,
            progress: true,
            theme_light: String::new(),
            theme_dark: String::new(),
            minimum_contrast: 1.,
            bold_is_bright: false,
            faint_opacity: 0.5,
            scrollback_mb: 50,
            clipboard_write: Access::Allow,
            clipboard_read: Access::Ask,
            scroll_speed: 1.,
        }
    }
}

/// A terminal's line height, in its font's size, unless the settings say.
pub const LINE_HEIGHT: f32 = 1.36;

impl Terminal {
    /// The settings' numbers, kept to what makes sense.
    pub fn line_height(&self) -> f32 {
        within(self.line_height, 1.0..=2.0, LINE_HEIGHT)
    }

    pub fn minimum_contrast(&self) -> f32 {
        within(self.minimum_contrast, 1.0..=21.0, 1.)
    }

    pub fn faint_opacity(&self) -> f32 {
        within(self.faint_opacity, 0.0..=1.0, 0.5)
    }

    pub fn scroll_speed(&self) -> f32 {
        within(self.scroll_speed, 0.25..=10.0, 1.)
    }

    pub fn scrollback_bytes(&self) -> usize {
        self.scrollback_mb.clamp(1, 1000) as usize * 1_000_000
    }
}

fn within(v: f32, range: std::ops::RangeInclusive<f32>, default: f32) -> f32 {
    if v.is_finite() {
        v.clamp(*range.start(), *range.end())
    } else {
        default
    }
}

/// The cursor's shape until a program asks for another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorShape {
    #[default]
    Block,
    Bar,
    Underline,
}

/// What a right-click in a terminal does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RightClick {
    /// The terminal's menu.
    #[default]
    Menu,
    Paste,
    Copy,
    /// Copies what is selected, else pastes.
    CopyOrPaste,
    Nothing,
}

/// Whether a program may do something: always, once the user says, or
/// never.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    #[default]
    Allow,
    Ask,
    Deny,
}

pub const EDITOR_FONT_SIZE: f32 = 13.;
pub const TERMINAL_FONT_SIZE: f32 = 12.5;
/// The sizes a font may be set to.
pub const FONT_SIZES: std::ops::RangeInclusive<f32> = 8.0..=32.0;

impl Values {
    /// The editor's font size, kept within FONT_SIZES.
    pub fn editor_font(&self) -> f32 {
        clamp_font(self.editor_font_size, EDITOR_FONT_SIZE)
    }

    /// The terminal's font size, kept within FONT_SIZES.
    pub fn terminal_font(&self) -> f32 {
        clamp_font(self.terminal_font_size, TERMINAL_FONT_SIZE)
    }
}

fn clamp_font(size: f32, default: f32) -> f32 {
    if size.is_finite() {
        size.clamp(*FONT_SIZES.start(), *FONT_SIZES.end())
    } else {
        default
    }
}

/// The terminal emulator a terminal's screen is kept by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalEmulator {
    /// Ghostty's, libghostty-vt (crates/ghostty); a session it starts is
    /// told it runs in Ghostty.
    #[default]
    Ghostty,
    /// alacritty_terminal's.
    Alacritty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

/// What comes to need the user, as notifications sort it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Event {
    /// An agent asks a question.
    Asking,
    /// An agent finishes.
    Finished,
    /// An environment fails to start.
    Failed,
    /// An environment comes up.
    Ready,
    /// A place asks to be signed in again.
    SignIn,
    /// A newer image is out for an environment.
    Image,
    /// A program in a terminal posts a notification (OSC 9, OSC 777).
    Terminal,
}

impl Event {
    pub const ALL: [Event; 7] = [
        Event::Asking,
        Event::Finished,
        Event::Failed,
        Event::Ready,
        Event::SignIn,
        Event::Image,
        Event::Terminal,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Event::Asking => "An agent asks a question",
            Event::Finished => "An agent finishes",
            Event::Failed => "An environment fails to start",
            Event::Ready => "An environment is ready",
            Event::SignIn => "A place requires sign-in",
            Event::Image => "A newer image is available",
            Event::Terminal => "A terminal program sends a notification",
        }
    }
}

/// Where a kind of notification shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Ways {
    /// In the bell's list.
    pub app: bool,
    /// As the system's own notification.
    pub system: bool,
    /// Counted on the Dock icon.
    pub badge: bool,
    /// With a sound.
    pub sound: bool,
}

impl Ways {
    const fn new(app: bool, system: bool, badge: bool, sound: bool) -> Ways {
        Ways {
            app,
            system,
            badge,
            sound,
        }
    }
}

/// How the app tells the user of what needs them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Notify {
    pub asking: Ways,
    pub finished: Ways,
    pub failed: Ways,
    pub ready: Ways,
    pub sign_in: Ways,
    pub image: Ways,
    pub terminal: Ways,
    /// Places whose events raise nothing but the bell's list.
    pub quiet_places: Vec<String>,
    /// Whether an environment in front of the user, in the window they are
    /// using, raises no system notification.
    pub while_looking: bool,
}

impl Default for Notify {
    fn default() -> Self {
        Notify {
            asking: Ways::new(true, true, true, true),
            finished: Ways::new(true, true, true, false),
            failed: Ways::new(true, true, true, false),
            ready: Ways::new(true, false, false, false),
            sign_in: Ways::new(true, true, false, false),
            image: Ways::new(true, false, false, false),
            terminal: Ways::new(true, true, false, false),
            quiet_places: vec![],
            while_looking: true,
        }
    }
}

impl Notify {
    pub fn ways(&self, e: Event) -> Ways {
        match e {
            Event::Asking => self.asking,
            Event::Finished => self.finished,
            Event::Failed => self.failed,
            Event::Ready => self.ready,
            Event::SignIn => self.sign_in,
            Event::Image => self.image,
            Event::Terminal => self.terminal,
        }
    }

    pub fn ways_mut(&mut self, e: Event) -> &mut Ways {
        match e {
            Event::Asking => &mut self.asking,
            Event::Finished => &mut self.finished,
            Event::Failed => &mut self.failed,
            Event::Ready => &mut self.ready,
            Event::SignIn => &mut self.sign_in,
            Event::Image => &mut self.image,
            Event::Terminal => &mut self.terminal,
        }
    }

    /// Whether a place may raise more than the bell's list.
    pub fn from(&self, place: &str) -> bool {
        !self.quiet_places.iter().any(|p| p == place)
    }
}

pub struct Settings {
    values: Values,
    file: Option<PathBuf>,
}

impl Global for Settings {}

impl Settings {
    /// The settings kept in file, or the defaults.
    pub fn init(file: Option<PathBuf>, cx: &mut App) {
        let values = file
            .as_ref()
            .and_then(|f| std::fs::read(f).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        cx.set_global(Settings { values, file });
        follow(cx);
    }

    pub fn get(cx: &App) -> Values {
        cx.try_global::<Settings>()
            .map(|s| s.values.clone())
            .unwrap_or_default()
    }

    /// Changes the settings and keeps them.
    pub fn update(cx: &mut App, f: impl FnOnce(&mut Values)) {
        if !cx.has_global::<Settings>() {
            Settings::init(None, cx);
        }
        let s = cx.global_mut::<Settings>();
        f(&mut s.values);
        if let (Some(file), Ok(b)) = (&s.file, serde_json::to_vec_pretty(&s.values)) {
            if let Some(dir) = file.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(file, b);
        }
        crate::palette::follow_settings(cx);
        follow(cx);
        cx.refresh_windows();
    }
}

/// What the settings change in the app as a whole: whether typing hides the
/// pointer.
fn follow(cx: &mut App) {
    cx.set_cursor_hide_mode(if Settings::get(cx).hide_pointer {
        gpui::CursorHideMode::OnTypingAndAction
    } else {
        gpui::CursorHideMode::Never
    });
}
