//! Pulling an image from its registry over the distribution API: its
//! manifest, by digest, for this machine's platform, and its layers, each
//! checked against its digest as it arrives, then merged straight into an
//! EROFS image.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::reference::Reference;

/// How a worker signs in to a registry to pull from it. A registry with
/// none is pulled from anonymously.
#[derive(Debug, Clone)]
pub struct Auth {
    pub username: String,
    /// Holds the password or token.
    pub password_file: PathBuf,
}

/// Hangar's own registry: images named on host are pulled from url,
/// beneath which is the distribution API's /v2/, with the worker's
/// credential.
#[derive(Debug, Clone)]
pub struct HangarRegistry {
    pub host: String,
    pub url: String,
    pub credential: String,
}

/// A fetch's stages: a pull downloads, then unpacks; an image with a local
/// source is copied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Copy,
    Download,
    Unpack,
}

impl Stage {
    /// The stage's name, as the worker reports it.
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Copy => "copy",
            Stage::Download => "download",
            Stage::Unpack => "unpack",
        }
    }
}

/// How far a fetch has got: its stage, and bytes done of the stage's total,
/// where it has one. Unpacking counts the compressed bytes of the layers
/// read so far, layer being the one in hand, from the top.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub stage: Stage,
    pub done: u64,
    pub total: u64,
    pub layer: usize,
    pub layers: usize,
}

/// How often a pull says how far it has got.
const REPORT_EVERY: Duration = Duration::from_millis(250);

/// The manifests a registry may answer with, best first.
const ACCEPT: &str = "application/vnd.oci.image.index.v1+json, \
application/vnd.docker.distribution.manifest.list.v2+json, \
application/vnd.oci.image.manifest.v1+json, \
application/vnd.docker.distribution.manifest.v2+json";

fn other(msg: String) -> io::Error {
    io::Error::other(msg)
}

/// The architecture a guest shares with this machine, as images name it.
pub fn guest_arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "amd64",
        a => a,
    }
}

/// Pulls images and looks up what their references name.
pub struct Client {
    agent: ureq::Agent,
    auth: HashMap<String, Auth>,
    hangar: Option<HangarRegistry>,
}

/// One repository at its registry, signed in as it asked.
struct Repo<'a> {
    agent: &'a ureq::Agent,
    /// The API's root: scheme, host and path, ending in /v2.
    base: String,
    name: String,
    creds: Option<(String, String)>,
    /// What to send: a token a Bearer challenge gave, or the credentials
    /// a Basic one asked for.
    authorization: Option<String>,
}

#[derive(Deserialize)]
struct Descriptor {
    #[serde(rename = "mediaType", default)]
    media_type: String,
    digest: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    platform: Option<Platform>,
}

#[derive(Deserialize)]
struct Platform {
    #[serde(default)]
    architecture: String,
    #[serde(default)]
    os: String,
    #[serde(default)]
    variant: Option<String>,
}

#[derive(Deserialize)]
struct Manifest {
    config: Descriptor,
    layers: Vec<Descriptor>,
}

impl Client {
    /// auth signs pulls in to registries, by host; hangar is where Hangar's
    /// own registry is, if there is one.
    pub fn new(auth: HashMap<String, Auth>, hangar: Option<HangarRegistry>) -> Client {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            // A blob's download may be sent on to a store of its own, which
            // is not given the registry's credentials.
            .redirect_auth_headers(ureq::config::RedirectAuthHeaders::Never)
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_recv_response(Some(Duration::from_secs(60)))
            .user_agent(concat!("hangar-worker/", env!("CARGO_PKG_VERSION")))
            .build()
            .new_agent();
        Client {
            agent,
            auth,
            hangar,
        }
    }

