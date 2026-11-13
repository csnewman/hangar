//! A small registry speaking just enough of the distribution API, for the
//! pull and store tests: signing in with a token from its realm or with
//! Basic credentials, a multi-platform index, gzip and zstd layers, and
//! blobs sent on to a store that must not be given the registry's
//! credentials.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

use hangar_images::guest_arch;
use sha2::{Digest, Sha256};

pub fn digest(b: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(b)))
}

/// A layer: a tar archive of (path, contents).
pub fn layer(files: &[(&str, &str)]) -> Vec<u8> {
    let mut t = tar::Builder::new(Vec::new());
    for (p, body) in files {
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(0o644);
        h.set_mtime(1700000000);
        t.append_data(&mut h, p, body.as_bytes()).unwrap();
    }
    t.into_inner().unwrap()
}

/// What the registry holds: blobs and manifests by digest, tags.
pub struct Store {
    pub blobs: HashMap<String, Vec<u8>>,
    pub tags: HashMap<String, String>,
    pub index: String,
    pub bottom: Vec<u8>,
    pub top: Vec<u8>,
}

pub fn store() -> Store {
    let bottom = layer(&[("etc/os-release", "ID=test\n"), ("etc/a", "base\n")]);
    let top = layer(&[("etc/a", "changed\n"), ("etc/.wh.os-release", "")]);
    let gz = {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(&bottom).unwrap();
        e.finish().unwrap()
    };
    let zst = zstd::encode_all(&top[..], 3).unwrap();
    let config = br#"{"architecture":"arm64","os":"linux"}"#.to_vec();
    let mut blobs = HashMap::new();
    let desc = |media: &str, b: &[u8]| serde_json::json!({"mediaType": media, "digest": digest(b), "size": b.len()});
    let manifest = serde_json::to_vec(&serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": desc("application/vnd.oci.image.config.v1+json", &config),
        "layers": [
            desc("application/vnd.oci.image.layer.v1.tar+gzip", &gz),
            desc("application/vnd.oci.image.layer.v1.tar+zstd", &zst),
        ],
    }))
    .unwrap();
    // Another platform's, which is never fetched.
    let other = br#"{"not":"fetched"}"#.to_vec();
    let mut entry = desc("application/vnd.oci.image.manifest.v1+json", &manifest);
    entry["platform"] = serde_json::json!({"os": "linux", "architecture": guest_arch(), "variant": if guest_arch() == "arm64" { "v8" } else { "" }});
    let mut wrong = desc("application/vnd.oci.image.manifest.v1+json", &other);
    wrong["platform"] = serde_json::json!({"os": "linux", "architecture": "s390x"});
    let index = serde_json::to_vec(&serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.index.v1+json",
        "manifests": [wrong, entry],
    }))
    .unwrap();
    let index_digest = digest(&index);
    for b in [config, gz, zst, manifest, index] {
        blobs.insert(digest(&b), b);
    }
    let mut tags = HashMap::new();
    tags.insert("v1".to_string(), index_digest.clone());
    Store {
        blobs,
        tags,
        index: index_digest,
        bottom,
        top,
    }
}

/// How the registry asks to be signed in to.
#[derive(Clone, Copy)]
pub enum Sign {
    /// A token from its realm, given for user:secret.
    Bearer,
    /// Basic credentials, hangar-worker and a worker's credential, as
    /// Hangar's own registry asks, beneath a path of its own.
    Basic,
}

/// The requests the registry saw: method, path, Authorization.
pub type Seen = Arc<Mutex<Vec<(String, String, Option<String>)>>>;

