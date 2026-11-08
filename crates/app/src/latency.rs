//! How long the way to an environment and back takes: a request to its
//! Code service, which answers ping at once, timed every few seconds
//! through Hangar's server and the environment's worker, and rated by how
//! typing over it will feel.

use std::time::{Duration, Instant};

use gpui::{App, AsyncApp, Entity, Task, WeakEntity};
use serde_json::json;

use crate::model::Tone;
use crate::workspace::{State, Workspace};

/// How often the way is timed.
pub const EVERY: Duration = Duration::from_secs(5);

/// A time's rating, as the web UI's status bar has it: under 150 ms good,
/// under 400 ms slow, else bad; with the signal icon it shows.
pub fn rating(ms: f64) -> (Tone, &'static str) {
    if ms < 150. {
        (Tone::Good, "signal-high")
    } else if ms < 400. {
        (Tone::Busy, "signal-medium")
    } else {
        (Tone::Bad, "signal-low")
    }
}

/// Times the workspace's connection every few seconds while it is up,
/// handing each time, in milliseconds, to took: None while it is not up or
/// the request fails. Ends when the view it reports to goes.
pub fn watch<V: 'static>(
    workspace: Entity<Workspace>,
    view: WeakEntity<V>,
    took: impl Fn(&mut V, Option<f64>, &mut gpui::Context<V>) + 'static,
    cx: &mut App,
) -> Task<()> {
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            let code = workspace.read_with(cx, |w, _| {
                matches!(w.state(), State::Ready)
                    .then(|| w.primary())
                    .flatten()
            });
            let ms = match code {
                Some(code) => {
                    let start = Instant::now();
                    code.call("ping", json!({}))
                        .await
                        .ok()
                        .map(|_| start.elapsed().as_secs_f64() * 1000.)
                }
                None => None,
            };
            if view.update(cx, |v, cx| took(v, ms, cx)).is_err() {
                return;
            }
            cx.background_executor().timer(EVERY).await;
        }
    })
}
