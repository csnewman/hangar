use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::{Map, Value, json};
use tokio::net::UnixStream;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use super::docker::{Action, DockerListening};
use super::docs::{Resume, SharedDoc};
use super::error::{Error, Result};
use super::fileops::{Clash, Encoding, Separator};
use super::files::{Listening, Run, writable};
use super::frame;
use super::git::Named;
use super::hub::{Hub, Tab};
use super::lsp;
use super::paths::resolve;
use super::repos::{At, ReposListening};
use super::search::TextQuery;
use super::{Folder, Services};

/// A tab's language servers, by its number for each: the hub, and the
/// tab's number there.
type Servers = Arc<Mutex<HashMap<u32, (Arc<Hub>, u32)>>>;

/// One browser tab's Code view: its requests, and the events it is sent. It
/// works in the service's folder unless its hello names another, for that
/// tab alone.
pub struct Connection {
    s: Arc<Services>,
    out: mpsc::UnboundedSender<Vec<u8>>,
    /// Which tab this is, among everyone's.
    id: u64,
    state: Mutex<State>,
    /// The container logs the tab follows, by its number for each.
    logs: Arc<Mutex<HashMap<u32, JoinHandle<()>>>>,
    /// The language servers the tab uses, by its number for each: the hub,
    /// and the tab's number there.
    servers: Servers,
}

struct State {
    folder: Arc<Folder>,
    /// The shared documents the tab has open, by its number for each.
    docs: HashMap<u32, Arc<SharedDoc>>,
    next_doc: u32,
    next_log: u32,
    next_server: u32,
    /// Docker, watched once the tab asks of it.
    docker: Option<(JoinHandle<()>, DockerListening)>,
    /// Forwards the folder's file and git changes to the tab.
    watching: Option<Watching>,
}

struct Watching {
    files: (JoinHandle<()>, Listening),
    git: (JoinHandle<()>, ReposListening),
}

impl Drop for Watching {
    fn drop(&mut self) {
        self.files.0.abort();
        self.git.0.abort();
    }
}

pub async fn run(stream: UnixStream, s: Arc<Services>) {
    let (mut rd, mut wr) = stream.into_split();
    let (out, mut outgoing) = mpsc::unbounded_channel::<Vec<u8>>();
    let writer = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        while let Some(f) = outgoing.recv().await {
            if wr.write_all(&f).await.is_err() {
                break;
            }
        }
    });
    let folder = s.folder(&s.root);
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let conn = Arc::new(Connection {
        s: s.clone(),
        out: out.clone(),
        id: NEXT.fetch_add(1, Ordering::Relaxed),
        logs: Arc::new(Mutex::new(HashMap::new())),
        servers: Arc::new(Mutex::new(HashMap::new())),
        state: Mutex::new(State {
            folder,
            docs: HashMap::new(),
            next_doc: 1,
            next_log: 1,
            next_server: 1,
            docker: None,
            watching: None,
        }),
    });
    conn.watch();
    let forwards = [
        forward(
            s.settings.subscribe(),
            out.clone(),
            "settings.changed",
            |v| v,
        ),
        forward(
            s.workspace.subscribe(),
            out.clone(),
            "workspace.changed",
            to_value,
        ),
    ];

    while let Some(f) = frame::read(&mut rd).await {
        if f.first() == Some(&frame::JSON) {
            let Ok(req) = serde_json::from_slice::<Request>(&f[1..]) else {
                continue;
            };
            let conn = conn.clone();
            tokio::spawn(async move { conn.handle(req).await });
        } else if f.first() == Some(&frame::LSP) && f.len() >= 5 {
            let n = u32::from_be_bytes([f[1], f[2], f[3], f[4]]);
            let server = conn.servers.lock().unwrap().get(&n).cloned();
            // Not JSON: nothing to pass on.
            if let (Some((hub, tab)), Ok(m)) = (server, serde_json::from_slice::<Value>(&f[5..])) {
                hub.tab_message(tab, m);
            }
        }
    }

    for t in forwards {
        t.abort();
    }
    let open: Vec<_> = {
        let mut st = conn.state.lock().unwrap();
        st.watching = None;
        if let Some((t, _)) = st.docker.take() {
            t.abort();
        }
        st.docs.drain().collect()
    };
    for (n, doc) in open {
        doc.leave(conn.id, n);
    }
    for (_, t) in conn.logs.lock().unwrap().drain() {
        t.abort();
    }
    conn.stop_servers();
    drop(conn);
    drop(out);
    let _ = writer.await;
}