pub fn serve(store: Store, sign: Sign) -> (String, Seen) {
    let ln = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = ln.local_addr().unwrap().to_string();
    let seen: Seen = Arc::default();
    let log = seen.clone();
    let store = Arc::new(store);
    let base = addr.clone();
    thread::spawn(move || {
        for conn in ln.incoming() {
            let Ok(mut conn) = conn else { return };
            let (store, log, base) = (store.clone(), log.clone(), base.clone());
            thread::spawn(move || {
                let mut r = BufReader::new(conn.try_clone().unwrap());
                loop {
                    let mut first = String::new();
                    if r.read_line(&mut first).unwrap_or(0) == 0 {
                        return;
                    }
                    let mut auth = None;
                    loop {
                        let mut h = String::new();
                        r.read_line(&mut h).unwrap();
                        if h.trim().is_empty() {
                            break;
                        }
                        if let Some(v) = h
                            .strip_prefix("authorization: ")
                            .or(h.strip_prefix("Authorization: "))
                        {
                            auth = Some(v.trim().to_string());
                        }
                    }
                    let mut parts = first.split(' ');
                    let (method, path) = (
                        parts.next().unwrap().to_string(),
                        parts.next().unwrap().to_string(),
                    );
                    log.lock()
                        .unwrap()
                        .push((method.clone(), path.clone(), auth.clone()));
                    let (status, headers, body) =
                        answer(&store, sign, &base, &method, &path, auth.as_deref());
                    let mut head =
                        format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n", body.len());
                    for (k, v) in headers {
                        head.push_str(&format!("{k}: {v}\r\n"));
                    }
                    head.push_str("\r\n");
                    conn.write_all(head.as_bytes()).unwrap();
                    if method != "HEAD" {
                        conn.write_all(&body).unwrap();
                    }
                }
            });
        }
    });
    (addr, seen)
}

fn answer(
    store: &Store,
    sign: Sign,
    base: &str,
    method: &str,
    path: &str,
    auth: Option<&str>,
) -> (&'static str, Vec<(String, String)>, Vec<u8>) {
    let none = Vec::new;
    if let Some(d) = path.strip_prefix("/cdn/") {
        // The store a blob is sent on to is given nothing of the registry's.
        if auth.is_some() {
            return ("403 Forbidden", none(), b"credentials sent on".to_vec());
        }
        return match store.blobs.get(d) {
            Some(b) => ("200 OK", none(), b.clone()),
            None => ("404 Not Found", none(), vec![]),
        };
    }
    if let Some(q) = path.strip_prefix("/token?") {
        let good = format!("Basic {}", base64_of("user:secret"));
        if auth != Some(good.as_str()) || !q.contains("scope=repository%3Atest%2Fapp%3Apull") {
            return ("401 Unauthorized", none(), vec![]);
        }
        return ("200 OK", none(), br#"{"token":"t0k"}"#.to_vec());
    }
    let (prefix, want) = match sign {
        Sign::Bearer => ("/v2/", "Bearer t0k".to_string()),
        Sign::Basic => (
            "/api/worker/v1/registry/v2/",
            format!("Basic {}", base64_of("hangar-worker:hgw_cred")),
        ),
    };
    let Some(rest) = path.strip_prefix(prefix) else {
        return ("404 Not Found", none(), vec![]);
    };
    if auth != Some(want.as_str()) {
        let challenge = match sign {
            Sign::Bearer => format!(
                r#"Bearer realm="http://{base}/token",service="test-registry",scope="repository:test/app:pull""#
            ),
            Sign::Basic => r#"Basic realm="Hangar", charset="UTF-8""#.to_string(),
        };
        return (
            "401 Unauthorized",
            vec![("WWW-Authenticate".into(), challenge)],
            vec![],
        );
    }
    let rest = rest.strip_prefix("test/app/").unwrap_or("");
    if let Some(r) = rest.strip_prefix("manifests/") {
        let d = store.tags.get(r).cloned().unwrap_or(r.to_string());
        return match store.blobs.get(&d) {
            Some(b) => (
                "200 OK",
                vec![("Docker-Content-Digest".into(), d.clone())],
                if method == "HEAD" { vec![] } else { b.clone() },
            ),
            None => (
                "404 Not Found",
                none(),
                br#"{"errors":[{"code":"MANIFEST_UNKNOWN","message":"manifest unknown"}]}"#
                    .to_vec(),
            ),
        };
    }
    if let Some(d) = rest.strip_prefix("blobs/") {
        return (
            "307 Temporary Redirect",
            vec![("Location".into(), format!("http://{base}/cdn/{d}"))],
            vec![],
        );
    }
    ("404 Not Found", none(), vec![])
}

pub fn base64_of(s: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(s)
}

pub fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hangar-pull-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The image the layers make, merged straight from them.
pub fn want_image(s: &Store, dir: &std::path::Path) -> Vec<u8> {
    let out = dir.join("want.erofs");
    let mut layers = vec![s.top.clone(), s.bottom.clone()].into_iter();
    hangar_images::build_erofs(&out, || {
        Ok(layers
            .next()
            .map(|l| Box::new(std::io::Cursor::new(l)) as Box<dyn std::io::Read>))
    })
    .unwrap();
    std::fs::read(out).unwrap()
}
