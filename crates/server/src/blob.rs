//! Where the server keeps files too large for its store: the registry's
//! blobs. A deployment keeps them in an S3-compatible
//! object store (`HANGAR_BLOB_URL`, http(s)://key:secret@host/bucket); a
//! server on one machine may keep them in a directory instead
//! (file:///path).

use std::path::{Path, PathBuf};
use std::pin::Pin;

use bytes::Bytes;
use chrono::Utc;
use futures_util::{Stream, TryStreamExt};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt};

/// Where the store is (`HANGAR_BLOB_URL`), and how it is signed in to.
#[derive(Debug, Clone)]
pub struct Config {
    pub url: String,
    /// The keys, when not in the URL.
    pub access: Option<String>,
    pub secret: Option<String>,
    pub region: String,
    /// Has the store encrypt what it keeps (SSE-S3).
    pub encrypt: bool,
}

/// An object kept.
#[derive(Debug, Clone)]
pub struct Object {
    pub key: String,
    pub size: u64,
}

/// An object's content, or a range of it, as it is read.
pub struct Body {
    pub stream: Pin<Box<dyn Stream<Item = std::io::Result<Bytes>> + Send>>,
}

/// A prefix whose objects the store deletes once they are this many days
/// old: what a failed upload leaves.
pub struct Expiry {
    pub prefix: &'static str,
    pub days: u32,
}

#[derive(Debug)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

fn io(e: impl std::fmt::Display) -> Error {
    Error(e.to_string())
}

pub enum Blobs {
    S3(S3),
    Dir(Dir),
}

/// How big each part of an upload of unknown size is.
const PART: usize = 16 << 20;

impl Blobs {
    /// The store cfg names: http(s) for S3, file for a directory.
    pub async fn open(cfg: &Config, expire: &[Expiry]) -> Result<Blobs> {
        if let Some(path) = cfg.url.strip_prefix("file://") {
            return Ok(Blobs::Dir(Dir::open(Path::new(path))?));
        }
        Ok(Blobs::S3(S3::open(cfg, expire).await?))
    }

    /// Keeps what r reads as key, however long it is.
    pub async fn put(&self, key: &str, r: impl AsyncRead + Unpin + Send) -> Result<()> {
        match self {
            Blobs::S3(s) => s.put(key, r).await,
            Blobs::Dir(d) => d.put(key, r).await,
        }
    }

    /// An object, or from start to end (inclusive) of it; None if there is
    /// no such object.
    pub async fn get(&self, key: &str, range: Option<(u64, Option<u64>)>) -> Result<Option<Body>> {
        match self {
            Blobs::S3(s) => s.get(key, range).await,
            Blobs::Dir(d) => d.get(key, range).await,
        }
    }

    pub async fn stat(&self, key: &str) -> Result<Option<Object>> {
        match self {
            Blobs::S3(s) => s.stat(key).await,
            Blobs::Dir(d) => d.stat(key),
        }
    }

    pub async fn copy(&self, src: &str, dst: &str) -> Result<()> {
        match self {
            Blobs::S3(s) => s.copy(src, dst).await,
            Blobs::Dir(d) => d.copy(src, dst),
        }
    }

    /// Deletes an object; one not there is no error.
    pub async fn delete(&self, key: &str) -> Result<()> {
        match self {
            Blobs::S3(s) => s.delete(key).await,
            Blobs::Dir(d) => d.delete(key),
        }
    }

    /// The objects whose keys start with prefix, by key.
    pub async fn list(&self, prefix: &str) -> Result<Vec<Object>> {
        let mut out = match self {
            Blobs::S3(s) => s.list(prefix).await?,
            Blobs::Dir(d) => d.list(prefix)?,
        };
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }
}

/// Fills buf from r as far as it goes, giving how much it read.
async fn fill(r: &mut (impl AsyncRead + Unpin), buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        let m = r.read(&mut buf[n..]).await?;
        if m == 0 {
            break;
        }
        n += m;
    }
    Ok(n)
}

