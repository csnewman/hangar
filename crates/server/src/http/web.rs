//! The web UI, built into the binary so the control plane is one program
//! with no Node.js beside it. `npm run build` in web/ writes it to
//! internal/webui/dist/app; a binary built before that has no UI and says
//! so. A debug build reads the files from there as it serves them.

use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};

#[derive(rust_embed::RustEmbed)]
#[folder = "../../internal/webui/dist/app/"]
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
    let body = match Ui::get("index.html") {
        Some(f) => f.data.into_owned(),
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
