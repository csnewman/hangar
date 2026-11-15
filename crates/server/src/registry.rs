//! Hangar's own container registry: the OCI
//! distribution API on its own host (registry.<host>), and to workers
//! beneath the worker API, its blobs in the blob store and everything else
//! in the server's store.
//!
//! A person signs in with their username and an access token, or as an
//! environment does, with a credential its profile session gave it; a
//! worker with its own credential, and pulls whatever it is asked to run.
//! A repository is its owner's, a person's or a team's, named by its first
//! component, and made by the first push to it.

use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::extract::Request;
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use chrono::Utc;
use futures_util::{StreamExt, TryStreamExt};
use serde_json::{Value, json};
use sha2::Digest as _;
use tokio::io::{AsyncRead, ReadBuf};
use uuid::Uuid;

use crate::audit::{self, Actor, Event};
use crate::blob::{self, Blobs, Expiry};
use crate::fail::{self, Fail};
use crate::http::Shared;
use crate::store::rows::{ImageRepository, Manifest, RegistryUpload, TeamMember};
use crate::store::{Error as StoreError, Store, Tx};
use crate::users::Principal;

/// The blob store, which a server running the registry has.
trait Held {
    fn blobs(&self) -> Result<&Blobs, Refused>;
}

impl Held for crate::http::App {
    fn blobs(&self) -> Result<&Blobs, Refused> {
        self.blobs.as_ref().ok_or_else(|| {
            refused(
                StatusCode::SERVICE_UNAVAILABLE,
                "UNAVAILABLE",
                "the registry has no blob store",
            )
        })
    }
}

/// What the blob store deletes of the registry's on its own: what a
/// failed upload leaves.
pub const EXPIRIES: &[Expiry] = &[
    Expiry {
        prefix: "registry/staging/",
        days: 2,
    },
    Expiry {
        prefix: "registry/uploads/",
        days: 2,
    },
];

const OCI_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const OCI_INDEX: &str = "application/vnd.oci.image.index.v1+json";
const DOCKER_MANIFEST: &str = "application/vnd.docker.distribution.manifest.v2+json";
const DOCKER_LIST: &str = "application/vnd.docker.distribution.manifest.list.v2+json";

/// Layers no registry holds: their manifest names where they are.
const FOREIGN: &[&str] = &[
    "application/vnd.oci.image.layer.nondistributable.v1.tar",
    "application/vnd.oci.image.layer.nondistributable.v1.tar+gzip",
    "application/vnd.oci.image.layer.nondistributable.v1.tar+zstd",
    "application/vnd.docker.image.rootfs.foreign.diff.tar.gzip",
];

const MAX_MANIFEST: usize = 4 << 20;

/// Where workers reach the registry, beneath the worker API.
pub const WORKER_PATH: &str = "/api/worker/v1/registry";

/// Who asks.
#[derive(Clone)]
enum Caller {
    Person(Principal),
    Worker(Uuid),
}

impl Caller {
    fn person(&self) -> Option<&Principal> {
        match self {
            Caller::Person(p) => Some(p),
            Caller::Worker(_) => None,
        }
    }

    fn actor(&self, ip: &str) -> Actor {
        match self {
            Caller::Person(p) => p.actor(ip),
            Caller::Worker(w) => Actor {
                worker: Some(*w),
                ip: ip.to_string(),
                ..Actor::default()
            },
        }
    }
}

/// A refusal, as the distribution API words one.
struct Refused {
    status: StatusCode,
    code: &'static str,
    message: String,
}

fn refused(status: StatusCode, code: &'static str, message: impl Into<String>) -> Refused {
    Refused {
        status,
        code,
        message: message.into(),
    }
}

impl From<StoreError> for Refused {
    fn from(e: StoreError) -> Refused {
        eprintln!("hangar-server: registry: {e}");
        refused(
            StatusCode::INTERNAL_SERVER_ERROR,
            "UNKNOWN",
            "internal error",
        )
    }
}

impl From<blob::Error> for Refused {
    fn from(e: blob::Error) -> Refused {
        eprintln!("hangar-server: registry: the blob store: {e}");
        refused(
            StatusCode::INTERNAL_SERVER_ERROR,
            "UNKNOWN",
            "internal error",
        )
    }
}

fn denied(why: impl Into<String>) -> Refused {
    refused(StatusCode::FORBIDDEN, "DENIED", why)
}

fn name_unknown() -> Refused {
    refused(
        StatusCode::NOT_FOUND,
        "NAME_UNKNOWN",
        "repository not found",
    )
}

fn json_response(status: StatusCode, v: &Value) -> Response {
    let mut r = (status, v.to_string()).into_response();
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    r
}

impl IntoResponse for Refused {
    fn into_response(self) -> Response {
        json_response(
            self.status,
            &json!({ "errors": [{ "code": self.code, "message": self.message }] }),
        )
    }
}

type Answer = Result<Response, Refused>;

fn component(s: &str) -> bool {
    // [a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*
    let b = s.as_bytes();
    if b.is_empty() || !b[0].is_ascii_lowercase() && !b[0].is_ascii_digit() {
        return false;
    }
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && matches!(b[i], b'.' | b'_' | b'-') {
            i += 1;
        }
        let sep = &s[start..i];
        let fine = sep == "." || sep == "_" || sep == "__" || sep.bytes().all(|x| x == b'-');
        if !fine || i == b.len() {
            return false;
        }
    }
    true
}

/// A repository's name: two components or more.
fn valid_name(name: &str) -> bool {
    name.len() <= 255 && name.split('/').count() >= 2 && name.split('/').all(component)
}

fn valid_tag(t: &str) -> bool {
    let b = t.as_bytes();
    !b.is_empty()
        && b.len() <= 128
        && (b[0].is_ascii_alphanumeric() || b[0] == b'_')
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

/// A digest and its algorithm, when it is one of those the registry
/// knows.
#[derive(Debug, Clone, PartialEq)]
struct Digest {
    algorithm: &'static str,
    hex: String,
}

impl Digest {
    fn parse(s: &str) -> Option<Digest> {
        let (alg, hex) = s.split_once(':')?;
        let (algorithm, len) = match alg {
            "sha256" => ("sha256", 64),
            "sha384" => ("sha384", 96),
            "sha512" => ("sha512", 128),
            _ => return None,
        };
        (hex.len() == len
            && hex
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)))
        .then(|| Digest {
            algorithm,
            hex: hex.to_string(),
        })
    }

    fn of(algorithm: &'static str, data: &[u8]) -> Digest {
        let mut h = Hasher::new(algorithm);
        h.update(data);
        h.finish()
    }

    fn key(&self) -> String {
        format!("registry/blobs/{}/{}", self.algorithm, self.hex)
    }
}

impl std::fmt::Display for Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.algorithm, self.hex)
    }
}

enum Hasher {
    Sha256(sha2::Sha256),
    Sha384(sha2::Sha384),
    Sha512(sha2::Sha512),
}

impl Hasher {
    fn new(algorithm: &str) -> Hasher {
        match algorithm {
            "sha384" => Hasher::Sha384(sha2::Sha384::new()),
            "sha512" => Hasher::Sha512(sha2::Sha512::new()),
            _ => Hasher::Sha256(sha2::Sha256::new()),
        }
    }

