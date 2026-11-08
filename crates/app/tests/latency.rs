//! The latency shown in the status bar, rated as the web UI rates it.

use hangar_app::latency::rating;
use hangar_app::model::Tone;

#[test]
fn a_time_is_rated_by_how_typing_over_it_feels() {
    assert_eq!(rating(12.), (Tone::Good, "signal-high"));
    assert_eq!(rating(149.9), (Tone::Good, "signal-high"));
    assert_eq!(rating(150.), (Tone::Busy, "signal-medium"));
    assert_eq!(rating(399.), (Tone::Busy, "signal-medium"));
    assert_eq!(rating(400.), (Tone::Bad, "signal-low"));
}
