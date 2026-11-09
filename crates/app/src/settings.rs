//! The app's own settings, kept in settings.json in its config folder.

use std::path::PathBuf;

use gpui::{App, Global};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Values {
    /// Whether pressing Shift twice opens Search Everywhere, as in
    /// IntelliJ.
    pub double_shift: bool,
    /// Whether the file shown is selected in the Project window as it is
    /// shown, as IntelliJ's Always Select Opened File.
    pub select_opened: bool,
}

impl Default for Values {
    fn default() -> Self {
        Values {
            double_shift: true,
            select_opened: true,
        }
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
            .map(|s| s.values)
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
        cx.refresh_windows();
    }
}