    fn update(&mut self, b: &[u8]) {
        match self {
            Hasher::Sha256(h) => h.update(b),
            Hasher::Sha384(h) => h.update(b),
            Hasher::Sha512(h) => h.update(b),
        }
    }

    fn finish(self) -> Digest {
        let (algorithm, bytes) = match self {
            Hasher::Sha256(h) => ("sha256", h.finalize().to_vec()),
            Hasher::Sha384(h) => ("sha384", h.finalize().to_vec()),
            Hasher::Sha512(h) => ("sha512", h.finalize().to_vec()),
        };
        Digest {
            algorithm,
            hex: bytes.iter().map(|b| format!("{b:02x}")).collect(),
        }
    }
}

fn digest_or_refuse(s: &str) -> Result<Digest, Refused> {
    Digest::parse(s).ok_or_else(|| {
        refused(
            StatusCode::BAD_REQUEST,
            "DIGEST_INVALID",
            format!("invalid digest {s}"),
        )
    })
}

/// Reads through, hashing and counting what passes.
struct Hashing<R> {
    inner: R,
    hasher: Option<Hasher>,
    n: u64,
}

impl<R: AsyncRead + Unpin> AsyncRead for Hashing<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let r = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &r {
            let new = buf.filled()[before..].to_vec();
            self.n += new.len() as u64;
            if let Some(h) = &mut self.hasher {
                h.update(&new);
            }
        }
        r
    }
}

/// Everything access is worked out from: repositories, their
/// collaborators, and who is in which team.
struct World {
    repos: Vec<ImageRepository>,
    collaborators: Vec<(Uuid, Uuid)>,
    team_collaborators: Vec<(Uuid, Uuid)>,
    members: Vec<TeamMember>,
}

/// What a caller may do with a repository.
#[derive(Debug, Clone)]
struct Access {
    repo: ImageRepository,
    pull: bool,
    push: bool,
    manage: bool,
}

impl World {
    async fn load(tx: &mut Tx) -> crate::store::Result<World> {
        let (users, teams) = tx.repository_collaborators().await?;
        Ok(World {
            repos: tx.image_repositories().await?,
            collaborators: users
                .into_iter()
                .map(|c| (c.repository_id, c.who))
                .collect(),
            team_collaborators: teams
                .into_iter()
                .map(|c| (c.repository_id, c.who))
                .collect(),
            members: tx.team_members().await?,
        })
    }

    fn role(&self, team: Option<Uuid>, user: Uuid) -> Option<&str> {
        let team = team?;
        self.members
            .iter()
            .find(|m| m.team_id == team && m.user_id == user)
            .map(|m| m.role.as_str())
    }

    fn access(&self, r: &ImageRepository, c: &Caller) -> Access {
        let (admin, user) = match c {
            Caller::Person(p) => (p.admin, Some(p.user_id)),
            Caller::Worker(_) => (false, None),
        };
        let owns = user.is_some() && r.owner_id == user;
        let role = user.and_then(|u| self.role(r.team_id, u));
        let collab = user.is_some_and(|u| self.collaborators.contains(&(r.id, u)));
        let via_team = |push: bool| {
            user.is_some_and(|u| {
                self.team_collaborators.iter().any(|(repo, team)| {
                    *repo == r.id
                        && self
                            .role(Some(*team), u)
                            .is_some_and(|role| !push || matches!(role, "member" | "admin"))
                })
            })
        };
        Access {
            pull: matches!(c, Caller::Worker(_))
                || r.visibility == "shared"
                || admin
                || owns
                || role.is_some()
                || collab
                || via_team(false),
            push: admin
                || owns
                || matches!(role, Some("member" | "admin"))
                || collab
                || via_team(true),
            manage: admin || owns || role == Some("admin"),
            repo: r.clone(),
        }
    }

    /// The repository at path, if c may pull it.
    fn lookup(&self, c: &Caller, path: &str) -> Option<Access> {
        self.repos
            .iter()
            .find(|r| r.path == path)
            .map(|r| self.access(r, c))
            .filter(|a| a.pull)
    }
}

/// The repository name names, made for a push to one not there yet.
async fn open(
    app: &Shared,
    c: &Caller,
    ip: &str,
    name: &str,
    create: bool,
) -> Result<Access, Refused> {
    if !valid_name(name) {
        return Err(refused(
            StatusCode::BAD_REQUEST,
            "NAME_INVALID",
            "invalid repository name",
        ));
    }
    let mut tx = app.store.begin().await?;
    let w = World::load(&mut tx).await?;
    if let Some(a) = w.lookup(c, name) {
        return Ok(a);
    }
    if w.repos.iter().any(|r| r.path == name) || !create {
        return Err(name_unknown());
    }
    let Some(p) = c.person() else {
        return Err(denied("denied"));
    };
    let ns = name.split('/').next().unwrap_or_default();
    let (owner, team) = if let Some(u) = tx.person_by_name(ns).await? {
        if u.id != p.user_id && !p.admin {
            return Err(denied(format!("you may not push to {ns}")));
        }
        (Some(u.id), None)
    } else if let Some(t) = tx
        .teams()
        .await?
        .into_iter()
        .find(|t| t.slug.to_lowercase() == ns)
    {
        if !p.admin && !matches!(w.role(Some(t.id), p.user_id), Some("member" | "admin")) {
            return Err(denied(format!("you may not push to {ns}")));
        }
        (None, Some(t.id))
    } else {
        return Err(denied(format!("no user or team is named {ns}")));
    };
    let now = Utc::now();
    let r = ImageRepository {
        id: Uuid::new_v4(),
        path: name.to_string(),
        owner_id: owner,
        team_id: team,
        visibility: "private".into(),
        description: String::new(),
        created_at: now,
        updated_at: now,
    };
    if tx.insert_image_repository(&r).await? {
        audit::record(
            &mut tx,
            &c.actor(ip),
            Event::new("image_repository.create", "image", name, name),
        )
        .await?;
    }
    tx.commit().await?;
    let mut tx = app.store.begin().await?;
    World::load(&mut tx)
        .await?
        .lookup(c, name)
        .ok_or_else(name_unknown)
}

/// Who a request's Basic credentials sign in.
async fn authenticate(app: &Shared, headers: &HeaderMap) -> Result<Option<Caller>, StoreError> {
    let Some((user, password)) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.strip_prefix("Basic ")
                .or_else(|| v.strip_prefix("basic "))
        })
        .and_then(|b| {
            base64::engine::general_purpose::STANDARD
                .decode(b.trim())
                .ok()
        })
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|s| {
            s.split_once(':')
                .map(|(u, p)| (u.to_string(), p.to_string()))
        })
    else {
        return Ok(None);
    };
    if password.is_empty() {
        return Ok(None);
    }
    if password.starts_with(crate::users::TOKEN_PREFIX) {
        return Ok(crate::users::token(&app.store, &password)
            .await?
            .filter(|p| p.username.eq_ignore_ascii_case(&user))
            .map(Caller::Person));
    }
    if password.starts_with(crate::profile::REGISTRY_CREDENTIAL_PREFIX) {
        let Some(id) = app
            .cfg
            .sealer
            .as_deref()
            .and_then(|s| open_credential(s, &password))
        else {
            return Ok(None);
        };
        let u = app.store.begin().await?.user(id).await?;
        return Ok(u
            .filter(|u| {
                u.disabled_at.is_none()
                    && u.kind == "person"
                    && u.username.eq_ignore_ascii_case(&user)
            })
            .map(|u| {
                Caller::Person(Principal {
                    user_id: u.id,
                    username: u.username,
                    admin: false,
                    via: String::new(),
                })
            }));
    }
    Ok(crate::workers::authenticate(&app.store, &password)
        .await?
        .map(Caller::Worker))
}

