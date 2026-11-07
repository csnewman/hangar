//! The app's own icons, the design's line icons, served to gpui's svg
//! element as icons/<name>.svg.

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

/// Each icon's drawing, on a 24-unit square, stroked.
const ICONS: &[(&str, &str)] = &[
    ("chevron-down", r#"<path d="m6 9 6 6 6-6"/>"#),
    ("chevron-right", r#"<path d="m9 6 6 6-6 6"/>"#),
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
