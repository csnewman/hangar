//! A worker's logs, for the control plane: the files an environment's
//! machine writes -- its console, its monitor's, its backends' -- and the
//! worker's own log, kept in memory, whole and per environment.
//!
//! Every log reads as a stream of bytes that only grows, so a reader asks
//! for what follows the offset it has read to. The oldest of a long log may
//! be gone, from a file's start or a ring's; `start` in a reply says where
//! what it holds begins.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The guest's serial console: the kernel's messages, the agent as init
/// assembling the root, and systemd.
pub const CONSOLE: &str = "console";
/// The monitor's own log.
pub const MONITOR: &str = "monitor";
/// The virtual GPU's backend.
pub const GPU: &str = "gpu";
/// What the worker logged about the environment, or, for the worker as a
/// whole, everything it logged.
pub const WORKER: &str = "worker";
/// All of an environment's other logs as one: each line as the worker sees
/// it written, marked with the log it is from. None of them says when its
/// lines were written -- the console has no clock to speak of -- so the
/// order they reach the worker is the only one there is.
pub const COMBINED: &str = "combined";

/// The most a reply holds.
pub const MAX_READ: i64 = 256 << 10;

/// How much of the worker's log is kept in memory: the latest of it whole,
/// and of each environment's lines.
pub const WHOLE_SIZE: usize = 1 << 20;
pub const ENVIRONMENT_SIZE: usize = 64 << 10;
/// How much of each environment's combined log is kept.
pub const COMBINED_SIZE: usize = 256 << 10;

/// How often the combined logs are read on, which is how finely lines of
/// different logs are put in order.
pub const COMBINE_EVERY: Duration = Duration::from_millis(250);

/// How long a line with no end yet waits for one before it is shown as it
/// is: a prompt, or a program that writes a line in pieces.
const PARTIAL_AFTER: Duration = Duration::from_secs(1);

/// The logs a combined log is made of, in the order each pass reads them.
const SOURCES: [&str; 4] = [WORKER, CONSOLE, MONITOR, GPU];

/// Asks for one log. An environment of "" asks for the worker's whole log.
/// A negative offset asks for the end: the last `max` bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub environment: String,
    #[serde(default)]
    pub log: String,
    #[serde(default)]
    pub offset: i64,
    #[serde(default)]
    pub max: i64,
}

/// Part of a log: data, from start, of a log size bytes long so far. A
/// reader continues from start+data.len().
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    pub start: i64,
    pub size: i64,
    /// Base64, as Go writes a byte slice.
    #[serde(
        serialize_with = "to_base64",
        deserialize_with = "from_base64",
        default
    )]
    pub data: Vec<u8>,
    /// There is no such log yet: the machine has not been started here, or
    /// has no GPU.
    #[serde(default, skip_serializing_if = "is_false")]
    pub missing: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
}

impl Reply {
    pub fn missing() -> Reply {
        Reply {
            missing: true,
            ..Reply::default()
        }
    }

    pub fn error(e: impl Into<String>) -> Reply {
        Reply {
            error: e.into(),
            ..Reply::default()
        }
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn to_base64<S: Serializer>(b: &[u8], s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(b))
}

fn from_base64<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
    let s: Option<String> = Option::deserialize(d)?;
    base64::engine::general_purpose::STANDARD
        .decode(s.unwrap_or_default())
        .map_err(serde::de::Error::custom)
}

/// The window a request reads of a log size bytes long, of which the oldest
/// kept begins at first.
fn clamp(req: &Request, first: i64, size: i64) -> (i64, i64) {
    let limit = if req.max <= 0 || req.max > MAX_READ {
        MAX_READ
    } else {
        req.max
    };
    let mut start = req.offset;
    if start < 0 || start > size {
        start = size - limit;
    }
    let start = start.max(first).min(size);
    (start, limit.min(size - start))
}