/// The person a registry credential is for, while it lasts.
fn open_credential(sealer: &crate::seal::Sealer, credential: &str) -> Option<Uuid> {
    let enc = credential.strip_prefix(crate::profile::REGISTRY_CREDENTIAL_PREFIX)?;
    let sealed = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(enc)
        .ok()?;
    let plain = sealer
        .open(&sealed, crate::profile::REGISTRY_CREDENTIAL_BINDING)
        .ok()?;
    let claims: Value = serde_json::from_slice(&plain).ok()?;
    let user = Uuid::parse_str(claims.get("u")?.as_str()?).ok()?;
    (Utc::now().timestamp() < claims.get("x")?.as_i64()?).then_some(user)
}

/// Serves the distribution API: a request to the registry's host, or to
/// WORKER_PATH with that taken off its path.
pub async fn serve(app: Shared, req: Request, path: &str) -> Response {
    let ip = crate::http::client_ip_of(req.headers(), req.extensions());
    let headers = req.headers().clone();
    let mut r = match authenticate(&app, &headers).await {
        Ok(Some(c)) => route(&app, c, &ip, req, path)
            .await
            .unwrap_or_else(|e| e.into_response()),
        Ok(None) => {
            let mut r = refused(
                StatusCode::UNAUTHORIZED,
                "UNAUTHORIZED",
                "sign in with your username and an access token",
            )
            .into_response();
            r.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Basic realm=\"Hangar\", charset=\"UTF-8\""),
            );
            r
        }
        Err(e) => Refused::from(e).into_response(),
    };
    r.headers_mut().insert(
        "docker-distribution-api-version",
        HeaderValue::from_static("registry/2.0"),
    );
    r
}

async fn route(app: &Shared, c: Caller, ip: &str, req: Request, path: &str) -> Answer {
    let Some(rest) = path.strip_prefix("/v2/") else {
        return Err(refused(
            StatusCode::NOT_FOUND,
            "NAME_UNKNOWN",
            "not a registry path",
        ));
    };
    if rest.is_empty() {
        return Ok(json_response(StatusCode::OK, &json!({})));
    }
    let rest = rest.to_string();
    if let Some(name) = rest.strip_suffix("/tags/list") {
        let (parts, _) = req.into_parts();
        return tags(app, &c, ip, &parts, name).await;
    }
    if let Some(i) = rest.rfind("/blobs/uploads") {
        let id = rest[i + "/blobs/uploads".len()..]
            .trim_matches('/')
            .to_string();
        return uploads(app, &c, ip, req, &rest[..i], &id).await;
    }
    if let Some(i) = rest.rfind("/manifests/") {
        return manifests(
            app,
            &c,
            ip,
            req,
            &rest[..i],
            &rest[i + "/manifests/".len()..],
        )
        .await;
    }
    let (parts, _) = req.into_parts();
    if let Some(i) = rest.rfind("/blobs/") {
        return blobs(
            app,
            &c,
            ip,
            &parts,
            &rest[..i],
            &rest[i + "/blobs/".len()..],
        )
        .await;
    }
    if let Some(i) = rest.rfind("/referrers/") {
        return referrers(
            app,
            &c,
            ip,
            &parts,
            &rest[..i],
            &rest[i + "/referrers/".len()..],
        )
        .await;
    }
    Err(refused(
        StatusCode::NOT_FOUND,
        "NAME_UNKNOWN",
        "not a registry path",
    ))
}

fn not_allowed() -> Refused {
    refused(
        StatusCode::METHOD_NOT_ALLOWED,
        "UNSUPPORTED",
        "method not allowed",
    )
}

fn header_of<'a>(h: &'a HeaderMap, name: &str) -> &'a str {
    h.get(name).and_then(|v| v.to_str().ok()).unwrap_or("")
}

fn query(uri: &axum::http::Uri) -> HashMap<String, String> {
    url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
        .into_owned()
        .collect()
}

fn with_headers(status: StatusCode, headers: &[(&str, String)], body: Body) -> Response {
    let mut r = Response::new(body);
    *r.status_mut() = status;
    for (k, v) in headers {
        if let (Ok(k), Ok(v)) = (header::HeaderName::try_from(*k), HeaderValue::from_str(v)) {
            r.headers_mut().insert(k, v);
        }
    }
    r
}

