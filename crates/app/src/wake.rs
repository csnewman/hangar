//! Noticing the machine wake from sleep: the wall clock runs on while it
//! sleeps and the monotonic one stops, so a tick that finds the first far
//! ahead of the second has slept through. Connections that wait on a
//! timer (the places' event streams) are then made again at once rather
//! than when their timers run out; the sockets check themselves by the
//! wall clock (ws::Liveness).

use std::time::{Duration, Instant, SystemTime};

use gpui::{App, Entity};

use crate::places::Places;

/// How often the clocks are compared.
const TICK: Duration = Duration::from_secs(5);

/// How much further the wall clock may run than the monotonic one before
/// it is taken as a sleep: more than a clock adjustment or a busy moment.
const SLEPT: Duration = Duration::from_secs(10);

/// Whether, over a tick, the wall clock's lead says the machine slept.
pub fn slept(monotonic: Duration, wall: Duration) -> bool {
    wall.saturating_sub(monotonic) > SLEPT
}

/// Watches for the machine waking, then reconnects every place.
pub fn watch(places: Entity<Places>, cx: &mut App) {
    let places = places.downgrade();
    cx.spawn(async move |cx| {
        loop {
            let (mono, wall) = (Instant::now(), SystemTime::now());
            cx.background_executor().timer(TICK).await;
            let wall = SystemTime::now().duration_since(wall).unwrap_or_default();
            if slept(mono.elapsed(), wall) && places.update(cx, |p, cx| p.reconnect(cx)).is_err() {
                return;
            }
        }
    })
    .detach();
}
