//! How often each of the main views has drawn, for the dev build's control
//! port to report (`stats`): the shell's count is the window's frames, as
//! every frame draws it. Counting is nothing without the `control` feature.

#[cfg(feature = "control")]
use std::sync::atomic::{AtomicU64, Ordering};

pub const SHELL: usize = 0;
pub const ENV_TAB: usize = 1;
pub const TERMINAL: usize = 2;
pub const PROJECT: usize = 3;
pub const TERMINAL_PAINT: usize = 4;
/// The environment tab's parts drawn as views of their own, all together.
pub const TAB_PARTS: usize = 5;
#[cfg(feature = "control")]
const NAMES: [&str; 6] = [
    "shell",
    "env_tab",
    "terminal",
    "project",
    "terminal_paint",
    "tab_parts",
];

#[cfg(feature = "control")]
static COUNTS: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];
#[cfg(feature = "control")]
static SINCE: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// One more of a view's draws.
#[inline]
pub fn count(_view: usize) {
    #[cfg(feature = "control")]
    COUNTS[_view].fetch_add(1, Ordering::Relaxed);
}

/// The counts since this was last asked, and over how long, each counted
/// from zero again.
#[cfg(feature = "control")]
pub fn take() -> serde_json::Value {
    let mut since = SINCE.lock().unwrap();
    let seconds = since.map_or(0., |at| at.elapsed().as_secs_f64());
    *since = Some(std::time::Instant::now());
    let mut out = serde_json::json!({ "seconds": seconds });
    for (i, name) in NAMES.iter().enumerate() {
        out[*name] = COUNTS[i].swap(0, Ordering::Relaxed).into();
    }
    out
}