/// The single range a Range header asks for, start to end inclusive.
fn range(h: &HeaderMap, size: u64) -> Option<(u64, u64)> {
    let spec = header_of(h, "range").strip_prefix("bytes=")?;
    if spec.contains(',') || size == 0 {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let (start, end) = match (a.trim(), b.trim()) {
        ("", n) => {
            let n: u64 = n.parse().ok()?;
            (size.saturating_sub(n), size - 1)
        }
        (s, "") => (s.parse().ok()?, size - 1),
        (s, e) => (s.parse().ok()?, e.parse::<u64>().ok()?.min(size - 1)),
    };
    (start <= end && start < size).then_some((start, end))
}

async fn blobs(
    app: &Shared,
    c: &Caller,
    ip: &str,
    req: &Parts,
    name: &str,
    reference: &str,
) -> Answer {
    let a = open(app, c, ip, name, false).await?;
    let d = digest_or_refuse(reference)?;
    match req.method {
        Method::GET | Method::HEAD => {
            let size = app
                .store
                .begin()
                .await?
                .repository_blob(a.repo.id, &d.to_string())
                .await?
                .ok_or_else(|| refused(StatusCode::NOT_FOUND, "BLOB_UNKNOWN", "blob not found"))?
                as u64;
            let wanted = range(&req.headers, size);
            let mut headers = vec![
                ("docker-content-digest", d.to_string()),
                ("content-type", "application/octet-stream".to_string()),
                ("accept-ranges", "bytes".to_string()),
            ];
            let status = match wanted {
                Some((s, e)) => {
                    headers.push(("content-range", format!("bytes {s}-{e}/{size}")));
                    headers.push(("content-length", (e + 1 - s).to_string()));
                    StatusCode::PARTIAL_CONTENT
                }
                None => {
                    headers.push(("content-length", size.to_string()));
                    StatusCode::OK
                }
            };
            if req.method == Method::HEAD {
                return Ok(with_headers(status, &headers, Body::empty()));
            }
            let blobs = app.blobs()?;
            let found = blobs
                .get(&d.key(), wanted.map(|(s, e)| (s, Some(e))))
                .await?;
            let Some(b) = found else {
                eprintln!(
                    "hangar-server: registry: blob {d} is recorded but not in the blob store"
                );
                return Err(refused(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "UNKNOWN",
                    "internal error",
                ));
            };
            Ok(with_headers(status, &headers, Body::from_stream(b.stream)))
        }
        Method::DELETE => {
            if !a.push {
                return Err(denied(format!("you may not delete from {name}")));
            }
            let mut tx = app.store.begin().await?;
            if tx.blob_used(a.repo.id, &d.to_string()).await? {
                return Err(refused(
                    StatusCode::CONFLICT,
                    "DENIED",
                    "a manifest in the repository refers to the blob",
                ));
            }
            if !tx.remove_repository_blob(a.repo.id, &d.to_string()).await? {
                return Err(refused(
                    StatusCode::NOT_FOUND,
                    "BLOB_UNKNOWN",
                    "blob not found",
                ));
            }
            tx.commit().await?;
            Ok(StatusCode::ACCEPTED.into_response())
        }
        _ => Err(not_allowed()),
    }
}

fn chunks_prefix(id: Uuid) -> String {
    format!("registry/uploads/{id}/")
}

fn upload_location(name: &str, id: Uuid) -> String {
    format!("/v2/{name}/blobs/uploads/{id}")
}

fn range_of(size: u64) -> String {
    if size == 0 {
        "0-0".into()
    } else {
        format!("0-{}", size - 1)
    }
}

fn created(name: &str, d: &Digest) -> Response {
    with_headers(
        StatusCode::CREATED,
        &[
            ("location", format!("/v2/{name}/blobs/{d}")),
            ("docker-content-digest", d.to_string()),
            ("content-length", "0".into()),
        ],
        Body::empty(),
    )
}

/// What a request's body reads as.
fn body_reader(req: Request) -> impl AsyncRead + Unpin + Send {
    blob::reader(Box::pin(
        req.into_body()
            .into_data_stream()
            .map_err(std::io::Error::other),
    ))
}

async fn upload_size(blobs: &Blobs, id: Uuid) -> Result<u64, Refused> {
    Ok(blobs
        .list(&chunks_prefix(id))
        .await?
        .iter()
        .map(|o| o.size)
        .sum())
}

/// Appends what r reads to an upload at offset, giving its new size.
async fn append(
    blobs: &Blobs,
    id: Uuid,
    offset: u64,
    r: impl AsyncRead + Unpin + Send,
) -> Result<u64, Refused> {
    let mut counted = Hashing {
        inner: r,
        hasher: None,
        n: 0,
    };
    let key = format!("{}{offset:020}", chunks_prefix(id));
    blobs.put(&key, &mut counted).await?;
    if counted.n == 0 {
        blobs.delete(&key).await?;
    }
    Ok(offset + counted.n)
}

/// An upload's chunks read in order, as one.
async fn chunks(blobs: &Blobs, id: Uuid) -> Result<impl AsyncRead + Unpin + Send + '_, Refused> {
    let keys: Vec<String> = blobs
        .list(&chunks_prefix(id))
        .await?
        .into_iter()
        .map(|o| o.key)
        .collect();
    let stream = futures_util::stream::iter(keys)
        .then(move |k| async move {
            match blobs.get(&k, None).await {
                Ok(Some(b)) => Ok(b.stream),
                Ok(None) => Err(std::io::Error::other(format!("chunk {k} is gone"))),
                Err(e) => Err(std::io::Error::other(e.0)),
            }
        })
        .try_flatten();
    Ok(blob::reader(Box::pin(stream)))
}

/// Keeps what content reads as the blob d, once it matches d, for repo.
async fn finish(
    app: &Shared,
    repo: Uuid,
    content: impl AsyncRead + Unpin + Send,
    d: &Digest,
) -> Result<(), Refused> {
    let blobs = app.blobs()?;
    let staging = format!("registry/staging/{}", Uuid::new_v4());
    let mut hashing = Hashing {
        inner: content,
        hasher: Some(Hasher::new(d.algorithm)),
        n: 0,
    };
    let put = blobs.put(&staging, &mut hashing).await;
    let result = async {
        put?;
        let got = hashing.hasher.take().expect("a hasher").finish();
        if got != *d {
            return Err(refused(
                StatusCode::BAD_REQUEST,
                "DIGEST_INVALID",
                "the upload's content does not match its digest",
            ));
        }
        let mut tx = app.store.begin().await?;
        tx.lock_blob(&d.to_string()).await?;
        if blobs.stat(&d.key()).await?.is_none() {
            blobs.copy(&staging, &d.key()).await?;
        }
        tx.add_blob(repo, &d.to_string(), hashing.n as i64, Utc::now())
            .await?;
        tx.commit().await?;
        Ok(())
    }
    .await;
    let _ = blobs.delete(&staging).await;
    result
}

async fn end_upload(app: &Shared, id: Uuid) {
    let done = async {
        let mut tx = app.store.begin().await?;
        tx.delete_upload(id).await?;
        tx.commit().await?;
        drop_chunks(app.blobs()?, id).await
    };
    if let Err(e) = done.await {
        eprintln!("hangar-server: registry: ending upload {id}: {}", e.message);
    }
}

async fn drop_chunks(blobs: &Blobs, id: Uuid) -> Result<(), Refused> {
    for o in blobs.list(&chunks_prefix(id)).await? {
        blobs.delete(&o.key).await?;
    }
    Ok(())
}

/// The upload's size when a chunk starts where the upload ends, as its
/// Content-Range says; a refusal saying where it ends otherwise.
async fn in_order(
    blobs: &Blobs,
    h: &HeaderMap,
    name: &str,
    id: Uuid,
) -> Result<u64, Box<Response>> {
    let size = upload_size(blobs, id)
        .await
        .map_err(|e| Box::new(e.into_response()))?;
    let cr = header_of(h, "content-range");
    if cr.is_empty() {
        return Ok(size);
    }
    let start = cr.split('-').next().unwrap_or("");
    if start.parse::<u64>().ok() == Some(size) {
        return Ok(size);
    }
    let mut r = refused(
        StatusCode::RANGE_NOT_SATISFIABLE,
        "BLOB_UPLOAD_INVALID",
        format!("the chunk starts at {start}, and the upload has {size} bytes"),
    )
    .into_response();
    for (k, v) in [
        ("range", range_of(size)),
        ("location", upload_location(name, id)),
    ] {
        if let Ok(v) = HeaderValue::from_str(&v) {
            r.headers_mut().insert(k, v);
        }
    }
    Err(Box::new(r))
}