#[derive(serde::Deserialize)]
struct Request {
    id: Value,
    method: String,
    #[serde(default)]
    params: Option<Map<String, Value>>,
}

/// Sends each broadcast value to the tab as an event, until the tab goes.
fn forward<T: Clone + Send + 'static>(
    mut rx: broadcast::Receiver<T>,
    out: mpsc::UnboundedSender<Vec<u8>>,
    event: &'static str,
    params: fn(T) -> Value,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(v) => {
                    let ev = json!({ "event": event, "params": params(v) });
                    if send_json(&out, &ev).is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    })
}

fn send_json(out: &mpsc::UnboundedSender<Vec<u8>>, v: &Value) -> std::result::Result<(), ()> {
    let body = serde_json::to_vec(v).map_err(|_| ())?;
    out.send(frame::encode(frame::JSON, &[&body]))
        .map_err(|_| ())
}

/// A request's parameters, read as the browser sends them.
struct Params<'a>(&'a Map<String, Value>);

impl Params<'_> {
    fn str(&self, k: &str) -> Result<&str> {
        self.0
            .get(k)
            .and_then(Value::as_str)
            .ok_or_else(|| Error::msg(format!("{k} is required")))
    }

    /// A string given and not empty.
    fn opt(&self, k: &str) -> Option<&str> {
        self.0
            .get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    }

    fn flag(&self, k: &str) -> bool {
        self.0.get(k) == Some(&Value::Bool(true))
    }

    fn strs(&self, k: &str) -> Result<Vec<String>> {
        match self.0.get(k) {
            Some(Value::Array(v)) if v.iter().all(Value::is_string) => Ok(v
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()),
            _ => Err(Error::msg(format!("{k} is required"))),
        }
    }

    fn text_query(&self) -> Result<TextQuery> {
        let Some(query) = self.0.get("query").and_then(Value::as_str) else {
            return Err(Error::msg("query is required"));
        };
        Ok(TextQuery {
            query: query.to_owned(),
            regex: self.flag("regex"),
            case_sensitive: self.flag("caseSensitive"),
            whole_word: self.flag("wholeWord"),
            mask: self
                .0
                .get("mask")
                .and_then(Value::as_str)
                .map(str::to_owned),
            dir: self.opt("dir").map(str::to_owned),
        })
    }

    fn num(&self, k: &str) -> Option<f64> {
        self.0.get(k).and_then(Value::as_f64)
    }

    fn get(&self, k: &str) -> Option<&Value> {
        self.0.get(k)
    }
}

impl Connection {
    async fn handle(self: Arc<Self>, req: Request) {
        let params = req.params.unwrap_or_default();
        let reply = match self.call(&req.method, Params(&params)).await {
            Ok(result) => json!({ "id": req.id, "result": result }),
            Err(e) => json!({ "id": req.id, "error": e.to_string() }),
        };
        let _ = send_json(&self.out, &reply);
    }

    fn folder(&self) -> Arc<Folder> {
        self.state.lock().unwrap().folder.clone()
    }

    /// Announces every change under the tab's folder, and every change to
    /// what git reports; the browser decides what to reload.
    fn watch(&self) {
        let folder = self.folder();
        let (mut rx, listening) = folder.files.listen();
        let out = self.out.clone();
        let files = tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok((dirs, _)) => {
                        let ev = json!({ "event": "fs.changed", "params": { "dirs": dirs } });
                        if send_json(&out, &ev).is_err() {
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        });
        let (rx, git_listening) = folder.repos.listen();
        let git = forward(rx, self.out.clone(), "git.changed", |()| json!({}));
        self.state.lock().unwrap().watching = Some(Watching {
            files: (files, listening),
            git: (git, git_listening),
        });
    }