    fn repo(&self, r: &Reference) -> io::Result<Repo<'_>> {
        if let Some(h) = &self.hangar
            && h.host.eq_ignore_ascii_case(&r.domain)
        {
            return Ok(Repo {
                agent: &self.agent,
                base: format!("{}/v2", h.url.trim_end_matches('/')),
                name: r.path.clone(),
                creds: Some(("hangar-worker".into(), h.credential.clone())),
                authorization: None,
            });
        }
        let scheme = if r.is_local() { "http" } else { "https" };
        let creds = match self.auth.get(&r.domain) {
            Some(a) => {
                let password = fs::read_to_string(&a.password_file)
                    .map_err(|e| other(format!("the password for {}: {e}", r.domain)))?;
                Some((a.username.clone(), password.trim().to_string()))
            }
            None => None,
        };
        Ok(Repo {
            agent: &self.agent,
            base: format!("{scheme}://{}/v2", r.api_host()),
            name: r.path.clone(),
            creds,
            authorization: None,
        })
    }

    /// What r names: the digest of the manifest its registry has for it, the
    /// multi-platform index where it is one. Only the manifest is fetched.
    pub fn resolve(&self, r: &Reference) -> io::Result<String> {
        let mut repo = self.repo(r)?;
        let path = format!("/manifests/{}", r.version());
        let head = repo.send("HEAD", &path, Some(ACCEPT))?;
        if let Some(d) = header(&head, "docker-content-digest") {
            return Ok(d);
        }
        // A registry that does not say a manifest's digest is asked for
        // the manifest, whose digest is that of its bytes.
        let body = read_all(repo.send("GET", &path, Some(ACCEPT))?)?;
        Ok(format!("sha256:{}", hex::encode(Sha256::digest(&body))))
    }

    /// Fetches the image r names at digest and writes the tree its layers
    /// describe to out as an EROFS image, without unpacking them. Its blobs
    /// are fetched into scratch, each checked against its digest; the
    /// caller removes it once the image is written. report is told how
    /// many bytes are downloaded of those known to be needed -- the total
    /// grows once the manifest names the layers -- and then how many of the
    /// layers' bytes have been converted.
    pub fn pull(
        &self,
        r: &Reference,
        digest: &str,
        scratch: &Path,
        out: &Path,
        report: &mut dyn FnMut(Progress),
    ) -> io::Result<()> {
        let pinned = r.at(digest);
        let mut repo = self.repo(&pinned)?;
        fs::create_dir_all(scratch)?;
        let tick = Tick::new(report);
        let (downloaded, needed) = (Cell::new(0u64), Cell::new(0u64));
        let download = |p: &Tick| {
            p.report(
                Progress {
                    stage: Stage::Download,
                    done: downloaded.get().min(needed.get()),
                    total: needed.get(),
                    layer: 0,
                    layers: 0,
                },
                false,
            )
        };

        // The manifest it names, through an index to this platform's.
        let mut body = repo.manifest(digest)?;
        needed.set(needed.get() + body.len() as u64);
        downloaded.set(downloaded.get() + body.len() as u64);
        let value: serde_json::Value = serde_json::from_slice(&body)
            .map_err(|e| other(format!("{pinned}: its manifest: {e}")))?;
        if value.get("manifests").is_some() {
            let index: Vec<Descriptor> = serde_json::from_value(value["manifests"].clone())
                .map_err(|e| other(format!("{pinned}: its index: {e}")))?;
            let chosen = pick_platform(&index).ok_or_else(|| {
                other(format!("{pinned} has no image for linux/{}", guest_arch()))
            })?;
            body = repo.manifest(&chosen.digest)?;
            needed.set(needed.get() + body.len() as u64);
            downloaded.set(downloaded.get() + body.len() as u64);
        }
        let manifest: Manifest = serde_json::from_slice(&body)
            .map_err(|e| other(format!("{pinned}: its manifest: {e}")))?;
        if manifest.layers.is_empty() {
            return Err(other(format!("{pinned} has no layers")));
        }
        needed.set(
            needed.get()
                + manifest.config.size
                + manifest.layers.iter().map(|l| l.size).sum::<u64>(),
        );
        download(&tick);
        for d in std::iter::once(&manifest.config).chain(&manifest.layers) {
            repo.blob(d, &scratch.join(blob_file(&d.digest)?), &mut |n| {
                downloaded.set(downloaded.get() + n);
                download(&tick);
            })
            .map_err(|e| other(format!("fetching {pinned}: {e}")))?;
        }
        tick.report(
            Progress {
                stage: Stage::Download,
                done: needed.get(),
                total: needed.get(),
                layer: 0,
                layers: 0,
            },
            true,
        );

        // Top layer first.
        let total: u64 = manifest.layers.iter().map(|l| l.size).sum();
        let layers = manifest.layers.len();
        let converted = Cell::new(0u64);
        let mut left = layers;
        let unpack = |p: &Tick, layer: usize, last: bool| {
            p.report(
                Progress {
                    stage: Stage::Unpack,
                    done: converted.get(),
                    total,
                    layer,
                    layers,
                },
                last,
            )
        };
        crate::merge::build_erofs(out, || {
            if left == 0 {
                return Ok(None);
            }
            left -= 1;
            let layer = &manifest.layers[left];
            let n = layers - left;
            let file = File::open(scratch.join(blob_file(&layer.digest)?))?;
            let (converted, unpack, tick) = (&converted, &unpack, &tick);
            let counted = Counted {
                inner: file,
                on: Box::new(move |k| {
                    converted.set(converted.get() + k);
                    unpack(tick, n, false);
                }),
            };
            decompress(BufReader::new(counted), &layer.media_type)
                .map_err(|e| other(format!("layer {}: {e}", layer.digest)))
        })
        .map_err(|e| other(format!("converting {pinned}: {e}")))?;
        unpack(&tick, layers, true);
        Ok(())
    }
}

