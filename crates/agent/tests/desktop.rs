use hangar_agent::desktop::clamp_size;

/// A size asked for is kept within what a desktop can be, with even sides.
#[test]
fn sizes_are_clamped_and_even() {
    assert_eq!(clamp_size(1281, 721), (1280, 720));
    assert_eq!(clamp_size(10, 10), (640, 400));
    assert_eq!(clamp_size(10000, 10000), (3840, 2160));
}
