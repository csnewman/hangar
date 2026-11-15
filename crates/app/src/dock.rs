//! The Dock icon's badge and bounce, and the sound notifications make:
//! AppKit's, on macOS; nothing elsewhere.

/// Shows count on the Dock icon, or nothing for none.
#[cfg(target_os = "macos")]
pub fn set_badge(count: usize) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    use objc2_foundation::NSString;
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let tile = NSApplication::sharedApplication(mtm).dockTile();
    let label = (count > 0).then(|| NSString::from_str(&count.to_string()));
    tile.setBadgeLabel(label.as_deref());
}

#[cfg(not(target_os = "macos"))]
pub fn set_badge(_count: usize) {}

/// Bounces the Dock icon once, for something the user should look at
/// while the app is behind others: a terminal's bell.
#[cfg(target_os = "macos")]
pub fn attention() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSRequestUserAttentionType};
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    NSApplication::sharedApplication(mtm)
        .requestUserAttention(NSRequestUserAttentionType::InformationalRequest);
}

#[cfg(not(target_os = "macos"))]
pub fn attention() {}

/// Plays the sound a notification asking for the user makes: the system's
/// own Glass.
#[cfg(target_os = "macos")]
pub fn chime() {
    use objc2_app_kit::NSSound;
    use objc2_foundation::NSString;
    if let Some(s) = NSSound::soundNamed(&NSString::from_str("Glass")) {
        s.play();
    }
}

#[cfg(not(target_os = "macos"))]
pub fn chime() {}