/// Reads part of a log file.
pub fn read_file(path: &Path, req: &Request) -> io::Result<Reply> {
    let mut f = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Reply::missing()),
        Err(e) => return Err(e),
    };
    read_open(&mut f, req)
}

fn read_open(f: &mut File, req: &Request) -> io::Result<Reply> {
    let size = f.metadata()?.len() as i64;
    let (start, n) = clamp(req, 0, size);
    f.seek(SeekFrom::Start(start as u64))?;
    let mut data = Vec::with_capacity(n as usize);
    f.take(n as u64).read_to_end(&mut data)?;
    Ok(Reply {
        start,
        size,
        data,
        ..Reply::default()
    })
}

/// Keeps the latest bytes written to a log that only grows.
struct Ring {
    buf: Vec<u8>,
    cap: usize,
    /// How much has ever been written, so the buffer holds what begins at
    /// end-buf.len().
    end: i64,
}

impl Ring {
    fn new(cap: usize) -> Ring {
        Ring {
            buf: Vec::new(),
            cap,
            end: 0,
        }
    }

    fn write(&mut self, p: &[u8]) {
        self.end += p.len() as i64;
        self.buf.extend_from_slice(p);
        if self.buf.len() > self.cap {
            let mut over = self.buf.len() - self.cap;
            // Drop whole lines, so what is kept starts at one.
            if let Some(i) = self.buf[over..].iter().position(|&b| b == b'\n') {
                over += i + 1;
            }
            self.buf.drain(..over);
        }
    }

    fn read(&self, req: &Request) -> Reply {
        let first = self.end - self.buf.len() as i64;
        let (start, n) = clamp(req, first, self.end);
        let from = (start - first) as usize;
        Reply {
            start,
            size: self.end,
            data: self.buf[from..from + n as usize].to_vec(),
            ..Reply::default()
        }
    }
}

/// Keeps the worker's log in memory, whole and per environment: each line
/// goes to the whole log and, if it names an environment, to that
/// environment's.
pub struct Rings {
    inner: Mutex<RingsInner>,
}

struct RingsInner {
    whole: Ring,
    envs: HashMap<String, Ring>,
}

impl Default for Rings {
    fn default() -> Rings {
        Rings::new()
    }
}

impl Rings {
    pub fn new() -> Rings {
        Rings {
            inner: Mutex::new(RingsInner {
                whole: Ring::new(WHOLE_SIZE),
                envs: HashMap::new(),
            }),
        }
    }

    /// Keeps one line of the worker's log, which is about env if that is
    /// not empty.
    pub fn write(&self, env: &str, line: &[u8]) {
        let mut r = self.inner.lock().unwrap();
        r.whole.write(line);
        if env.is_empty() {
            return;
        }
        r.envs
            .entry(env.to_string())
            .or_insert_with(|| Ring::new(ENVIRONMENT_SIZE))
            .write(line);
    }

    /// Reads the worker's whole log, or an environment's lines of it.
    pub fn read(&self, req: &Request) -> Reply {
        let r = self.inner.lock().unwrap();
        if req.environment.is_empty() {
            return r.whole.read(req);
        }
        match r.envs.get(&req.environment) {
            Some(e) => e.read(req),
            None => Reply::missing(),
        }
    }

    /// The environments the rings have lines of.
    pub fn environments(&self) -> Vec<String> {
        self.inner.lock().unwrap().envs.keys().cloned().collect()
    }

    /// Drops an environment's lines, once it is gone.
    pub fn forget(&self, env: &str) {
        self.inner.lock().unwrap().envs.remove(env);
    }
}

/// Where an environment's log of a name is, if it is one the environment
/// has.
pub type LogFiles = Arc<dyn Fn(&str, &str) -> Option<PathBuf> + Send + Sync>;

/// Keeps each environment's combined log, reading on in the logs it is
/// made of as they grow. It follows the environments the rings have lines
/// of, which is every one the worker has been asked to run.
pub struct Combiner {
    rings: Arc<Rings>,
    files: LogFiles,
    envs: Mutex<HashMap<String, Arc<Mutex<Combined>>>>,
}