    /// Joins the tab to its folder's language server for a language,
    /// starting it if no tab has, its messages relayed on the LSP channel
    /// under the number returned. A language with no server installed is told
    /// why.
    async fn start_server(&self, language: &str) -> Result<Value> {
        let f = self.folder();
        let existing = f.hubs.lock().unwrap().get(language).cloned();
        let hub = match existing {
            Some(h) => h,
            None => {
                let settings = self.s.settings.get().await;
                let (folder, lang) = (Arc::downgrade(&f), language.to_owned());
                let started = Hub::start(
                    &f.root,
                    language,
                    &settings,
                    self.s.docs.clone(),
                    &f.files,
                    move |_| {
                        if let Some(f) = folder.upgrade() {
                            let mut hubs = f.hubs.lock().unwrap();
                            if hubs.get(&lang).is_some_and(|h| h.stopped()) {
                                hubs.remove(&lang);
                            }
                        }
                    },
                );
                let h = match started {
                    Ok(h) => h,
                    Err(missing) => return Ok(missing),
                };
                // Another request may have started one meanwhile.
                let raced = {
                    let mut hubs = f.hubs.lock().unwrap();
                    match hubs.get(language).cloned() {
                        Some(r) => Some(r),
                        None => {
                            hubs.insert(language.to_owned(), h.clone());
                            None
                        }
                    }
                };
                match raced {
                    Some(r) => {
                        h.stop("started twice");
                        r
                    }
                    None => h,
                }
            }
        };
        let n = {
            let mut st = self.state.lock().unwrap();
            st.next_server += 1;
            st.next_server - 1
        };
        let (out, out2, servers) = (self.out.clone(), self.out.clone(), self.servers.clone());
        let tab = hub.attach(Tab {
            send: Box::new(move |m: Value| {
                let mut with = Map::new();
                with.insert("jsonrpc".into(), json!("2.0"));
                if let Value::Object(o) = m {
                    with.extend(o);
                }
                let body = serde_json::to_vec(&Value::Object(with)).unwrap_or_default();
                let _ = out.send(frame::encode(frame::LSP, &[&n.to_be_bytes(), &body]));
            }),
            gone: Box::new(move |why: String| {
                if servers.lock().unwrap().remove(&n).is_some() {
                    let _ = send_json(
                        &out2,
                        &json!({ "event": "lsp.exited", "params": { "server": n, "why": why } }),
                    );
                }
            }),
        });
        self.servers.lock().unwrap().insert(n, (hub.clone(), tab));
        Ok(json!({ "server": n, "root": f.root.display().to_string(), "command": hub.command }))
    }

    /// Leaves the tab's servers, which stay for others, and for a while after
    /// the last leaves.
    fn stop_servers(&self) {
        let servers: Vec<(Arc<Hub>, u32)> = self
            .servers
            .lock()
            .unwrap()
            .drain()
            .map(|(_, s)| s)
            .collect();
        for (hub, tab) in servers {
            hub.detach(tab);
        }
    }

    async fn open_doc(&self, path: &str, resume: Option<Resume>) -> Result<Value> {
        let abs = resolve(&self.folder().root, Path::new(path));
        let doc = self.s.docs.open(&abs.display().to_string()).await?;
        let n = {
            let mut st = self.state.lock().unwrap();
            let n = st.next_doc;
            st.next_doc += 1;
            st.docs.insert(n, doc.clone());
            n
        };
        let abs = abs.display().to_string();
        let joined = doc.join(self.id, n, &abs, self.out.clone(), resume);
        let mut v = json!({
            "doc": n,
            "path": abs,
            // A file the user may not write -- a library's, the system's --
            // is shown, not edited.
            "writable": writable(&doc.path),
            "epoch": joined.epoch,
            "version": joined.version,
            "client": joined.client,
            "presence": joined.presence,
        });
        if let Some(text) = joined.text {
            v["text"] = text.into();
        }
        if let Some(updates) = joined.updates {
            v["updates"] = updates;
        }
        Ok(v)
    }

    /// The open document a request names.
    fn doc(&self, p: &Params<'_>) -> Result<(u32, Arc<SharedDoc>)> {
        let n = p.num("doc").unwrap_or(0.0) as u32;
        let doc = self.state.lock().unwrap().docs.get(&n).cloned();
        doc.map(|d| (n, d))
            .ok_or_else(|| Error::msg("the document is not open"))
    }

    /// The repository a path is in, the path made absolute, and where that
    /// repository's paths start in the folder.
    async fn in_repo(&self, path: &str) -> At {
        self.folder().repos.at(path, false).await
    }

    /// A whole repository, named by a path in it: the folder's own when none
    /// is given.
    async fn of_repo(&self, path: Option<&str>) -> At {
        self.folder().repos.at(path.unwrap_or(""), true).await
    }