/// This platform's manifest in an index: linux, this machine's
/// architecture, and for arm64 its v8 variant or none.
fn pick_platform(index: &[Descriptor]) -> Option<&Descriptor> {
    let arch = guest_arch();
    index.iter().find(|d| {
        d.platform.as_ref().is_some_and(|p| {
            p.os == "linux"
                && p.architecture == arch
                && (arch != "arm64" || matches!(p.variant.as_deref(), None | Some("") | Some("v8")))
        })
    })
}

/// Where a blob is kept in scratch: its digest's hex.
fn blob_file(digest: &str) -> io::Result<String> {
    let hex = digest
        .strip_prefix("sha256:")
        .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| other(format!("a digest this worker does not check: {digest}")))?;
    Ok(hex.to_string())
}

/// A layer's tar stream, from its blob: gzip and zstd told by their first
/// bytes, as containerd tells them, whatever the media type says.
fn decompress<'a, R: io::BufRead + 'a>(
    mut r: R,
    _media_type: &str,
) -> io::Result<Option<Box<dyn Read + 'a>>> {
    let head = r.fill_buf()?;
    Ok(Some(if head.starts_with(&[0x1f, 0x8b]) {
        Box::new(flate2::bufread::MultiGzDecoder::new(r))
    } else if head.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        Box::new(zstd::stream::read::Decoder::with_buffer(r)?)
    } else {
        Box::new(r)
    }))
}

/// A reader that says how many bytes it has read.
struct Counted<'a, R> {
    inner: R,
    on: Box<dyn Fn(u64) + 'a>,
}

impl<R: Read> Read for Counted<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        (self.on)(n as u64);
        Ok(n)
    }
}

/// Says how far a pull has got, at most every REPORT_EVERY, and always
/// when told it is the last word of a stage.
struct Tick<'a> {
    last: Cell<Option<Instant>>,
    f: RefCell<&'a mut dyn FnMut(Progress)>,
}