/// One environment's combined log and how far each of its sources has been
/// read.
struct Combined {
    ring: Ring,
    sources: HashMap<&'static str, Source>,
}

#[derive(Default)]
struct Source {
    next: i64,
    partial: Vec<u8>,
    since: Option<Instant>,
    /// The file a file's log was last read from, held open: one put in its
    /// place is another log, read from its start. Held, its inode cannot be
    /// given to the file that replaces it, which would then look the same.
    file: Option<File>,
}

impl Combiner {
    /// Combines an environment's lines of the worker's log in rings with
    /// the files its machine writes, which files says where they are.
    pub fn new(rings: Arc<Rings>, files: LogFiles) -> Combiner {
        Combiner {
            rings,
            files,
            envs: Mutex::new(HashMap::new()),
        }
    }

    /// Reads on every COMBINE_EVERY, for as long as the combiner is held
    /// elsewhere.
    pub async fn run(self: Arc<Self>) {
        let weak = Arc::downgrade(&self);
        drop(self);
        let mut t = tokio::time::interval(COMBINE_EVERY);
        loop {
            t.tick().await;
            let Some(c) = weak.upgrade() else { return };
            let _ = tokio::task::spawn_blocking(move || c.poll()).await;
        }
    }

    /// Reads on once in every environment's logs. An environment seen for
    /// the first time takes its worker lines from the oldest kept, and its
    /// files from their ends: a machine does not outlive the worker that
    /// started it, so what they hold already is from before this one. One
    /// the rings have forgotten is gone, and so is its combined log.
    pub fn poll(&self) {
        let envs = self.rings.environments();
        self.envs
            .lock()
            .unwrap()
            .retain(|env, _| envs.contains(env));
        for env in envs {
            let e = {
                let mut all = self.envs.lock().unwrap();
                match all.get(&env) {
                    Some(e) => e.clone(),
                    None => {
                        let mut sources = HashMap::new();
                        for name in SOURCES {
                            let mut s = Source::default();
                            if name != WORKER
                                && let Some(path) = (self.files)(&env, name)
                                && let Ok(f) = File::open(path)
                            {
                                s.next = f.metadata().map(|m| m.len() as i64).unwrap_or(0);
                                s.file = Some(f);
                            }
                            sources.insert(name, s);
                        }
                        let e = Arc::new(Mutex::new(Combined {
                            ring: Ring::new(COMBINED_SIZE),
                            sources,
                        }));
                        all.insert(env.clone(), e.clone());
                        e
                    }
                }
            };
            let mut e = e.lock().unwrap();
            for name in SOURCES {
                self.follow(&env, &mut e, name);
            }
        }
    }

    /// Reads on in one source of an environment's combined log.
    fn read(&self, env: &str, s: &mut Source, name: &str) -> Option<Reply> {
        let mut req = Request {
            environment: env.to_string(),
            log: name.to_string(),
            offset: s.next,
            max: 0,
        };
        if name == WORKER {
            let r = self.rings.read(&req);
            return (!r.missing).then_some(r);
        }
        let path = (self.files)(env, name)?;
        let mut f = File::open(&path).ok()?;
        if let Some(old) = &s.file
            && !same_file(old, &f)
        {
            req.offset = 0;
        }
        let r = read_open(&mut f, &req).ok();
        s.file = Some(f);
        r
    }