    /// A folder's entries, with which of them git ignores.
    async fn list(&self, path: &str) -> Result<Value> {
        let f = self.folder();
        let mut entries = f.files.list(path).await?;
        let dir = f.files.abs(path);
        let abs: Vec<_> = entries.iter().map(|e| dir.join(&e.name)).collect();
        let ignored = self
            .of_repo(Some(&dir.display().to_string()))
            .await
            .git
            .ignored(&abs)
            .await;
        for (e, a) in entries.iter_mut().zip(&abs) {
            e.ignored = ignored.contains(a);
        }
        Ok(to_value(entries))
    }

    /// Starts a connection in a folder: the one asked for, which must be an
    /// absolute path to a directory, or the service's own.
    async fn hello(&self, root: Option<&Value>) -> Result<Value> {
        let mut want = self.s.root.clone();
        if let Some(root) = root.and_then(Value::as_str).filter(|r| !r.is_empty()) {
            if !root.starts_with('/') {
                return Err(Error::msg(format!("{root} is not an absolute path")));
            }
            want = resolve(Path::new("/"), Path::new(root));
            match tokio::fs::metadata(&want).await {
                Err(_) => return Err(Error::msg(format!("{} does not exist", want.display()))),
                Ok(m) if !m.is_dir() => {
                    return Err(Error::msg(format!("{} is not a folder", want.display())));
                }
                Ok(_) => {}
            }
        }
        if want != self.folder().root {
            self.state.lock().unwrap().folder = self.s.folder(&want);
            self.watch();
            // A server knows one folder.
            self.stop_servers();
        }
        Ok(json!({
            "root": self.folder().root.display().to_string(),
            "home": self.s.root.display().to_string(),
            "settings": self.s.settings.path().display().to_string(),
            // Shared documents are edited through one authority, as
            // @codemirror/collab has them, not as Yjs documents.
            "docs": "collab",
        }))
    }

