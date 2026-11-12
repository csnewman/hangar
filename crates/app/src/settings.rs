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
            notify: Notify::default(),
        }
    }
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
}

impl Event {
    pub const ALL: [Event; 6] = [
        Event::Asking,
        Event::Finished,
        Event::Failed,
        Event::Ready,
        Event::SignIn,
        Event::Image,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Event::Asking => "An agent asks a question",
            Event::Finished => "An agent finishes",
            Event::Failed => "An environment fails",
            Event::Ready => "An environment is ready",
            Event::SignIn => "A place needs signing in",
            Event::Image => "A newer image is available",
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
        cx.refresh_windows();
    }
}