    fn follow(&self, env: &str, e: &mut Combined, name: &'static str) {
        let s = e.sources.get_mut(name).expect("every source");
        let Some(r) = self.read(env, s, name) else {
            return;
        };
        let mut out = Vec::new();
        if r.start != s.next {
            // The log started over -- the machine booted again -- or lost
            // its oldest before it was read: what was waiting for an end
            // has none.
            s.flush(&mut out, name);
        }
        s.next = r.start + r.data.len() as i64;
        let mut data = r.data;
        if !s.partial.is_empty() && !data.is_empty() {
            let mut joined = std::mem::take(&mut s.partial);
            joined.extend_from_slice(&data);
            data = joined;
        }
        let mut rest = &data[..];
        while let Some(i) = rest.iter().position(|&b| b == b'\n') {
            mark(&mut out, name, &rest[..i]);
            rest = &rest[i + 1..];
        }
        if !rest.is_empty() {
            s.partial = rest.to_vec();
            s.since = Some(Instant::now());
        } else if !s.partial.is_empty() && s.since.is_some_and(|t| t.elapsed() >= PARTIAL_AFTER) {
            s.flush(&mut out, name);
        }
        if !out.is_empty() {
            e.ring.write(&out);
        }
    }

    /// Reads an environment's combined log.
    pub fn read_combined(&self, req: &Request) -> Reply {
        let e = self.envs.lock().unwrap().get(&req.environment).cloned();
        match e {
            Some(e) => e.lock().unwrap().ring.read(req),
            None => Reply::missing(),
        }
    }
}

impl Source {
    fn flush(&mut self, out: &mut Vec<u8>, name: &str) {
        if !self.partial.is_empty() {
            let p = std::mem::take(&mut self.partial);
            mark(out, name, &p);
        }
    }
}

/// The widest source's name, which every line's mark is padded to.
const WIDTH: usize = 7;

/// Writes one line of a source, marked with its name.
fn mark(out: &mut Vec<u8>, name: &str, line: &[u8]) {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    out.extend_from_slice(format!("{name:<WIDTH$} │ ").as_bytes());
    out.extend_from_slice(line);
    out.push(b'\n');
}

#[cfg(unix)]
fn same_file(a: &File, b: &File) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (a.metadata(), b.metadata()) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_file(_: &File, _: &File) -> bool {
    true
}

/// The worker's logger: each line to standard error, and kept in rings,
/// whole and, when it has an `environment=` field, as that environment's.
/// Lines from Hangar's own code are kept from `level` up; other crates'
/// only from warnings up.
pub struct Logger {
    pub rings: Arc<Rings>,
    pub level: log::LevelFilter,
}

impl Logger {
    /// Installs the logger for the whole process.
    pub fn install(rings: Arc<Rings>, level: log::LevelFilter) {
        let l = Box::new(Logger { rings, level });
        if log::set_boxed_logger(l).is_ok() {
            log::set_max_level(level.max(log::LevelFilter::Warn));
        }
    }

    fn wants(&self, m: &log::Metadata) -> bool {
        if m.target().starts_with("hangar") {
            m.level() <= self.level
        } else {
            m.level() <= log::Level::Warn
        }
    }
}

impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        self.wants(m)
    }

    fn log(&self, r: &log::Record) {
        if !self.wants(r.metadata()) {
            return;
        }
        let text = r.args().to_string();
        let level = match r.level() {
            log::Level::Error => "ERROR",
            log::Level::Warn => "WARN",
            log::Level::Info => "INFO",
            log::Level::Debug | log::Level::Trace => "DEBUG",
        };
        let time = hangar_api::rfc3339(std::time::SystemTime::now());
        let line = if r.target().starts_with("hangar") {
            format!("time={time} level={level} {text}\n")
        } else {
            format!(
                "time={time} level={level} msg={text:?} component={}\n",
                r.target()
            )
        };
        eprint!("{line}");
        self.rings.write(environment(&text), line.as_bytes());
    }

    fn flush(&self) {}
}

/// The environment a line's `environment=` field names, or "".
pub fn environment(line: &str) -> &str {
    let Some(i) = line.find(" environment=") else {
        return "";
    };
    let v = &line[i + " environment=".len()..];
    v.split([' ', '\n']).next().unwrap_or("").trim_matches('"')
}