async fn uploads(app: &Shared, c: &Caller, ip: &str, req: Request, name: &str, id: &str) -> Answer {
    let a = open(
        app,
        c,
        ip,
        name,
        req.method() == Method::POST && id.is_empty(),
    )
    .await?;
    if !a.push {
        return Err(denied(format!("you may not push to {name}")));
    }
    let blobs = app.blobs()?;
    if id.is_empty() {
        if req.method() != Method::POST {
            return Err(not_allowed());
        }
        return start_upload(app, c, ip, req, &a).await;
    }
    let upload_unknown = || {
        refused(
            StatusCode::NOT_FOUND,
            "BLOB_UPLOAD_UNKNOWN",
            "upload not found",
        )
    };
    let id = Uuid::parse_str(id).map_err(|_| upload_unknown())?;
    if !app
        .store
        .begin()
        .await?
        .upload_exists(id, a.repo.id)
        .await?
    {
        return Err(upload_unknown());
    }
    let progress = |size: u64, status: StatusCode| {
        with_headers(
            status,
            &[
                ("location", upload_location(name, id)),
                ("range", range_of(size)),
                ("docker-upload-uuid", id.to_string()),
            ],
            Body::empty(),
        )
    };
    match req.method().clone() {
        Method::PATCH => {
            let headers = req.headers().clone();
            let at = match in_order(blobs, &headers, name, id).await {
                Ok(at) => at,
                Err(r) => return Ok(*r),
            };
            let size = append(blobs, id, at, body_reader(req)).await?;
            Ok(progress(size, StatusCode::ACCEPTED))
        }
        Method::PUT => {
            let d = digest_or_refuse(
                query(req.uri())
                    .get("digest")
                    .map(String::as_str)
                    .unwrap_or(""),
            )?;
            let headers = req.headers().clone();
            let at = match in_order(blobs, &headers, name, id).await {
                Ok(at) => at,
                Err(r) => return Ok(*r),
            };
            append(blobs, id, at, body_reader(req)).await?;
            finish(app, a.repo.id, chunks(blobs, id).await?, &d).await?;
            end_upload(app, id).await;
            Ok(created(name, &d))
        }
        Method::GET => Ok(progress(
            upload_size(blobs, id).await?,
            StatusCode::NO_CONTENT,
        )),
        Method::DELETE => {
            end_upload(app, id).await;
            Ok(StatusCode::NO_CONTENT.into_response())
        }
        _ => Err(not_allowed()),
    }
}

async fn start_upload(app: &Shared, c: &Caller, ip: &str, req: Request, a: &Access) -> Answer {
    let q = query(req.uri());
    if let (Some(mount), Some(from)) = (q.get("mount"), q.get("from"))
        && let Some(d) = Digest::parse(mount)
        && let Ok(src) = open(app, c, ip, from, false).await
    {
        let mut tx = app.store.begin().await?;
        if tx
            .repository_blob(src.repo.id, &d.to_string())
            .await?
            .is_some()
        {
            tx.mount_blob(a.repo.id, &d.to_string(), Utc::now()).await?;
            tx.commit().await?;
            return Ok(created(&a.repo.path, &d));
        }
    }
    if let Some(ds) = q.get("digest") {
        let d = digest_or_refuse(ds)?;
        finish(app, a.repo.id, body_reader(req), &d).await?;
        return Ok(created(&a.repo.path, &d));
    }
    let u = RegistryUpload {
        id: Uuid::new_v4(),
        repository_id: a.repo.id,
        created_at: Utc::now(),
    };
    let mut tx = app.store.begin().await?;
    tx.insert_upload(&u).await?;
    tx.commit().await?;
    Ok(with_headers(
        StatusCode::ACCEPTED,
        &[
            ("location", upload_location(&a.repo.path, u.id)),
            ("range", range_of(0)),
            ("docker-upload-uuid", u.id.to_string()),
            ("content-length", "0".into()),
        ],
        Body::empty(),
    ))
}

async fn manifests(
    app: &Shared,
    c: &Caller,
    ip: &str,
    req: Request,
    name: &str,
    reference: &str,
) -> Answer {
    let a = open(app, c, ip, name, req.method() == Method::PUT).await?;
    match req.method().clone() {
        Method::GET | Method::HEAD => {
            let unknown = || {
                refused(
                    StatusCode::NOT_FOUND,
                    "MANIFEST_UNKNOWN",
                    "manifest not found",
                )
            };
            let (digest, tag) = if Digest::parse(reference).is_some() {
                (Some(reference), None)
            } else if valid_tag(reference) {
                (None, Some(reference))
            } else {
                return Err(unknown());
            };
            let m = app
                .store
                .begin()
                .await?
                .manifest(a.repo.id, digest, tag)
                .await?
                .ok_or_else(unknown)?;
            let headers = [
                ("content-type", m.media_type.clone()),
                ("docker-content-digest", m.digest.clone()),
                ("content-length", m.content.len().to_string()),
            ];
            let body = if req.method() == Method::GET {
                Body::from(m.content)
            } else {
                Body::empty()
            };
            Ok(with_headers(StatusCode::OK, &headers, body))
        }
        Method::PUT => {
            if !a.push {
                return Err(denied(format!("you may not push to {name}")));
            }
            let (d, subject) = put_manifest(app, c, ip, &a, req, reference).await?;
            let mut headers = vec![
                ("location", format!("/v2/{name}/manifests/{d}")),
                ("docker-content-digest", d.to_string()),
                ("content-length", "0".into()),
            ];
            if let Some(s) = subject {
                headers.push(("oci-subject", s));
            }
            Ok(with_headers(StatusCode::CREATED, &headers, Body::empty()))
        }
        Method::DELETE => {
            if !a.push {
                return Err(denied(format!("you may not delete from {name}")));
            }
            let mut tx = app.store.begin().await?;
            let (gone, details) = if Digest::parse(reference).is_some() {
                (
                    tx.delete_manifest(a.repo.id, reference).await?,
                    json!({ "digest": reference }),
                )
            } else if valid_tag(reference) {
                (
                    tx.delete_tag(a.repo.id, reference).await?,
                    json!({ "tag": reference }),
                )
            } else {
                return Err(refused(
                    StatusCode::BAD_REQUEST,
                    "MANIFEST_INVALID",
                    format!("invalid reference {reference}"),
                ));
            };
            if !gone {
                return Err(refused(
                    StatusCode::NOT_FOUND,
                    "MANIFEST_UNKNOWN",
                    "manifest not found",
                ));
            }
            audit::record(
                &mut tx,
                &c.actor(ip),
                Event::new("image.delete", "image", &a.repo.path, &a.repo.path).details(details),
            )
            .await?;
            tx.commit().await?;
            Ok(StatusCode::ACCEPTED.into_response())
        }
        _ => Err(not_allowed()),
    }
}

fn invalid_manifest(why: impl Into<String>) -> Refused {
    refused(StatusCode::BAD_REQUEST, "MANIFEST_INVALID", why)
}

