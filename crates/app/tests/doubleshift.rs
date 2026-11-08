use std::time::{Duration, Instant};

use gpui::Modifiers;
use hangar_app::doubleshift::DoubleShift;

fn shift(down: bool) -> Modifiers {
    Modifiers {
        shift: down,
        ..Default::default()
    }
}

#[test]
fn shift_twice_quickly_is_a_double_press() {
    let t = Instant::now();
    let mut d = DoubleShift::default();
    assert!(!d.modifiers(&shift(true), t));
    assert!(!d.modifiers(&shift(false), t + Duration::from_millis(60)));
    assert!(d.modifiers(&shift(true), t + Duration::from_millis(150)));
    // A third press starts over.
    d.modifiers(&shift(false), t + Duration::from_millis(200));
    assert!(!d.modifiers(&shift(true), t + Duration::from_millis(260)));
}

#[test]
fn too_slow_or_a_key_between_is_not() {
    let t = Instant::now();
    let mut d = DoubleShift::default();
    d.modifiers(&shift(true), t);
    d.modifiers(&shift(false), t + Duration::from_millis(50));
    assert!(!d.modifiers(&shift(true), t + Duration::from_millis(900)));

    let mut d = DoubleShift::default();
    d.modifiers(&shift(true), t);
    d.key();
    d.modifiers(&shift(false), t + Duration::from_millis(50));
    assert!(!d.modifiers(&shift(true), t + Duration::from_millis(100)));
}

#[test]
fn shift_with_another_modifier_is_not() {
    let t = Instant::now();
    let mut d = DoubleShift::default();
    d.modifiers(&shift(true), t);
    d.modifiers(&shift(false), t + Duration::from_millis(50));
    let both = Modifiers {
        shift: true,
        platform: true,
        ..Default::default()
    };
    assert!(!d.modifiers(&both, t + Duration::from_millis(100)));
}