impl<'a> Tick<'a> {
    fn new(f: &'a mut dyn FnMut(Progress)) -> Tick<'a> {
        Tick {
            last: Cell::new(None),
            f: RefCell::new(f),
        }
    }

    fn report(&self, p: Progress, last: bool) {
        let now = Instant::now();
        if !last && self.last.get().is_some_and(|t| now - t < REPORT_EVERY) {
            return;
        }
        self.last.set(Some(now));
        (self.f.borrow_mut())(p);
    }
}

fn header(resp: &ureq::http::Response<ureq::Body>, name: &str) -> Option<String> {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// The most of a manifest, or an error's body, that is read.
const MAX_BODY: u64 = 8 << 20;

fn read_all(resp: ureq::http::Response<ureq::Body>) -> io::Result<Vec<u8>> {
    let mut out = vec![];
    resp.into_body()
        .into_reader()
        .take(MAX_BODY)
        .read_to_end(&mut out)?;
    Ok(out)
}

impl Repo<'_> {
    /// Sends a request for path, beneath the repository, signing in as a
    /// 401's challenge asks and sending it again once.
    fn send(
        &mut self,
        method: &str,
        path: &str,
        accept: Option<&str>,
    ) -> io::Result<ureq::http::Response<ureq::Body>> {
        let url = format!("{}/{}{path}", self.base, self.name);
        for attempt in 0..2 {
            let mut req = ureq::http::Request::builder().method(method).uri(&url);
            if let Some(a) = accept {
                req = req.header("Accept", a);
            }
            if let Some(a) = &self.authorization {
                req = req.header("Authorization", a);
            }
            let req = req.body(()).map_err(|e| other(format!("{url}: {e}")))?;
            let resp = self
                .agent
                .run(req)
                .map_err(|e| other(format!("{url}: {e}")))?;
            let status = resp.status().as_u16();
            if status == 401 && attempt == 0 {
                let challenge = header(&resp, "www-authenticate").unwrap_or_default();
                self.authorize(&challenge)?;
                continue;
            }
            if !(200..300).contains(&status) {
                let what = if method == "HEAD" {
                    String::new()
                } else {
                    registry_error(&read_all(resp).unwrap_or_default())
                };
                return Err(io::Error::new(
                    if status == 404 {
                        io::ErrorKind::NotFound
                    } else {
                        io::ErrorKind::Other
                    },
                    format!("{url}: {status}{what}"),
                ));
            }
            return Ok(resp);
        }
        Err(other(format!("{url}: not signed in")))
    }

    /// Signs in as a challenge asks: with the credentials for Basic, and
    /// with a token from its realm for Bearer.
    fn authorize(&mut self, challenge: &str) -> io::Result<()> {
        let (scheme, params) = parse_challenge(challenge);
        let basic = self.creds.as_ref().map(|(u, p)| {
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("{u}:{p}"))
            )
        });
        if scheme.eq_ignore_ascii_case("basic") {
            self.authorization = Some(basic.ok_or_else(|| {
                other(format!(
                    "{}: asks to sign in, and this worker has no credentials for it",
                    self.base
                ))
            })?);
            return Ok(());
        }
        if !scheme.eq_ignore_ascii_case("bearer") {
            return Err(other(format!(
                "{}: an unknown sign-in, {challenge:?}",
                self.base
            )));
        }
        let realm = params
            .get("realm")
            .ok_or_else(|| other(format!("{}: a token challenge with no realm", self.base)))?;
        let scope = params
            .get("scope")
            .cloned()
            .unwrap_or_else(|| format!("repository:{}:pull", self.name));
        let mut url = format!("{realm}?scope={}", encode(&scope));
        if let Some(s) = params.get("service") {
            url.push_str(&format!("&service={}", encode(s)));
        }
        let mut req = self.agent.get(&url);
        if let Some(b) = &basic {
            req = req.header("Authorization", b);
        }
        let resp = req.call().map_err(|e| other(format!("{url}: {e}")))?;
        let status = resp.status().as_u16();
        let body = read_all(resp)?;
        if status != 200 {
            return Err(other(format!(
                "asking {realm} for a token: {status}{}",
                registry_error(&body)
            )));
        }
        #[derive(Deserialize)]
        struct Token {
            #[serde(default)]
            token: String,
            #[serde(default)]
            access_token: String,
        }
        let t: Token =
            serde_json::from_slice(&body).map_err(|e| other(format!("{realm}'s token: {e}")))?;
        let token = if t.token.is_empty() {
            t.access_token
        } else {
            t.token
        };
        self.authorization = Some(format!("Bearer {token}"));
        Ok(())
    }

    /// A manifest by digest, checked against it.
    fn manifest(&mut self, digest: &str) -> io::Result<Vec<u8>> {
        let body = read_all(self.send("GET", &format!("/manifests/{digest}"), Some(ACCEPT))?)?;
        let got = format!("sha256:{}", hex::encode(Sha256::digest(&body)));
        if got != digest {
            return Err(other(format!("the manifest {digest} arrived as {got}")));
        }
        Ok(body)
    }

    /// Fetches a blob into path, checking it against its digest and size,
    /// telling on how many bytes arrive. One already there is kept.
    fn blob(&mut self, d: &Descriptor, path: &Path, on: &mut dyn FnMut(u64)) -> io::Result<()> {
        if path.exists() {
            on(d.size);
            return Ok(());
        }
        let resp = self.send("GET", &format!("/blobs/{}", d.digest), None)?;
        let tmp = path.with_extension("part");
        let mut f = File::create(&tmp)?;
        let mut hash = Sha256::new();
        let mut body = resp.into_body().into_with_config().limit(u64::MAX).reader();
        let mut buf = vec![0u8; 1 << 20];
        let mut size = 0u64;
        loop {
            let n = body.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
            f.write_all(&buf[..n])?;
            size += n as u64;
            on(n as u64);
        }
        let got = format!("sha256:{}", hex::encode(hash.finalize()));
        if got != d.digest || (d.size != 0 && size != d.size) {
            let _ = fs::remove_file(&tmp);
            return Err(other(format!(
                "the blob {} arrived as {got}, {size} bytes of {}",
                d.digest, d.size
            )));
        }
        f.sync_all()?;
        fs::rename(&tmp, path)
    }
}