/// A directory of objects, each a file at its key.
pub struct Dir(PathBuf);

impl Dir {
    fn open(path: &Path) -> Result<Dir> {
        std::fs::create_dir_all(path).map_err(|e| Error(format!("{}: {e}", path.display())))?;
        Ok(Dir(path.to_path_buf()))
    }

    fn path(&self, key: &str) -> Result<PathBuf> {
        if key
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
        {
            return Err(Error(format!("{key:?} is not an object's key")));
        }
        Ok(self.0.join(key))
    }

    async fn put(&self, key: &str, mut r: impl AsyncRead + Unpin + Send) -> Result<()> {
        let path = self.path(key)?;
        let dir = path.parent().expect("a parent");
        tokio::fs::create_dir_all(dir).await.map_err(io)?;
        let tmp = dir.join(format!(".put-{}", uuid::Uuid::new_v4()));
        let mut f = tokio::fs::File::create(&tmp).await.map_err(io)?;
        let done = async {
            tokio::io::copy(&mut r, &mut f).await?;
            f.sync_all().await?;
            tokio::fs::rename(&tmp, &path).await
        };
        if let Err(e) = done.await {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(io(e));
        }
        Ok(())
    }

    async fn get(&self, key: &str, range: Option<(u64, Option<u64>)>) -> Result<Option<Body>> {
        let mut f = match tokio::fs::File::open(self.path(key)?).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io(e)),
        };
        let size = f.metadata().await.map_err(io)?.len();
        let (start, end) = match range {
            Some((s, e)) => (
                s,
                e.unwrap_or(size.saturating_sub(1))
                    .min(size.saturating_sub(1)),
            ),
            None => (0, size.saturating_sub(1)),
        };
        f.seek(std::io::SeekFrom::Start(start)).await.map_err(io)?;
        let len = if size == 0 { 0 } else { end + 1 - start };
        let stream = tokio_util::io::ReaderStream::new(f.take(len));
        Ok(Some(Body {
            stream: Box::pin(stream),
        }))
    }

    fn stat(&self, key: &str) -> Result<Option<Object>> {
        match std::fs::metadata(self.path(key)?) {
            Ok(m) => Ok(Some(Object {
                key: key.to_string(),
                size: m.len(),
            })),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io(e)),
        }
    }

    fn copy(&self, src: &str, dst: &str) -> Result<()> {
        let (from, to) = (self.path(src)?, self.path(dst)?);
        std::fs::create_dir_all(to.parent().expect("a parent")).map_err(io)?;
        let tmp = to.with_file_name(format!(".copy-{}", uuid::Uuid::new_v4()));
        if std::fs::hard_link(&from, &tmp).is_err() {
            std::fs::copy(&from, &tmp).map_err(io)?;
        }
        std::fs::rename(&tmp, &to).map_err(io)
    }

    fn delete(&self, key: &str) -> Result<()> {
        match std::fs::remove_file(self.path(key)?) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io(e)),
        }
    }

    fn list(&self, prefix: &str) -> Result<Vec<Object>> {
        let mut out = Vec::new();
        let start = match prefix.rsplit_once('/') {
            Some((dir, _)) => self.0.join(dir),
            None => self.0.clone(),
        };
        let mut stack = vec![start];
        while let Some(dir) = stack.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(e) => e,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(io(e)),
            };
            for e in entries {
                let e = e.map_err(io)?;
                let kind = e.file_type().map_err(io)?;
                if kind.is_dir() {
                    stack.push(e.path());
                    continue;
                }
                let name = e.file_name().to_string_lossy().into_owned();
                if !kind.is_file() || name.starts_with(".put-") || name.starts_with(".copy-") {
                    continue;
                }
                let key = e
                    .path()
                    .strip_prefix(&self.0)
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
                if !key.starts_with(prefix) {
                    continue;
                }
                let m = e.metadata().map_err(io)?;
                out.push(Object { key, size: m.len() });
            }
        }
        Ok(out)
    }
}

