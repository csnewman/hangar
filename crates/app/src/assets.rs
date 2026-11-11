//! The app's own icons, the design's line icons, served to gpui's svg
//! element as icons/<name>.svg.

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

/// Each icon's drawing, on a 24-unit square, stroked.
const ICONS: &[(&str, &str)] = &[
    ("chevron-down", r#"<path d="m6 9 6 6 6-6"/>"#),
    ("chevron-right", r#"<path d="m9 6 6 6-6 6"/>"#),
    ("plus", r#"<path d="M12 5v14M5 12h14"/>"#),
    ("minus", r#"<path d="M5 12h14"/>"#),
    (
        "undo",
        r#"<path d="M9 14 4 9l5-5"/><path d="M4 9h11a5 5 0 0 1 0 10h-3"/>"#,
    ),
    (
        "refresh",
        r#"<path d="M21 12a9 9 0 1 1-3-6.7L21 8"/><path d="M21 3v5h-5"/>"#,
    ),
    (
        "bell",
        r#"<path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9"/><path d="M10.3 21a1.9 1.9 0 0 0 3.4 0"/>"#,
    ),
    (
        "search",
        r#"<circle cx="11" cy="11" r="7"/><path d="m20 20-4-4"/>"#,
    ),
    (
        "project",
        r#"<path d="M4 20h16a1 1 0 0 0 1-1V8a1 1 0 0 0-1-1h-8l-2-3H4a1 1 0 0 0-1 1v14a1 1 0 0 0 1 1z"/>"#,
    ),
    (
        "locate",
        r#"<circle cx="12" cy="12" r="7"/><circle cx="12" cy="12" r="2"/><path d="M12 2v3M12 19v3M2 12h3M19 12h3"/>"#,
    ),
    ("more", r#"<path d="M5 12h.01M12 12h.01M19 12h.01"/>"#),
    ("play", r#"<path d="M7 4v16l13-8z"/>"#),
    (
        "external",
        r#"<path d="M15 3h6v6M10 14 21 3M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/>"#,
    ),
    (
        "copy",
        r#"<rect x="8" y="8" width="13" height="13" rx="2"/><path d="M4 16a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2"/>"#,
    ),
    (
        "stop",
        r#"<rect x="6" y="6" width="12" height="12" rx="1.5"/>"#,
    ),
    (
        "trash",
        r#"<path d="M3 6h18M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/>"#,
    ),
    (
        "layers",
        r#"<path d="m12 2 10 5-10 5L2 7z"/><path d="m2 17 10 5 10-5"/><path d="m2 12 10 5 10-5"/>"#,
    ),
    (
        "branch",
        r#"<path d="M6 3v12"/><circle cx="18" cy="6" r="3"/><circle cx="6" cy="18" r="3"/><path d="M18 9a9 9 0 0 1-9 9"/>"#,
    ),
    (
        "eye",
        r#"<path d="M2.06 12.35a1 1 0 0 1 0-.7 10.75 10.75 0 0 1 19.88 0 1 1 0 0 1 0 .7 10.75 10.75 0 0 1-19.88 0"/><circle cx="12" cy="12" r="3"/>"#,
    ),
    (
        "signal-high",
        r#"<path d="M2 20h.01M7 20v-4M12 20v-8M17 20V8"/>"#,
    ),
    ("signal-medium", r#"<path d="M2 20h.01M7 20v-4M12 20v-8"/>"#),
    ("signal-low", r#"<path d="M2 20h.01M7 20v-4"/>"#),
    (
        "folder",
        r#"<path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2z"/>"#,
    ),
    (
        "folder-open",
        r#"<path d="m6 14 1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6a2 2 0 0 1-1.94 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.69.9l.81 1.2a2 2 0 0 0 1.67.9H18a2 2 0 0 1 2 2v2"/>"#,
    ),
    (
        "file",
        r#"<path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7z"/><path d="M14 2v4a2 2 0 0 0 2 2h4"/>"#,
    ),
    (
        "filesystem",
        r#"<rect x="2" y="13" width="20" height="8" rx="2"/><path d="M5 13 7.5 4h9L19 13"/><path d="M6 17h.01M10 17h.01"/>"#,
    ),
    (
        "commit",
        r#"<path d="M12 4v8M8 8h8M8 18h8"/><rect x="3" y="2" width="18" height="20" rx="2"/>"#,
    ),
    (
        "structure",
        r#"<path d="M3 5h5M3 12h5M3 19h5M12 5h9M14 12h7M14 19h7"/>"#,
    ),
    (
        "terminal",
        r#"<rect x="3" y="4" width="18" height="16" rx="2"/><path d="m7 9 3 3-3 3M13 15h4"/>"#,
    ),
    (
        "problems",
        r#"<path d="M12 3 2 21h20z"/><path d="M12 10v5M12 18v.01"/>"#,
    ),
    (
        "git",
        r#"<circle cx="6" cy="5" r="2"/><circle cx="6" cy="19" r="2"/><circle cx="18" cy="7" r="2"/><path d="M6 7v10M18 9c0 4-5 5-11 8"/>"#,
    ),
    (
        "docker",
        r#"<path d="M21 8l-9-5-9 5v8l9 5 9-5z"/><path d="M3 8l9 5 9-5M12 13v8"/>"#,
    ),
    (
        "log",
        r#"<path d="M8 6h13M8 12h13M8 18h13M3 6h1M3 12h1M3 18h1"/>"#,
    ),
    (
        "agents",
        r#"<path d="M12 3l1.8 4.7L18.5 9.5l-4.7 1.8L12 16l-1.8-4.7L5.5 9.5l4.7-1.8z"/><path d="M19 15l.8 2.2L22 18l-2.2.8L19 21l-.8-2.2L16 18l2.2-.8z"/>"#,
    ),
    (
        "ports",
        r#"<circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3a14 14 0 0 1 0 18M12 3a14 14 0 0 0 0 18"/>"#,
    ),
    (
        "pin",
        r#"<path d="M12 17v5"/><path d="M9 10.8a2 2 0 0 1-1.1 1.8l-1.8.9A2 2 0 0 0 5 15.2V16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-.8a2 2 0 0 0-1.1-1.8l-1.8-.9a2 2 0 0 1-1.1-1.8V7a1 1 0 0 1 1-1 2 2 0 0 0 0-4H8a2 2 0 0 0 0 4 1 1 0 0 1 1 1z"/>"#,
    ),
    (
        "bulb",
        r#"<path d="M15 14c.2-1 .7-1.7 1.5-2.5 1-.9 1.5-2.2 1.5-3.5A6 6 0 0 0 6 8c0 1 .2 2.2 1.5 3.5.7.7 1.3 1.5 1.5 2.5"/><path d="M9 18h6M10 22h4"/>"#,
    ),
    (
        "wrench",
        r#"<path d="M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.8-3.8a6 6 0 0 1-7.9 7.9l-6.9 6.9a2.1 2.1 0 0 1-3-3l6.9-6.9a6 6 0 0 1 7.9-7.9z"/>"#,
    ),
];

/// The icons, for Application::with_assets.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let Some(name) = path
            .strip_prefix("icons/")
            .and_then(|p| p.strip_suffix(".svg"))
        else {
            return Ok(None);
        };
        Ok(ICONS.iter().find(|(n, _)| *n == name).map(|(_, body)| {
            Cow::Owned(
                format!(
                    r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="black" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">{body}</svg>"#
                )
                .into_bytes(),
            )
        }))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(match path.trim_end_matches('/') {
            "icons" => ICONS
                .iter()
                .map(|(n, _)| format!("icons/{n}.svg").into())
                .collect(),
            _ => vec![],
        })
    }
}

/// An icon's path, for gpui's svg element.
pub fn icon(name: &str) -> SharedString {
    format!("icons/{name}.svg").into()
}