/// Keeps a manifest pushed, giving its digest and its subject.
async fn put_manifest(
    app: &Shared,
    c: &Caller,
    ip: &str,
    a: &Access,
    req: Request,
    reference: &str,
) -> Result<(Digest, Option<String>), Refused> {
    let content_type = header_of(req.headers(), "content-type")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    let content = axum::body::to_bytes(req.into_body(), MAX_MANIFEST + 1)
        .await
        .map_err(|_| {
            refused(
                StatusCode::PAYLOAD_TOO_LARGE,
                "SIZE_INVALID",
                "the manifest is larger than 4 MiB",
            )
        })?
        .to_vec();
    let m: Value = serde_json::from_slice(&content)
        .map_err(|e| invalid_manifest(format!("the manifest is not JSON: {e}")))?;
    let own_type = m.get("mediaType").and_then(Value::as_str).unwrap_or("");
    let mt = if content_type.is_empty() {
        own_type.to_string()
    } else {
        content_type
    };
    if !own_type.is_empty() && own_type != mt {
        return Err(invalid_manifest(
            "the manifest's mediaType is not its Content-Type",
        ));
    }
    let (d, tag) = if let Some(want) = Digest::parse(reference) {
        let d = Digest::of(want.algorithm, &content);
        if d != want {
            return Err(refused(
                StatusCode::BAD_REQUEST,
                "DIGEST_INVALID",
                "the manifest's content does not match its digest",
            ));
        }
        (d, None)
    } else if valid_tag(reference) {
        (Digest::of("sha256", &content), Some(reference.to_string()))
    } else if reference.contains(':') {
        return Err(refused(
            StatusCode::BAD_REQUEST,
            "DIGEST_INVALID",
            "unsupported digest algorithm",
        ));
    } else {
        return Err(invalid_manifest(format!("invalid tag {reference}")));
    };
    let digest_of = |x: &Value| {
        x.get("digest")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let mut blobs: Vec<String> = Vec::new();
    let mut manifests: Vec<String> = Vec::new();
    let mut artifact_type = m
        .get("artifactType")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    match mt.as_str() {
        OCI_MANIFEST | DOCKER_MANIFEST => {
            let Some(config) = m.get("config").filter(|c| c.is_object()) else {
                return Err(invalid_manifest("an image manifest needs a config"));
            };
            blobs.push(digest_of(config));
            for l in m
                .get("layers")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let lt = l.get("mediaType").and_then(Value::as_str).unwrap_or("");
                if !FOREIGN.contains(&lt) {
                    blobs.push(digest_of(l));
                }
            }
            if artifact_type.is_empty() {
                artifact_type = config
                    .get("mediaType")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
            }
        }
        OCI_INDEX | DOCKER_LIST => {
            for x in m
                .get("manifests")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                manifests.push(digest_of(x));
            }
        }
        other => {
            return Err(invalid_manifest(format!(
                "unsupported manifest type {other}"
            )));
        }
    }
    for r in blobs.iter().chain(manifests.iter()) {
        if Digest::parse(r).is_none() {
            return Err(invalid_manifest(format!(
                "the manifest names an invalid digest {r}"
            )));
        }
    }
    let subject = match m.get("subject").filter(|s| s.is_object()) {
        Some(s) => {
            let sd = digest_of(s);
            if Digest::parse(&sd).is_none() {
                return Err(invalid_manifest("the manifest's subject is not a digest"));
            }
            Some(sd)
        }
        None => None,
    };
    let annotations = m
        .get("annotations")
        .filter(|a| a.as_object().is_some_and(|o| !o.is_empty()))
        .cloned();
    let mut tx = app.store.begin().await?;
    let (missing_blob, missing_manifest) = tx.missing(a.repo.id, &blobs, &manifests).await?;
    if let Some(b) = missing_blob {
        return Err(refused(
            StatusCode::BAD_REQUEST,
            "MANIFEST_BLOB_UNKNOWN",
            format!("the repository has no blob {b}"),
        ));
    }
    if let Some(x) = missing_manifest {
        return Err(refused(
            StatusCode::BAD_REQUEST,
            "MANIFEST_BLOB_UNKNOWN",
            format!("the repository has no manifest {x}"),
        ));
    }
    let now = Utc::now();
    let refs: Vec<String> = blobs.iter().chain(manifests.iter()).cloned().collect();
    tx.insert_manifest(
        &Manifest {
            repository_id: a.repo.id,
            digest: d.to_string(),
            media_type: mt,
            artifact_type,
            subject: subject.clone(),
            annotations,
            content,
            created_at: now,
        },
        &refs,
    )
    .await?;
    let mut details = json!({ "digest": d.to_string() });
    if let Some(t) = &tag {
        tx.set_tag(a.repo.id, t, &d.to_string(), now).await?;
        details["tag"] = json!(t);
    }
    tx.touch_image_repository(a.repo.id, now).await?;
    audit::record(
        &mut tx,
        &c.actor(ip),
        Event::new("image.push", "image", &a.repo.path, &a.repo.path).details(details),
    )
    .await?;
    tx.commit().await?;
    Ok((d, subject))
}

async fn tags(app: &Shared, c: &Caller, ip: &str, req: &Parts, name: &str) -> Answer {
    if req.method != Method::GET {
        return Err(not_allowed());
    }
    let a = open(app, c, ip, name, false).await?;
    let q = query(&req.uri);
    let limit = match q.get("n") {
        Some(n) => Some(n.parse::<usize>().map_err(|_| {
            refused(
                StatusCode::BAD_REQUEST,
                "PAGINATION_NUMBER_INVALID",
                "invalid n",
            )
        })?),
        None => None,
    };
    let last = q.get("last").cloned().unwrap_or_default();
    let mut list: Vec<String> = app
        .store
        .begin()
        .await?
        .image_tags(Some(a.repo.id))
        .await?
        .into_iter()
        .map(|t| t.tag)
        .filter(|t| *t > last)
        .collect();
    list.sort();
    let mut headers = Vec::new();
    if let Some(n) = limit
        && list.len() > n
    {
        list.truncate(n);
        if let Some(end) = list.last() {
            let next = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("last", end)
                .append_pair("n", &n.to_string())
                .finish();
            headers.push((
                "link",
                format!("</v2/{name}/tags/list?{next}>; rel=\"next\""),
            ));
        }
    }
    let mut r = json_response(StatusCode::OK, &json!({ "name": name, "tags": list }));
    for (k, v) in headers {
        if let Ok(v) = HeaderValue::from_str(&v) {
            r.headers_mut().insert(k, v);
        }
    }
    Ok(r)
}

async fn referrers(
    app: &Shared,
    c: &Caller,
    ip: &str,
    req: &Parts,
    name: &str,
    reference: &str,
) -> Answer {
    if req.method != Method::GET {
        return Err(not_allowed());
    }
    let d = digest_or_refuse(reference)?;
    let repo = match open(app, c, ip, name, false).await {
        Ok(a) => Some(a.repo.id),
        Err(e) if e.code == "NAME_UNKNOWN" => None,
        Err(e) => return Err(e),
    };
    let filter = query(&req.uri)
        .get("artifactType")
        .cloned()
        .unwrap_or_default();
    let mut list = Vec::new();
    if let Some(repo) = repo {
        for m in app
            .store
            .begin()
            .await?
            .referrers(repo, &d.to_string())
            .await?
        {
            if !filter.is_empty() && m.artifact_type != filter {
                continue;
            }
            let mut x =
                json!({ "mediaType": m.media_type, "digest": m.digest, "size": m.content.len() });
            if !m.artifact_type.is_empty() {
                x["artifactType"] = json!(m.artifact_type);
            }
            if let Some(a) = m
                .annotations
                .filter(|a| a.as_object().is_some_and(|o| !o.is_empty()))
            {
                x["annotations"] = a;
            }
            list.push(x);
        }
    }
    let mut r = json_response(
        StatusCode::OK,
        &json!({ "schemaVersion": 2, "mediaType": OCI_INDEX, "manifests": list }),
    );
    r.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(OCI_INDEX));
    if !filter.is_empty() {
        r.headers_mut().insert(
            "oci-filters-applied",
            HeaderValue::from_static("artifactType"),
        );
    }
    Ok(r)
}