/// An S3-compatible object store's bucket, reached by path, its requests
/// signed with AWS Signature Version 4.
pub struct S3 {
    http: reqwest::Client,
    /// http(s)://host[:port]
    endpoint: String,
    host: String,
    bucket: String,
    access: String,
    secret: String,
    region: String,
    /// Has the store encrypt what it keeps (SSE-S3).
    encrypt: bool,
}

/// How long the store has to come up as the server starts.
const STARTUP: std::time::Duration = std::time::Duration::from_secs(120);

/// Characters kept as they are in a signed request's path and query.
fn encode(s: &str, slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'_' | b'.' | b'~')
            || (slash && b == b'/')
        {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    let mut m = Hmac::<Sha256>::new_from_slice(key).expect("any key");
    m.update(data.as_bytes());
    m.finalize().into_bytes().to_vec()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The text of the first <tag>…</tag> in xml.
fn tag<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&format!("</{name}>"))? + start;
    Some(&xml[start..end])
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// What a request carries: nothing, bytes whose hash is signed, or a body
/// whose hash is not.
enum Payload {
    Empty,
    Bytes(Bytes),
    Unsigned(reqwest::Body),
}

impl S3 {
    async fn open(cfg: &Config, expire: &[Expiry]) -> Result<S3> {
        let bad = || {
            Error("the object store's URL is not http(s)://key:secret@host[:port]/bucket".into())
        };
        let u = url::Url::parse(&cfg.url).map_err(|_| bad())?;
        let bucket = u.path().trim_matches('/').to_string();
        if !matches!(u.scheme(), "http" | "https")
            || u.host_str().is_none()
            || bucket.is_empty()
            || bucket.contains('/')
        {
            return Err(bad());
        }
        let host = match u.port() {
            Some(p) => format!("{}:{p}", u.host_str().unwrap_or_default()),
            None => u.host_str().unwrap_or_default().to_string(),
        };
        let decode = |s: &str| {
            url::form_urlencoded::parse(format!("x={s}").as_bytes())
                .next()
                .map(|(_, v)| v.into_owned())
                .unwrap_or_default()
        };
        let s = S3 {
            http: hangar_openapi::http_client()
                .map_err(io)?
                .build()
                .map_err(io)?,
            endpoint: format!("{}://{host}", u.scheme()),
            host,
            bucket,
            access: cfg.access.clone().unwrap_or_else(|| decode(u.username())),
            secret: cfg
                .secret
                .clone()
                .unwrap_or_else(|| decode(u.password().unwrap_or_default())),
            region: if cfg.region.is_empty() {
                "us-east-1".into()
            } else {
                cfg.region.clone()
            },
            encrypt: cfg.encrypt,
        };
        let deadline = tokio::time::Instant::now() + STARTUP;
        let mut wait = std::time::Duration::from_secs(1);
        let exists = loop {
            match s
                .send(reqwest::Method::HEAD, "", &[], &[], Payload::Empty)
                .await
            {
                Ok(r) if r.status().is_success() => break true,
                Ok(r) if r.status() == reqwest::StatusCode::NOT_FOUND => break false,
                Ok(r) if tokio::time::Instant::now() > deadline => {
                    return Err(Error(format!(
                        "reaching the object store at {}: {}",
                        s.host,
                        r.status()
                    )));
                }
                Err(e) if tokio::time::Instant::now() > deadline => {
                    return Err(Error(format!(
                        "reaching the object store at {}: {e}",
                        s.host
                    )));
                }
                _ => {
                    tokio::time::sleep(wait).await;
                    wait = (wait * 2).min(std::time::Duration::from_secs(5));
                }
            }
        };
        if !exists {
            let r = s
                .send(reqwest::Method::PUT, "", &[], &[], Payload::Empty)
                .await?;
            if !r.status().is_success() {
                let again = s
                    .send(reqwest::Method::HEAD, "", &[], &[], Payload::Empty)
                    .await?;
                if !again.status().is_success() {
                    return Err(Error(format!("making bucket {}: {}", s.bucket, r.status())));
                }
            }
        }
        if !expire.is_empty() {
            let mut xml = String::from("<LifecycleConfiguration>");
            for (i, e) in expire.iter().enumerate() {
                xml.push_str(&format!(
                    "<Rule><ID>hangar-expire-{i}</ID><Filter><Prefix>{}</Prefix></Filter><Status>Enabled</Status><Expiration><Days>{}</Days></Expiration></Rule>",
                    escape(e.prefix),
                    e.days
                ));
            }
            xml.push_str("</LifecycleConfiguration>");
            use base64::Engine;
            let md5 =
                base64::engine::general_purpose::STANDARD.encode(md5::Md5::digest(xml.as_bytes()));
            let r = s
                .send(
                    reqwest::Method::PUT,
                    "",
                    &[("lifecycle", "")],
                    &[("content-md5", &md5)],
                    Payload::Bytes(xml.into()),
                )
                .await?;
            if !r.status().is_success() {
                return Err(Error(format!(
                    "setting up bucket {}'s lifecycle: {}",
                    s.bucket,
                    s.why(r).await
                )));
            }
        }
        Ok(s)
    }