    async fn call(&self, method: &str, p: Params<'_>) -> Result<Value> {
        let f = self.folder();
        let files = &f.files;
        Ok(match method {
            "hello" => self.hello(p.get("root")).await?,
            // Answered at once, so the browser can time the way here and back.
            "ping" => Value::Null,
            "fs.list" => self.list(p.str("path")?).await?,
            "fs.read" => json!({ "text": files.read(p.str("path")?).await? }),
            "fs.patch" => {
                let runs = match p.get("runs") {
                    Some(Value::Array(runs)) => runs
                        .iter()
                        .map(|r| Run {
                            offset: r.get("offset").and_then(Value::as_f64).unwrap_or(f64::NAN),
                            data: r
                                .get("data")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                let version = files.patch(p.str("path")?, p.str("version")?, runs).await?;
                json!({ "version": version })
            }
            "fs.probe" => to_value(files.probe(p.str("path")?).await?),
            "fs.bytes" => to_value(
                files
                    .bytes(
                        p.str("path")?,
                        p.num("offset").unwrap_or(0.0),
                        p.num("length").unwrap_or(65536.0),
                    )
                    .await?,
            ),
            "fs.create" => {
                files.create(p.str("path")?).await?;
                Value::Null
            }
            "fs.mkdir" => {
                files.mkdir(p.str("path")?).await?;
                Value::Null
            }
            "fs.rename" => {
                files.rename(p.str("from")?, p.str("to")?).await?;
                Value::Null
            }
            "fs.delete" => {
                files.remove(p.str("path")?).await?;
                Value::Null
            }
            "fs.transfer" => {
                let op = p.str("op")?;
                let clash_name = p.str("clash")?;
                let move_ = match op {
                    "copy" => false,
                    "move" => true,
                    _ => return Err(Error::msg(format!("no such operation: {op}"))),
                };
                let Some(clash) = Clash::parse(clash_name) else {
                    return Err(Error::msg(format!("no such choice: {clash_name}")));
                };
                let name = p.opt("name").map(str::to_owned);
                to_value(
                    files
                        .transfer(move_, &p.strs("from")?, p.str("to")?, clash, name)
                        .await?,
                )
            }
            "fs.trash" => to_value(files.trash(&p.strs("paths")?).await?),
            "fs.restore" => {
                let items = match p.get("items") {
                    Some(Value::Array(items)) => items
                        .iter()
                        .map(|t| {
                            let s = |k: &str| match t.get(k) {
                                Some(Value::String(s)) => s.clone(),
                                Some(v) => v.to_string(),
                                None => "undefined".into(),
                            };
                            (s("path"), s("id"))
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                files.restore(items).await?;
                Value::Null
            }
            "fs.uploadStart" => Value::String(files.upload_start(p.str("path")?)?),
            "fs.uploadWrite" => {
                files.upload_write(p.str("token")?, p.str("data")?)?;
                Value::Null
            }
            "fs.uploadEnd" => Value::String(files.upload_end(p.str("token")?, p.flag("replace"))?),
            "fs.uploadAbort" => {
                files.upload_abort(p.str("token")?);
                Value::Null
            }
            "fs.zip" => to_value(files.zip(&p.strs("paths")?).await?),
            "fs.unzip" => {
                files.unzip(p.str("path")?).await;
                Value::Null
            }
            "fs.setMode" => {
                let b = |k: &str| p.get(k).and_then(Value::as_bool);
                json!(
                    files
                        .set_mode(p.str("path")?, b("exec"), b("writable"))
                        .await?
                )
            }
            "fs.textProps" => to_value(files.text_props(p.str("path")?).await?),
            "fs.convert" => {
                let separator = match p.opt("separator") {
                    Some(s) => Some(
                        Separator::parse(s)
                            .ok_or_else(|| Error::msg(format!("no such separator: {s}")))?,
                    ),
                    None => None,
                };
                let encoding = match p.opt("encoding") {
                    Some(e) => Some(
                        Encoding::parse(e)
                            .ok_or_else(|| Error::msg(format!("no such encoding: {e}")))?,
                    ),
                    None => None,
                };
                let bom = p.get("bom").and_then(Value::as_bool);
                to_value(
                    files
                        .convert(p.str("path")?, separator, encoding, bom)
                        .await?,
                )
            }
            "lsp.start" => self.start_server(p.str("language")?).await?,
            "lsp.stop" => {
                let n = p.num("server").unwrap_or(0.0) as u32;
                let server = self.servers.lock().unwrap().remove(&n);
                if let Some((hub, tab)) = server {
                    hub.detach(tab);
                }
                Value::Null
            }
            "lsp.info" => {
                let language = p.str("language")?;
                let hub = f.hubs.lock().unwrap().get(language).cloned();
                match hub {
                    Some(h) => with(json!({ "running": true }), h.info()),
                    None => match lsp::find(language, &self.s.settings.get().await) {
                        Err(missing) => with(json!({ "running": false }), missing),
                        Ok((spec, path)) => json!({
                            "running": false,
                            "command": spec.command,
                            "path": path.display().to_string(),
                        }),
                    },
                }
            }
            // Every tab using it is told it stopped, and starts it again.
            "lsp.restart" => {
                let hub = f.hubs.lock().unwrap().get(p.str("language")?).cloned();
                if let Some(h) = hub {
                    h.stop("restarted");
                }
                Value::Null
            }
            "docker.overview" => {
                let watching = self.state.lock().unwrap().docker.is_some();
                if !watching {
                    let (rx, listening) = self.s.docker.listen();
                    let t = forward(rx, self.out.clone(), "docker.changed", |()| json!({}));
                    self.state.lock().unwrap().docker = Some((t, listening));
                }
                to_value(self.s.docker.overview().await)
            }
            "docker.act" => {
                let action = p.str("action")?;
                let Some(a) = Action::parse(action) else {
                    return Err(Error::msg(format!("no such action: {action}")));
                };
                self.s.docker.act(p.str("id")?, &a).await?;
                Value::Null
            }
            "docker.project" => {
                let action = p.str("action")?;
                if !["start", "stop", "restart", "down"].contains(&action) {
                    return Err(Error::msg(format!("no such action: {action}")));
                }
                self.s.docker.project(p.str("project")?, action).await?;
                Value::Null
            }
            "docker.removeImage" => {
                self.s.docker.remove_image(p.str("id")?).await?;
                Value::Null
            }
            "docker.logs" => {
                let n = {
                    let mut st = self.state.lock().unwrap();
                    st.next_log += 1;
                    st.next_log - 1
                };
                let (out, logs) = (self.out.clone(), self.logs.clone());
                let out2 = out.clone();
                let t = self.s.docker.logs(
                    p.str("id")?.to_owned(),
                    p.num("tail").unwrap_or(1000.0),
                    move |text| {
                        let _ = send_json(&out, &json!({ "event": "docker.log", "params": { "stream": n, "text": text } }));
                    },
                    move |why| {
                        if logs.lock().unwrap().remove(&n).is_some() {
                            let _ = send_json(&out2, &json!({ "event": "docker.logEnd", "params": { "stream": n, "why": why } }));
                        }
                    },
                );
                self.logs.lock().unwrap().insert(n, t);
                json!({ "stream": n })
            }
            "docker.logsStop" => {
                let n = p.num("stream").unwrap_or(0.0) as u32;
                if let Some(t) = self.logs.lock().unwrap().remove(&n) {
                    t.abort();
                }
                Value::Null
            }
            "preview.start" => {
                let (port, secret) = self.s.preview.start().await?;
                json!({ "port": port, "secret": secret })
            }
            "workspace.get" => to_value(self.s.workspace.get().await),
            "workspace.set" => to_value(
                self.s
                    .workspace
                    .set(p.get("folders").unwrap_or(&Value::Null))
                    .await?,
            ),
            "workspace.repos" => to_value(self.s.workspace.repositories(&self.s.home).await),
            "settings.get" => Value::Object(self.s.settings.get().await),
            "settings.set" => {
                self.s
                    .settings
                    .set(p.get("settings").unwrap_or(&Value::Null))
                    .await?;
                Value::Null
            }
            "doc.open" => {
                let resume = match (p.opt("epoch"), p.opt("client"), p.num("since")) {
                    (Some(epoch), Some(client), Some(since)) if since >= 0.0 => Some(Resume {
                        epoch: epoch.to_owned(),
                        client: client.to_owned(),
                        since: since as u64,
                    }),
                    _ => None,
                };
                self.open_doc(p.str("path")?, resume).await?
            }
            "doc.close" => {
                let n = p.num("doc").unwrap_or(0.0) as u32;
                let doc = self.state.lock().unwrap().docs.remove(&n);
                if let Some(doc) = doc {
                    doc.leave(self.id, n);
                }
                Value::Null
            }
            "doc.push" => {
                let (n, doc) = self.doc(&p)?;
                let changes = match p.get("changes") {
                    Some(Value::Array(c)) => c.clone(),
                    _ => return Err(Error::msg("changes is required")),
                };
                doc.push(
                    self.id,
                    n,
                    p.num("version").unwrap_or(-1.0).max(0.0) as u64,
                    &changes,
                )?;
                Value::Null
            }
            "doc.presence" => {
                let (n, doc) = self.doc(&p)?;
                let state = p.get("state").cloned().unwrap_or(Value::Null);
                doc.presence(self.id, n, p.num("version").unwrap_or(0.0) as u64, state);
                Value::Null
            }
            "search.files" => to_value(f.search.files().await),
            "search.text" => to_value(f.search.text(&p.text_query()?).await?),
            "search.replace" => {
                let only = match p.get("paths") {
                    Some(Value::Array(_)) => Some(p.strs("paths")?),
                    _ => None,
                };
                let q = p.text_query()?;
                to_value(f.search.replace(&q, p.str("replacement")?, only).await?)
            }
            "git.status" => to_value(f.repos.status().await),
            // These act on a file or folder, in the repository it is in.
            "git.diff" => {
                let r = self.in_repo(p.str("path")?).await;
                let from = p
                    .opt("from")
                    .map(|from| files.abs(from).display().to_string());
                let against = p.opt("against").unwrap_or("HEAD");
                let d = r
                    .git
                    .diff(&abs_str(&r), against, from.as_deref(), p.opt("to"))
                    .await?;
                named_value(&r.base, to_value(d))
            }
            "git.log" => {
                let skip = p.num("skip").unwrap_or(0.0);
                let max = p.num("max").unwrap_or(300.0);
                match p.opt("path") {
                    Some(path) => {
                        let r = self.in_repo(path).await;
                        to_value(
                            r.git
                                .log(p.opt("ref"), skip, max, Some(&abs_str(&r)))
                                .await?,
                        )
                    }
                    None => {
                        let r = self.of_repo(p.opt("repo")).await;
                        to_value(r.git.log(p.opt("ref"), skip, max, None).await?)
                    }
                }
            }
            "git.ignore" => {
                let r = self.in_repo(p.str("path")?).await;
                r.git.ignore(&abs_str(&r), p.flag("dir")).await?;
                Value::Null
            }
            "git.blob" => {
                let r = self.in_repo(p.str("path")?).await;
                r.git.blob(p.str("rev")?, &abs_str(&r)).await?
            }
            "git.blame" => {
                let r = self.in_repo(p.str("path")?).await;
                to_value(r.git.blame(&abs_str(&r)).await?)
            }
            "git.stage" => {
                let r = self.in_repo(p.str("path")?).await;
                r.git.stage(&abs_str(&r)).await?;
                Value::Null
            }
            "git.unstage" => {
                let r = self.in_repo(p.str("path")?).await;
                r.git.unstage(&abs_str(&r)).await?;
                Value::Null
            }
            "git.rollback" => {
                let r = self.in_repo(p.str("path")?).await;
                r.git.rollback(&abs_str(&r)).await?;
                Value::Null
            }
            "git.show" => {
                let r = self.in_repo(p.str("path")?).await;
                let head = p.get("from") == Some(&Value::String("HEAD".into()));
                json!({ "text": r.git.show(&abs_str(&r), head).await })
            }
            "git.setIndex" => {
                let r = self.in_repo(p.str("path")?).await;
                r.git.set_index(&abs_str(&r), p.str("content")?).await?;
                Value::Null
            }
            "git.initSubmodule" => {
                let r = self.in_repo(p.str("path")?).await;
                r.git.init_submodule(&abs_str(&r)).await?;
                Value::Null
            }
            // The rest act on a whole repository: repo is a path in it, the
            // folder's own repository when none is given.
            "git.commitInfo" => {
                let r = self.of_repo(p.opt("repo")).await;
                let mut info = r.git.commit_info(p.str("hash")?).await?;
                info.files = named_all(&r.base, info.files);
                to_value(info)
            }
            "git.branchChanges" => {
                let r = self.of_repo(p.opt("repo")).await;
                let base = p.get("base").and_then(Value::as_str);
                let mut c = r.git.branch_changes(base).await?;
                c.changes = named_all(&r.base, c.changes);
                to_value(c)
            }
            "git.commit" => {
                let r = self.of_repo(p.opt("repo")).await;
                r.git.commit(p.str("message")?, p.flag("amend")).await?;
                Value::Null
            }
            "git.lastMessage" => {
                Value::String(self.of_repo(p.opt("repo")).await.git.last_message().await)
            }
            "git.branches" => to_value(self.of_repo(p.opt("repo")).await.git.branches().await?),
            "git.checkout" => {
                let r = self.of_repo(p.opt("repo")).await;
                r.git.checkout(p.str("name")?).await?;
                Value::Null
            }
            "git.createBranch" => {
                let r = self.of_repo(p.opt("repo")).await;
                r.git.create_branch(p.str("name")?).await?;
                Value::Null
            }
            "git.push" => {
                self.of_repo(p.opt("repo")).await.git.push().await?;
                Value::Null
            }
            "git.pull" => {
                self.of_repo(p.opt("repo")).await.git.pull().await?;
                Value::Null
            }
            "git.fetch" => {
                self.of_repo(p.opt("repo")).await.git.fetch().await?;
                Value::Null
            }
            _ => return Err(Error::msg(format!("no such method: {method}"))),
        })
    }
}

fn to_value<T: Serialize>(v: T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

/// `extra`'s fields after `first`'s.
fn with(mut first: Value, extra: Value) -> Value {
    if let (Value::Object(a), Value::Object(b)) = (&mut first, extra) {
        a.extend(b);
    }
    first
}

fn abs_str(r: &At) -> String {
    r.abs.display().to_string()
}

/// A repository's paths in what it returns, as the folder names them.
fn named_value(base: &str, mut v: Value) -> Value {
    if !base.is_empty() {
        for k in ["path", "from"] {
            if let Some(Value::String(s)) = v.get_mut(k) {
                *s = format!("{base}{s}");
            }
        }
    }
    v
}

fn named_all(base: &str, files: Vec<Named>) -> Vec<Named> {
    files
        .into_iter()
        .map(|mut f| {
            if !base.is_empty() {
                f.path = format!("{base}{}", f.path);
                f.from = f.from.map(|from| format!("{base}{from}"));
            }
            f
        })
        .collect()
}
