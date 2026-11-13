//! Pulls from a small registry speaking just enough of the distribution
//! API: signing in with a token from its realm or with Basic credentials,
//! a multi-platform index, gzip and zstd layers, and blobs sent on to a
//! store that must not be given the registry's credentials.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

use hangar_images::{
    Auth, Client, HangarRegistry, Progress, Reference, Stage, build_erofs, guest_arch,
};
use sha2::{Digest, Sha256};

fn digest(b: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(b)))
}

/// A layer: a tar archive of (path, contents).
fn layer(files: &[(&str, &str)]) -> Vec<u8> {
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
struct Store {
    blobs: HashMap<String, Vec<u8>>,
    tags: HashMap<String, String>,
    index: String,
    bottom: Vec<u8>,
    top: Vec<u8>,
}

fn store() -> Store {
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
enum Sign {
    /// A token from its realm, given for user:secret.
    Bearer,
    /// Basic credentials, hangar-worker and a worker's credential, as
    /// Hangar's own registry asks, beneath a path of its own.
    Basic,
}

/// The requests the registry saw: method, path, Authorization.
type Seen = Arc<Mutex<Vec<(String, String, Option<String>)>>>;

fn serve(store: Store, sign: Sign) -> (String, Seen) {
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

fn base64_of(s: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(s)
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hangar-pull-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The image the layers make, merged straight from them.
fn want_image(s: &Store, dir: &std::path::Path) -> Vec<u8> {
    let out = dir.join("want.erofs");
    let mut layers = vec![s.top.clone(), s.bottom.clone()].into_iter();
    build_erofs(&out, || {
        Ok(layers
            .next()
            .map(|l| Box::new(std::io::Cursor::new(l)) as Box<dyn Read>))
    })
    .unwrap();
    std::fs::read(out).unwrap()
}

#[test]
fn a_pull_signs_in_picks_its_platform_and_makes_the_image() {
    let s = store();
    let index = s.index.clone();
    let dir = scratch("bearer");
    let want = want_image(&s, &dir);
    let (addr, seen) = serve(s, Sign::Bearer);
    let pw = dir.join("password");
    std::fs::write(&pw, "secret\n").unwrap();
    let auth = HashMap::from([(
        addr.clone(),
        Auth {
            username: "user".into(),
            password_file: pw,
        },
    )]);
    let client = Client::new(auth, None);
    let r = Reference::parse(&format!("{addr}/test/app:v1")).unwrap();

    assert_eq!(client.resolve(&r).unwrap(), index, "the index's digest");

    let mut said: Vec<Progress> = vec![];
    let out = dir.join("image.erofs");
    client
        .pull(&r, &index, &dir.join("blobs"), &out, &mut |p| said.push(p))
        .unwrap();
    assert_eq!(
        std::fs::read(&out).unwrap(),
        want,
        "the image the layers make"
    );

    let last_download = said.iter().rfind(|p| p.stage == Stage::Download).unwrap();
    assert_eq!(last_download.done, last_download.total);
    let last = said.last().unwrap();
    assert_eq!((last.stage, last.layer, last.layers), (Stage::Unpack, 2, 2));
    assert_eq!(last.done, last.total);

    let seen = seen.lock().unwrap();
    assert!(
        seen.iter()
            .all(|(_, p, _)| !p.contains("/manifests/sha256:")
                || !p.ends_with(&digest(br#"{"not":"fetched"}"#))),
        "another platform's manifest is not fetched"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn hangars_registry_is_reached_through_the_server_with_the_workers_credential() {
    let s = store();
    let index = s.index.clone();
    let dir = scratch("hangar");
    let want = want_image(&s, &dir);
    let (addr, _) = serve(s, Sign::Basic);
    let hangar = HangarRegistry {
        host: "Registry.Hangar.Test".into(),
        url: format!("http://{addr}/api/worker/v1/registry"),
        credential: "hgw_cred".into(),
    };
    let client = Client::new(HashMap::new(), Some(hangar));
    let r = Reference::parse("registry.hangar.test/test/app:v1").unwrap();
    assert_eq!(client.resolve(&r).unwrap(), index);
    let out = dir.join("image.erofs");
    client
        .pull(&r, &index, &dir.join("blobs"), &out, &mut |_| {})
        .unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), want);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn what_cannot_be_pulled_says_why() {
    let s = store();
    let dir = scratch("fail");
    let (addr, _) = serve(s, Sign::Bearer);
    let pw = dir.join("password");
    std::fs::write(&pw, "wrong").unwrap();
    let auth = HashMap::from([(
        addr.clone(),
        Auth {
            username: "user".into(),
            password_file: pw.clone(),
        },
    )]);
    let r = Reference::parse(&format!("{addr}/test/app:v1")).unwrap();
    let err = Client::new(auth, None).resolve(&r).unwrap_err().to_string();
    assert!(err.contains("token"), "a refused sign-in: {err}");

    std::fs::write(&pw, "secret").unwrap();
    let auth = HashMap::from([(
        addr.clone(),
        Auth {
            username: "user".into(),
            password_file: pw,
        },
    )]);
    let client = Client::new(auth, None);
    let missing = Reference::parse(&format!("{addr}/test/app:v2")).unwrap();
    let err = client.resolve(&missing).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound, "{err}");
    let wrong = format!("sha256:{}", "0".repeat(64));
    let err = client
        .pull(
            &r,
            &wrong,
            &dir.join("blobs"),
            &dir.join("x.erofs"),
            &mut |_| {},
        )
        .unwrap_err();
    assert!(err.to_string().contains("MANIFEST_UNKNOWN"), "{err}");
    assert!(!dir.join("x.erofs").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn references_read_as_dockers() {
    let r = |s: &str| Reference::parse(s).map(|r| r.to_string());
    assert_eq!(r("ubuntu").unwrap(), "docker.io/library/ubuntu:latest");
    assert_eq!(r("ubuntu:24.04").unwrap(), "docker.io/library/ubuntu:24.04");
    assert_eq!(r("csnewman/app").unwrap(), "docker.io/csnewman/app:latest");
    assert_eq!(
        r("index.docker.io/library/ubuntu").unwrap(),
        "docker.io/library/ubuntu:latest"
    );
    assert_eq!(
        r("ghcr.io/csnewman/hangar/base:ubuntu-26.04").unwrap(),
        "ghcr.io/csnewman/hangar/base:ubuntu-26.04"
    );
    assert_eq!(
        r("localhost:5000/app").unwrap(),
        "localhost:5000/app:latest"
    );
    assert_eq!(r("localhost/app").unwrap(), "localhost/app:latest");
    let d = format!("sha256:{}", "a".repeat(64));
    assert_eq!(
        r(&format!("app:v1@{d}")).unwrap(),
        format!("docker.io/library/app@{d}"),
        "a digest wins"
    );
    assert!(r("App").is_err(), "capitals");
    assert!(r("app:bad tag").is_err());
    assert!(r("app@sha256:short").is_err());
    let local = Reference::parse("127.0.0.1:5000/app").unwrap();
    assert!(local.is_local());
    assert!(!Reference::parse("ghcr.io/x/y").unwrap().is_local());
    assert_eq!(
        Reference::parse("ubuntu").unwrap().api_host(),
        "registry-1.docker.io"
    );
}