    async fn why(&self, r: reqwest::Response) -> String {
        let status = r.status();
        let body = r.text().await.unwrap_or_default();
        match tag(&body, "Message").or_else(|| tag(&body, "Code")) {
            Some(m) => format!("{status}: {}", unescape(m)),
            None => status.to_string(),
        }
    }

    /// Sends a signed request for key (none for the bucket) with query and
    /// extra headers.
    async fn send(
        &self,
        method: reqwest::Method,
        key: &str,
        query: &[(&str, &str)],
        headers: &[(&str, &str)],
        payload: Payload,
    ) -> Result<reqwest::Response> {
        let now = Utc::now();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date = now.format("%Y%m%d").to_string();
        let path = if key.is_empty() {
            format!("/{}", self.bucket)
        } else {
            format!("/{}/{}", self.bucket, encode(key, true))
        };
        let mut q: Vec<(String, String)> = query
            .iter()
            .map(|(k, v)| (encode(k, false), encode(v, false)))
            .collect();
        q.sort();
        let canonical_query = q
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        let hash = match &payload {
            Payload::Empty => hex(&Sha256::digest(b"")),
            Payload::Bytes(b) => hex(&Sha256::digest(b)),
            Payload::Unsigned(_) => "UNSIGNED-PAYLOAD".to_string(),
        };
        let mut signed: Vec<(String, String)> = vec![
            ("host".into(), self.host.clone()),
            ("x-amz-content-sha256".into(), hash.clone()),
            ("x-amz-date".into(), amz_date.clone()),
        ];
        for (k, v) in headers {
            signed.push((k.to_lowercase(), v.trim().to_string()));
        }
        signed.sort();
        let names = signed
            .iter()
            .map(|(k, _)| k.as_str())
            .collect::<Vec<_>>()
            .join(";");
        let canonical = format!(
            "{method}\n{path}\n{canonical_query}\n{}\n{names}\n{hash}",
            signed
                .iter()
                .map(|(k, v)| format!("{k}:{v}\n"))
                .collect::<String>()
        );
        let scope = format!("{date}/{}/s3/aws4_request", self.region);
        let to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
            hex(&Sha256::digest(canonical.as_bytes()))
        );
        let key = [date.as_str(), self.region.as_str(), "s3", "aws4_request"]
            .iter()
            .fold(format!("AWS4{}", self.secret).into_bytes(), |k, part| {
                hmac(&k, part)
            });
        let signature = hex(&hmac(&key, &to_sign));
        let mut url = format!("{}{path}", self.endpoint);
        if !canonical_query.is_empty() {
            url.push('?');
            url.push_str(&canonical_query);
        }
        let mut req = self.http.request(method, url).header(
            "authorization",
            format!("AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={names}, Signature={signature}", self.access),
        );
        for (k, v) in &signed {
            if k != "host" {
                req = req.header(k.as_str(), v.as_str());
            }
        }
        req = match payload {
            Payload::Empty => req,
            Payload::Bytes(b) => req.body(b),
            Payload::Unsigned(b) => req.body(b),
        };
        req.send()
            .await
            .map_err(|e| Error(format!("the object store: {e}")))
    }

    fn sse(&self) -> Vec<(&'static str, &'static str)> {
        if self.encrypt {
            vec![("x-amz-server-side-encryption", "AES256")]
        } else {
            Vec::new()
        }
    }

    async fn put(&self, key: &str, mut r: impl AsyncRead + Unpin + Send) -> Result<()> {
        let mut first = vec![0u8; PART];
        let n = fill(&mut r, &mut first).await.map_err(io)?;
        first.truncate(n);
        if n < PART {
            let len = n.to_string();
            let mut headers = self.sse();
            headers.push(("content-length", &len));
            headers.push(("content-type", "application/octet-stream"));
            let resp = self
                .send(
                    reqwest::Method::PUT,
                    key,
                    &[],
                    &headers,
                    Payload::Unsigned(first.into()),
                )
                .await?;
            if !resp.status().is_success() {
                return Err(Error(format!("putting {key}: {}", self.why(resp).await)));
            }
            return Ok(());
        }
        // Too long to know: a multipart upload, a part at a time.
        let mut headers = self.sse();
        headers.push(("content-type", "application/octet-stream"));
        let resp = self
            .send(
                reqwest::Method::POST,
                key,
                &[("uploads", "")],
                &headers,
                Payload::Empty,
            )
            .await?;
        if !resp.status().is_success() {
            return Err(Error(format!(
                "starting an upload of {key}: {}",
                self.why(resp).await
            )));
        }
        let body = resp.text().await.map_err(io)?;
        let id = tag(&body, "UploadId")
            .map(unescape)
            .ok_or_else(|| Error("no upload ID".into()))?;
        let parts = async {
            let mut etags = Vec::new();
            let mut part = first;
            loop {
                let number = (etags.len() + 1).to_string();
                let len = part.len().to_string();
                let resp = self
                    .send(
                        reqwest::Method::PUT,
                        key,
                        &[("partNumber", &number), ("uploadId", &id)],
                        &[("content-length", &len)],
                        Payload::Unsigned(part.into()),
                    )
                    .await?;
                if !resp.status().is_success() {
                    return Err(Error(format!(
                        "putting part {number} of {key}: {}",
                        self.why(resp).await
                    )));
                }
                let etag = resp
                    .headers()
                    .get("etag")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                etags.push(etag);
                part = vec![0u8; PART];
                let n = fill(&mut r, &mut part).await.map_err(io)?;
                if n == 0 {
                    return Ok(etags);
                }
                part.truncate(n);
            }
        };
        let etags = match parts.await {
            Ok(e) => e,
            Err(e) => {
                let _ = self
                    .send(
                        reqwest::Method::DELETE,
                        key,
                        &[("uploadId", &id)],
                        &[],
                        Payload::Empty,
                    )
                    .await;
                return Err(e);
            }
        };
        let mut xml = String::from("<CompleteMultipartUpload>");
        for (i, e) in etags.iter().enumerate() {
            xml.push_str(&format!(
                "<Part><PartNumber>{}</PartNumber><ETag>{}</ETag></Part>",
                i + 1,
                escape(e)
            ));
        }
        xml.push_str("</CompleteMultipartUpload>");
        let resp = self
            .send(
                reqwest::Method::POST,
                key,
                &[("uploadId", &id)],
                &[],
                Payload::Bytes(xml.into()),
            )
            .await?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() || body.contains("<Error>") {
            return Err(Error(format!(
                "completing the upload of {key}: {status} {body}"
            )));
        }
        Ok(())
    }

    async fn get(&self, key: &str, range: Option<(u64, Option<u64>)>) -> Result<Option<Body>> {
        let range = range.map(|(s, e)| match e {
            Some(e) => format!("bytes={s}-{e}"),
            None => format!("bytes={s}-"),
        });
        let headers: Vec<(&str, &str)> = range.iter().map(|r| ("range", r.as_str())).collect();
        let resp = self
            .send(reqwest::Method::GET, key, &[], &headers, Payload::Empty)
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(Error(format!("getting {key}: {}", self.why(resp).await)));
        }
        let stream = resp.bytes_stream().map_err(std::io::Error::other);
        Ok(Some(Body {
            stream: Box::pin(stream),
        }))
    }

    async fn stat(&self, key: &str) -> Result<Option<Object>> {
        let resp = self
            .send(reqwest::Method::HEAD, key, &[], &[], Payload::Empty)
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(Error(format!("looking at {key}: {}", resp.status())));
        }
        Ok(Some(Object {
            key: key.to_string(),
            size: resp.content_length().unwrap_or(0),
        }))
    }

    async fn copy(&self, src: &str, dst: &str) -> Result<()> {
        let source = format!("/{}/{}", self.bucket, encode(src, true));
        let mut headers = self.sse();
        headers.push(("x-amz-copy-source", &source));
        let resp = self
            .send(reqwest::Method::PUT, dst, &[], &headers, Payload::Empty)
            .await?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() || body.contains("<Error>") {
            return Err(Error(format!(
                "copying {src} to {dst}: {status} {}",
                tag(&body, "Message").unwrap_or(&body)
            )));
        }
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<()> {
        let resp = self
            .send(reqwest::Method::DELETE, key, &[], &[], Payload::Empty)
            .await?;
        if !resp.status().is_success() && resp.status() != reqwest::StatusCode::NOT_FOUND {
            return Err(Error(format!("deleting {key}: {}", self.why(resp).await)));
        }
        Ok(())
    }

    async fn list(&self, prefix: &str) -> Result<Vec<Object>> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut query = vec![("list-type", "2"), ("prefix", prefix)];
            if let Some(t) = &token {
                query.push(("continuation-token", t));
            }
            let resp = self
                .send(reqwest::Method::GET, "", &query, &[], Payload::Empty)
                .await?;
            if !resp.status().is_success() {
                return Err(Error(format!("listing {prefix}: {}", self.why(resp).await)));
            }
            let body = resp.text().await.map_err(io)?;
            let mut rest = body.as_str();
            while let Some(start) = rest.find("<Contents>") {
                let end = rest[start..]
                    .find("</Contents>")
                    .map(|e| e + start)
                    .unwrap_or(rest.len());
                let c = &rest[start..end];
                if let Some(key) = tag(c, "Key") {
                    out.push(Object {
                        key: unescape(key),
                        size: tag(c, "Size").and_then(|s| s.parse().ok()).unwrap_or(0),
                    });
                }
                rest = &rest[end..];
            }
            if tag(&body, "IsTruncated") != Some("true") {
                return Ok(out);
            }
            match tag(&body, "NextContinuationToken") {
                Some(t) => token = Some(unescape(t)),
                None => return Ok(out),
            }
        }
    }
}

/// Writes a stream to an async reader's worth, for putting what a request
/// carries.
pub fn reader(
    s: impl Stream<Item = std::io::Result<Bytes>> + Send + Unpin,
) -> impl AsyncRead + Unpin + Send {
    tokio_util::io::StreamReader::new(s)
}