/// The registry's host, when the server runs one.
pub fn host(app: &crate::http::App) -> Option<String> {
    app.cfg
        .public
        .as_ref()
        .filter(|_| app.cfg.registry)
        .map(|p| p.name("registry"))
}

/// Fails unless p may pull the image a reference names, when it is one of
/// this registry's: a template's saver must reach what it names, since
/// workers pull whatever a template names.
pub async fn check_pull(
    store: &Store,
    registry_host: Option<&str>,
    p: &Principal,
    image: &str,
) -> fail::Result<()> {
    let Some(host) = registry_host else {
        return Ok(());
    };
    let Some((domain, rest)) = image.split_once('/') else {
        return Ok(());
    };
    if !domain.eq_ignore_ascii_case(host) {
        return Ok(());
    }
    let path = rest.split('@').next().unwrap_or(rest);
    let path = match path.rsplit_once(':') {
        Some((p, tag)) if !tag.contains('/') => p,
        _ => path,
    };
    let w = World::load(&mut store.begin().await?).await?;
    if w.lookup(&Caller::Person(p.clone()), path).is_none() {
        return Err(Fail::Invalid(format!(
            "invalid: image {image}: not found: no repository {path} you may pull"
        )));
    }
    Ok(())
}

/// A repository as the web UI shows it.
pub struct RepositoryView {
    pub repo: ImageRepository,
    pub owner: Option<crate::templates::Person>,
    pub team: Option<crate::templates::TeamRef>,
    pub collaborators: Vec<crate::templates::Person>,
    pub collaborator_teams: Vec<crate::templates::TeamRef>,
    pub tag_count: usize,
    pub tags: Option<Vec<TagView>>,
    pub can_push: bool,
    pub can_manage: bool,
}

pub struct TagView {
    pub name: String,
    pub digest: String,
    pub media_type: String,
    pub size: i64,
    pub updated_at: chrono::DateTime<Utc>,
}

/// An image manifest's size, its config's and layers'; an index's, its
/// largest image's.
fn image_size(content: &[u8], child: &dyn Fn(&str) -> Option<Vec<u8>>) -> i64 {
    let Ok(m) = serde_json::from_slice::<Value>(content) else {
        return 0;
    };
    let size_of = |m: &Value| -> Option<i64> {
        let config = m.get("config")?;
        let mut size = config.get("size").and_then(Value::as_i64).unwrap_or(0);
        for l in m
            .get("layers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            size += l.get("size").and_then(Value::as_i64).unwrap_or(0);
        }
        Some(size)
    };
    if let Some(s) = size_of(&m) {
        return s;
    }
    m.get("manifests")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|x| child(x.get("digest").and_then(Value::as_str).unwrap_or("")))
        .filter_map(|c| serde_json::from_slice::<Value>(&c).ok())
        .filter_map(|c| size_of(&c))
        .max()
        .unwrap_or(0)
}

/// The repositories p may pull, by path, or the one with id with its tags.
pub async fn repositories(
    store: &Store,
    p: &Principal,
    only: Option<Uuid>,
) -> crate::store::Result<Vec<RepositoryView>> {
    let mut tx = store.begin().await?;
    let w = World::load(&mut tx).await?;
    let people: HashMap<Uuid, crate::store::rows::User> =
        tx.people().await?.into_iter().map(|u| (u.id, u)).collect();
    let teams: HashMap<Uuid, crate::store::rows::Team> =
        tx.teams().await?.into_iter().map(|t| (t.id, t)).collect();
    let person = |id: Uuid| {
        people.get(&id).map(|u| crate::templates::Person {
            id: u.id,
            username: u.username.clone(),
            display_name: u.display_name.clone(),
        })
    };
    let team = |id: Uuid| {
        teams.get(&id).map(|t| crate::templates::TeamRef {
            id: t.id,
            slug: t.slug.clone(),
            name: t.name.clone(),
        })
    };
    let all_tags = tx.image_tags(only).await?;
    let caller = Caller::Person(p.clone());
    let mut out = Vec::new();
    for r in w.repos.iter().filter(|r| only.is_none_or(|id| r.id == id)) {
        let a = w.access(r, &caller);
        if !a.pull {
            continue;
        }
        let mut collaborators: Vec<_> = w
            .collaborators
            .iter()
            .filter(|(repo, _)| *repo == r.id)
            .filter_map(|(_, u)| person(*u))
            .collect();
        collaborators.sort_by_key(|c| c.username.to_lowercase());
        let mut collaborator_teams: Vec<_> = w
            .team_collaborators
            .iter()
            .filter(|(repo, _)| *repo == r.id)
            .filter_map(|(_, t)| team(*t))
            .collect();
        collaborator_teams.sort_by_key(|t| t.name.to_lowercase());
        let tag_count = all_tags.iter().filter(|t| t.repository_id == r.id).count();
        let tags = match only {
            Some(_) => {
                let mut ts: Vec<_> = all_tags
                    .iter()
                    .filter(|t| t.repository_id == r.id)
                    .cloned()
                    .collect();
                ts.sort_by(|a, b| {
                    b.updated_at
                        .cmp(&a.updated_at)
                        .then_with(|| a.tag.cmp(&b.tag))
                });
                let mut views = Vec::new();
                for t in ts {
                    let Some(m) = tx.manifest(r.id, Some(&t.digest), None).await? else {
                        continue;
                    };
                    let mut children: HashMap<String, Vec<u8>> = HashMap::new();
                    if let Ok(v) = serde_json::from_slice::<Value>(&m.content) {
                        for x in v
                            .get("manifests")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                        {
                            let d = x
                                .get("digest")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            if let Some(c) = tx.manifest(r.id, Some(&d), None).await? {
                                children.insert(d, c.content);
                            }
                        }
                    }
                    views.push(TagView {
                        name: t.tag,
                        digest: t.digest,
                        media_type: m.media_type.clone(),
                        size: image_size(&m.content, &|d| children.get(d).cloned()),
                        updated_at: t.updated_at,
                    });
                }
                Some(views)
            }
            None => None,
        };
        out.push(RepositoryView {
            owner: r.owner_id.and_then(person),
            team: r.team_id.and_then(team),
            collaborators,
            collaborator_teams,
            tag_count,
            tags,
            can_push: a.push,
            can_manage: a.manage,
            repo: r.clone(),
        });
    }
    Ok(out)
}

async fn one(store: &Store, p: &Principal, id: Uuid) -> fail::Result<RepositoryView> {
    repositories(store, p, Some(id))
        .await?
        .pop()
        .ok_or_else(|| Fail::NotFound("repository not found".into()))
}

