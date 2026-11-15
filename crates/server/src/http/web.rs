//! The web UI, built into the binary so the control plane is one program
//! with no Node.js beside it. `npm run build` in web/ writes it to
//! web/dist; a binary built before that has no UI and says so. A debug
//! build reads the files from there as it serves them.
//!
//! A page open while the server is upgraded is the older UI, whose next
//! asset may be gone: the page names its build (a meta tag, `build`), every
//! answer of the web UI's API names the server's (UI_HEADER), and the page
//! offers to reload when the two differ. An asset the UI does not have is
//! 404, never the page, which a browser would try to run as the asset.

use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};

/// The response header that names the UI's build.
pub const UI_HEADER: &str = "hangar-ui";

/// The UI's build: the start of a digest of its index.html, which names
/// every asset by its content, so any change to the UI changes it. Empty
/// for a binary with no UI.
pub fn build() -> String {
    #[cfg(not(debug_assertions))]
    {
        static BUILD: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        BUILD.get_or_init(read_build).clone()
    }
    #[cfg(debug_assertions)]
    read_build()
}

fn read_build() -> String {
    Ui::get("index.html")
        .map(|f| {
            let d = Sha256::digest(&f.data);
            d[..6].iter().map(|b| format!("{b:02x}")).collect()
        })
        .unwrap_or_default()
}

/// index.html with the build named in it.
fn page(index: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(index);
    let meta = format!(
        "<meta name=\"hangar-ui\" content=\"{}\" />\n  </head>",
        build()
    );
    text.replacen("</head>", &meta, 1).into_bytes()
}

/// Names the UI's build in an answer.
pub fn name_build(resp: &mut Response) {
    if let Ok(v) = HeaderValue::from_str(&build())
        && !v.is_empty()
    {
        resp.headers_mut().insert(UI_HEADER, v);
    }
}

#[derive(rust_embed::RustEmbed)]
#[folder = "../../web/dist/"]
#[allow_missing = true]
struct Ui;

const NOT_BUILT: &str = "<!doctype html>
<title>Hangar</title>
<p>The web UI is not built into this binary. Run <code>npm run build</code> in
<code>web/</code> and rebuild, or use the development stack in
<code>compose.yaml</code>, which serves the UI from Vite.</p>
";

/// A request's path as a name in the UI's files: cleaned of `.`, `..` and
/// empty segments, without a leading slash.
fn name(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for p in path.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}

/// Serves the single-page app: a path that names a file serves the file,
/// and any other serves index.html, so the app's own routes survive a
/// reload.
pub async fn serve(uri: Uri) -> Response {
    let name = name(uri.path());
    if !name.is_empty()
        && let Some(f) = Ui::get(&name)
    {
        let mut r = f.data.into_owned().into_response();
        let h = r.headers_mut();
        if let Ok(v) = HeaderValue::from_str(f.metadata.mimetype()) {
            h.insert(header::CONTENT_TYPE, v);
        }
        // Vite puts a content hash in every asset's name, so an asset never
        // changes under the same URL.
        if name.starts_with("assets/") {
            h.insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=31536000, immutable"),
            );
        }
        return r;
    }
    if name.starts_with("assets/") {
        return (StatusCode::NOT_FOUND, "no such asset").into_response();
    }
    let body = match Ui::get("index.html") {
        Some(f) => page(&f.data),
        None => NOT_BUILT.as_bytes().to_vec(),
    };
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}