/// A registry's error body, as a phrase: ": NAME_UNKNOWN repository name
/// not known to registry".
fn registry_error(body: &[u8]) -> String {
    #[derive(Deserialize)]
    struct Errors {
        errors: Vec<Error>,
    }
    #[derive(Deserialize)]
    struct Error {
        #[serde(default)]
        code: String,
        #[serde(default)]
        message: String,
    }
    match serde_json::from_slice::<Errors>(body) {
        Ok(e) if !e.errors.is_empty() => format!(": {} {}", e.errors[0].code, e.errors[0].message),
        _ => String::new(),
    }
}

/// A WWW-Authenticate challenge's scheme and parameters:
/// Bearer realm="https://auth.docker.io/token",service="registry.docker.io".
fn parse_challenge(s: &str) -> (String, HashMap<String, String>) {
    let s = s.trim();
    let (scheme, rest) = s.split_once(' ').unwrap_or((s, ""));
    let mut params = HashMap::new();
    let mut rest = rest.trim();
    while !rest.is_empty() {
        let Some((key, after)) = rest.split_once('=') else {
            break;
        };
        let key = key.trim().to_ascii_lowercase();
        let after = after.trim_start();
        let (value, next) = if let Some(quoted) = after.strip_prefix('"') {
            let mut value = String::new();
            let mut chars = quoted.char_indices();
            let mut end = quoted.len();
            while let Some((i, c)) = chars.next() {
                match c {
                    '\\' => {
                        if let Some((_, n)) = chars.next() {
                            value.push(n);
                        }
                    }
                    '"' => {
                        end = i + 1;
                        break;
                    }
                    c => value.push(c),
                }
            }
            (value, &quoted[end..])
        } else {
            let end = after.find(',').unwrap_or(after.len());
            (after[..end].trim().to_string(), &after[end..])
        };
        params.insert(key, value);
        rest = next.trim_start().trim_start_matches(',').trim_start();
    }
    (scheme.to_string(), params)
}

/// A query parameter's value, percent-encoded.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
