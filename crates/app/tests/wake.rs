//! Telling a sleep from a slow moment by the clocks.

use std::time::Duration;

use hangar_app::wake::slept;

#[test]
fn a_sleep_is_the_wall_clock_running_ahead() {
    let s = Duration::from_secs;
    assert!(!slept(s(5), s(5)), "awake");
    assert!(!slept(s(5), s(7)), "a clock adjustment");
    assert!(slept(s(5), s(3605)), "an hour asleep");
    assert!(!slept(s(5), s(0)), "the clock set back");
}