/// Changes a repository's visibility and description.
pub async fn update(
    store: &Store,
    p: &Principal,
    ip: &str,
    id: Uuid,
    visibility: &str,
    description: &str,
) -> fail::Result<RepositoryView> {
    if visibility != "private" && visibility != "shared" {
        return fail::invalid("visibility must be private or shared");
    }
    let description = description.trim();
    if description.chars().count() > 2000 {
        return fail::invalid("a description is at most 2000 characters");
    }
    let cur = one(store, p, id).await?;
    if !cur.can_manage {
        return fail::forbidden("only the repository's owner may change it");
    }
    let mut tx = store.begin().await?;
    tx.lock_image_repository(id).await?;
    let mut r = cur.repo.clone();
    r.visibility = visibility.to_string();
    r.description = description.to_string();
    r.updated_at = Utc::now();
    tx.save_image_repository(&r).await?;
    let mut details = json!({});
    if cur.repo.visibility != visibility {
        details["visibility"] = json!({ "from": cur.repo.visibility, "to": visibility });
    }
    audit::record(
        &mut tx,
        &p.actor(ip),
        Event::new("image_repository.update", "image", &r.path, &r.path).details(details),
    )
    .await?;
    tx.commit().await?;
    one(store, p, id).await
}

/// Sets who besides its owner may push to a repository.
pub async fn set_collaborators(
    store: &Store,
    p: &Principal,
    ip: &str,
    id: Uuid,
    users: &[String],
    teams: &[String],
) -> fail::Result<RepositoryView> {
    let parse = |xs: &[String]| -> fail::Result<Vec<Uuid>> {
        xs.iter()
            .map(|x| {
                Uuid::parse_str(x).or_else(|_| fail::invalid(format!("unknown user or team {x:?}")))
            })
            .collect()
    };
    let (mut users, mut teams) = (parse(users)?, parse(teams)?);
    let cur = one(store, p, id).await?;
    if !cur.can_push {
        return fail::forbidden(
            "only those who may push to the repository choose its collaborators",
        );
    }
    users.retain(|u| cur.repo.owner_id != Some(*u));
    users.sort();
    users.dedup();
    teams.retain(|t| cur.repo.team_id != Some(*t));
    teams.sort();
    teams.dedup();
    let mut tx = store.begin().await?;
    tx.lock_image_repository(id).await?;
    let people: Vec<Uuid> = tx.people().await?.into_iter().map(|u| u.id).collect();
    if users.iter().any(|u| !people.contains(u)) {
        return fail::invalid("unknown user among collaborators");
    }
    let known: Vec<Uuid> = tx.teams().await?.into_iter().map(|t| t.id).collect();
    if teams.iter().any(|t| !known.contains(t)) {
        return fail::invalid("unknown team among collaborators");
    }
    tx.set_repository_collaborators(id, &users, &teams).await?;
    audit::record(
        &mut tx,
        &p.actor(ip),
        Event::new(
            "image_repository.set_collaborators",
            "image",
            &cur.repo.path,
            &cur.repo.path,
        )
        .details(json!({ "collaborators": users, "teams": teams })),
    )
    .await?;
    tx.commit().await?;
    match one(store, p, id).await {
        Ok(v) => Ok(v),
        Err(Fail::NotFound(_)) => {
            let admin = Principal {
                admin: true,
                ..p.clone()
            };
            let mut v = one(store, &admin, id).await?;
            v.can_push = false;
            v.can_manage = false;
            Ok(v)
        }
        Err(e) => Err(e),
    }
}

/// Deletes one of a repository's tags.
pub async fn delete_tag(
    store: &Store,
    p: &Principal,
    ip: &str,
    id: Uuid,
    tag: &str,
) -> fail::Result<RepositoryView> {
    let cur = one(store, p, id).await?;
    if !cur.can_push {
        return fail::forbidden("only those who may push to the repository delete its tags");
    }
    let mut tx = store.begin().await?;
    if !tx.delete_tag(id, tag).await? {
        return fail::invalid(format!("no tag {tag}"));
    }
    audit::record(
        &mut tx,
        &p.actor(ip),
        Event::new("image.delete", "image", &cur.repo.path, &cur.repo.path)
            .details(json!({ "tag": tag })),
    )
    .await?;
    tx.commit().await?;
    one(store, p, id).await
}

/// Deletes a repository and everything in it; its blobs go at the next
/// collection.
pub async fn delete(store: &Store, p: &Principal, ip: &str, id: Uuid) -> fail::Result<()> {
    let cur = one(store, p, id).await?;
    if !cur.can_manage {
        return fail::forbidden("only the repository's owner may delete it");
    }
    let mut tx = store.begin().await?;
    tx.delete_image_repository(id).await?;
    audit::record(
        &mut tx,
        &p.actor(ip),
        Event::new(
            "image_repository.delete",
            "image",
            &cur.repo.path,
            &cur.repo.path,
        ),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// What a collection removed.
#[derive(Default)]
pub struct Collected {
    pub manifests: u64,
    pub blobs: u64,
    pub bytes: i64,
}

/// Deletes what nothing keeps: manifests no tag, recent push, pinned
/// environment or kept manifest names, then the blobs no repository uses,
/// from the store and the blob store. Nothing made within grace goes, so
/// no push in progress is caught halfway.
pub async fn collect(
    store: &Store,
    blobs: &Blobs,
    grace: chrono::Duration,
) -> Result<Collected, String> {
    let cutoff = Utc::now() - grace;
    let mut out = Collected::default();
    let mut tx = store.begin().await.map_err(|e| e.to_string())?;
    out.manifests = tx
        .collect_manifests(cutoff)
        .await
        .map_err(|e| e.to_string())?;
    tx.commit().await.map_err(|e| e.to_string())?;
    let orphans = store
        .begin()
        .await
        .map_err(|e| e.to_string())?
        .orphan_blobs(cutoff)
        .await
        .map_err(|e| e.to_string())?;
    for digest in orphans {
        let Some(d) = Digest::parse(&digest) else {
            continue;
        };
        let mut tx = store.begin().await.map_err(|e| e.to_string())?;
        tx.lock_blob(&digest).await.map_err(|e| e.to_string())?;
        if let Some(size) = tx.forget_blob(&digest).await.map_err(|e| e.to_string())? {
            blobs.delete(&d.key()).await.map_err(|e| e.to_string())?;
            tx.commit().await.map_err(|e| e.to_string())?;
            out.blobs += 1;
            out.bytes += size;
        }
    }
    Ok(out)
}

/// Deletes the uploads begun over a day ago, and their chunks.
pub async fn prune_uploads(store: &Store, blobs: &Blobs) -> Result<usize, String> {
    let mut tx = store.begin().await.map_err(|e| e.to_string())?;
    let gone = tx
        .prune_uploads(Utc::now() - chrono::Duration::hours(24))
        .await
        .map_err(|e| e.to_string())?;
    tx.commit().await.map_err(|e| e.to_string())?;
    for id in &gone {
        drop_chunks(blobs, *id).await.map_err(|e| e.message)?;
    }
    Ok(gone.len())
}
