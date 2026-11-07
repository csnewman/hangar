//! One language server for one folder, shared by every tab that has files
//! of its language open there.
//!
//! A language server serves one client, so the hub is that client, and the
//! tabs are its users: each tab's requests are passed on under ids of the
//! hub's own and their answers passed back; what the server announces goes
//! to every tab. Documents are the hub's to keep the server told of: they are
//! the shared documents everyone edits, so the server is sent their text as
//! they change, once, whichever tab changed them, and the tabs' own document
//! notifications are dropped. The server's answer to initialize is kept and
//! given to each tab that asks, and the last diagnostics for each file given
//! to each tab that opens it.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use super::docs::{Documents, SharedDoc};
use super::files::{Files, Listening};
use super::lsp::{self, Server, now_ms, url_path};

/// How long a server is kept once no tab uses it, so a tab reloaded, or
/// opened again soon, finds it warm.
const KEEP: Duration = Duration::from_secs(5 * 60);
/// How soon after an edit the server is sent the document's text.
const SYNC_AFTER: Duration = Duration::from_millis(50);
/// How many messages the trace keeps.
const TRACE: usize = 300;

/// One browser tab's use of a hub: what it is sent, and being told the
/// server has gone.
pub struct Tab {
    pub send: Box<dyn Fn(Value) + Send + Sync>,
    pub gone: Box<dyn Fn(String) + Send + Sync>,
}

struct OpenDoc {
    doc: Arc<SharedDoc>,
    language: String,
    version: i64,
    tabs: HashSet<u32>,
    dirty: bool,
    /// Bumped by each change, so only the last of a run is sent.
    pulse: u64,
    follow: JoinHandle<()>,
}

#[derive(Default)]
struct Inner {
    tabs: BTreeMap<u32, Arc<Tab>>,
    next_tab: u32,
    next_id: u64,
    /// Requests passed on, by the hub's id: whose, their id, what, and when.
    waiting: HashMap<String, (u32, Value, Option<String>, f64)>,
    trace: VecDeque<Value>,
    /// What the server logs as LSP messages (window/logMessage), the latest
    /// 64 KiB, beside what it writes to stderr.
    logged: String,
    server_info: Option<Value>,
    /// Requests from the server passed to a tab, by the hub's id.
    asked: HashMap<String, Value>,
    init_sent: bool,
    docs: HashMap<String, OpenDoc>,
    diagnostics: HashMap<String, Value>,
    idle: u64,
    stopped: bool,
}

/// What a hub tells as it stops.
type Gone = Box<dyn FnOnce(String) + Send>;

/// Messages to tabs, sent once the hub's lock is let go.
type Out = Vec<(Arc<Tab>, Value)>;

pub struct Hub {
    pub root: PathBuf,
    pub language: String,
    pub command: String,
    server: Server,
    documents: Arc<Documents>,
    inner: Mutex<Inner>,
    /// The server's answer to initialize, once it has one.
    init: watch::Sender<Option<Result<Value, String>>>,
    gone: Mutex<Option<Gone>>,
    files: Mutex<Option<(JoinHandle<()>, Listening)>>,
    me: Weak<Hub>,
}

impl Hub {
    /// Makes a hub for a folder's files of a language, or says why there is
    /// none. `gone` is told when the hub stops.
    pub fn start(
        root: &Path,
        language: &str,
        settings: &Map<String, Value>,
        documents: Arc<Documents>,
        files: &Arc<Files>,
        gone: impl FnOnce(String) + Send + 'static,
    ) -> Result<Arc<Hub>, Value> {
        let (spec, path) = lsp::find(language, settings)?;
        let (msgs, mut from) = mpsc::unbounded_channel::<Value>();
        let (exit, ended) = oneshot::channel::<String>();
        let server = Server::start(
            root,
            &spec,
            &path,
            move |m| {
                let _ = msgs.send(m);
            },
            move |why| {
                let _ = exit.send(why);
            },
        )
        .map_err(|e| json!({ "missing": format!("{} could not be started: {e}", spec.command), "install": [] }))?;
        let hub = Arc::new_cyclic(|me: &Weak<Hub>| Hub {
            root: root.to_path_buf(),
            language: language.to_owned(),
            command: spec.command.clone(),
            server,
            documents,
            inner: Mutex::new(Inner {
                next_tab: 1,
                next_id: 1,
                ..Default::default()
            }),
            init: watch::channel(None).0,
            gone: Mutex::new(Some(Box::new(gone))),
            files: Mutex::new(None),
            me: me.clone(),
        });
        let me = Arc::downgrade(&hub);
        tokio::spawn(async move {
            let mut ended = ended;
            loop {
                tokio::select! {
                    m = from.recv() => match (m, me.upgrade()) {
                        (Some(m), Some(h)) => h.server_message(m),
                        _ => return,
                    },
                    why = &mut ended => {
                        // What it said before it went is passed on first.
                        while let Ok(m) = from.try_recv() {
                            if let Some(h) = me.upgrade() { h.server_message(m); }
                        }
                        if let (Ok(why), Some(h)) = (why, me.upgrade()) {
                            h.exited(why);
                        }
                        return;
                    }
                }
            }
        });
        let (mut rx, listening) = files.listen();
        let me = Arc::downgrade(&hub);
        let task = tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok((_, changed)) => match me.upgrade() {
                        Some(h) => h.server.files_changed(&changed),
                        None => return,
                    },
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        });
        *hub.files.lock().unwrap() = Some((task, listening));
        Ok(hub)
    }

    pub fn stopped(&self) -> bool {
        self.inner.lock().unwrap().stopped
    }

    /// Adds a tab, returning its number.
    pub fn attach(&self, tab: Tab) -> u32 {
        let mut i = self.inner.lock().unwrap();
        i.idle += 1;
        let n = i.next_tab;
        i.next_tab += 1;
        i.tabs.insert(n, Arc::new(tab));
        n
    }

    pub fn detach(&self, n: u32) {
        let mut i = self.inner.lock().unwrap();
        if i.tabs.remove(&n).is_none() {
            return;
        }
        let gone: Vec<String> = i
            .docs
            .iter_mut()
            .filter_map(|(uri, d)| (d.tabs.remove(&n) && d.tabs.is_empty()).then(|| uri.clone()))
            .collect();
        for uri in gone {
            self.close_doc(&mut i, &uri, true);
        }
        i.waiting.retain(|_, w| w.0 != n);
        if i.tabs.is_empty() {
            i.idle += 1;
            let generation = i.idle;
            let me = self.me.clone();
            tokio::spawn(async move {
                tokio::time::sleep(KEEP).await;
                if let Some(h) = me.upgrade()
                    && h.inner.lock().unwrap().idle == generation
                {
                    h.stop("no tab used it");
                }
            });
        }
    }

    /// Ends the server at once, the service itself stopping.
    pub fn kill(&self) {
        self.server.kill();
    }

    pub fn info(&self) -> Value {
        let i = self.inner.lock().unwrap();
        let log: Vec<String> = [
            self.server.log().trim().to_owned(),
            i.logged.trim().to_owned(),
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect();
        let mut v = json!({
            "language": self.language,
            "command": self.command,
            "path": self.server.path.display().to_string(),
            "args": self.server.args,
            "started": self.server.started,
            "root": self.root.display().to_string(),
            "tabs": i.tabs.len(),
            "documents": i.docs.len(),
            "log": log.join("\n"),
            "trace": i.trace,
        });
        if let Some(pid) = self.server.pid {
            v["pid"] = json!(pid);
        }
        if let Some(si) = &i.server_info {
            v["serverInfo"] = si.clone();
        }
        v
    }

    fn note(
        i: &mut Inner,
        dir: &str,
        kind: &str,
        method: Option<&str>,
        id: Option<String>,
        ms: Option<f64>,
        error: Option<String>,
    ) {
        let mut t = json!({ "t": now_ms(), "dir": dir, "kind": kind });
        if let Some(m) = method {
            t["method"] = json!(m);
        }
        if let Some(id) = id {
            t["id"] = json!(id);
        }
        if let Some(ms) = ms {
            t["ms"] = json!(ms);
        }
        if let Some(e) = error {
            t["error"] = json!(e);
        }
        i.trace.push_back(t);
        while i.trace.len() > TRACE {
            i.trace.pop_front();
        }
    }

    /// Sends the server a message, noting it.
    fn to_server(&self, i: &mut Inner, m: Value) {
        let method = m.get("method").and_then(Value::as_str).map(str::to_owned);
        let id = m.get("id").filter(|v| !v.is_null()).map(id_text);
        match (&method, &id) {
            (Some(method), _) => {
                let kind = if id.is_some() {
                    "request"
                } else {
                    "notification"
                };
                Self::note(i, "to", kind, Some(method), id.clone(), None, None);
            }
            (None, Some(id)) => {
                let err = error_text(m.get("error"));
                Self::note(i, "to", "response", None, Some(id.clone()), None, err);
            }
            _ => {}
        }
        self.server.send(m);
    }

    pub fn stop(&self, why: &str) {
        let tabs: Vec<Arc<Tab>> = {
            let mut i = self.inner.lock().unwrap();
            if i.stopped {
                return;
            }
            i.stopped = true;
            i.idle += 1;
            let uris: Vec<String> = i.docs.keys().cloned().collect();
            for uri in uris {
                self.close_doc(&mut i, &uri, false);
            }
            i.tabs.values().cloned().collect()
        };
        if let Some((t, _)) = self.files.lock().unwrap().take() {
            t.abort();
        }
        self.server.stop();
        self.finish(why, tabs);
    }

    fn exited(&self, why: String) {
        let tabs: Vec<Arc<Tab>> = {
            let mut i = self.inner.lock().unwrap();
            if i.stopped {
                return;
            }
            i.stopped = true;
            i.idle += 1;
            for (_, d) in i.docs.drain() {
                d.follow.abort();
            }
            i.tabs.values().cloned().collect()
        };
        if let Some((t, _)) = self.files.lock().unwrap().take() {
            t.abort();
        }
        self.init.send_if_modified(|v| {
            if v.is_none() {
                *v = Some(Err(why.clone()));
                true
            } else {
                false
            }
        });
        self.finish(&why, tabs);
    }

    fn finish(&self, why: &str, tabs: Vec<Arc<Tab>>) {
        if let Some(gone) = self.gone.lock().unwrap().take() {
            gone(why.to_owned());
        }
        for t in tabs {
            (t.gone)(why.to_owned());
        }
    }

    pub fn tab_message(&self, n: u32, m: Value) {
        let mut out: Out = Vec::new();
        {
            let mut i = self.inner.lock().unwrap();
            let Some(tab) = i.tabs.get(&n).cloned() else {
                return;
            };
            let method = m.get("method").and_then(Value::as_str).map(str::to_owned);
            let id = m.get("id").filter(|v| !v.is_null()).cloned();
            match (method, id) {
                (Some(method), Some(id)) => self.request(&mut i, n, &tab, &method, id, m, &mut out),
                (Some(method), None) => self.notification(&mut i, n, &method, m),
                (None, Some(id)) => {
                    // A tab answering the server.
                    if let Some(sid) = i.asked.remove(&id_text(&id)) {
                        let mut a = json!({ "id": sid });
                        for k in ["result", "error"] {
                            if let Some(v) = m.get(k) {
                                a[k] = v.clone();
                            }
                        }
                        self.to_server(&mut i, a);
                    }
                }
                _ => {}
            }
        }
        deliver(out);
    }

    #[allow(clippy::too_many_arguments)]
    fn request(
        &self,
        i: &mut Inner,
        n: u32,
        tab: &Arc<Tab>,
        method: &str,
        id: Value,
        m: Value,
        out: &mut Out,
    ) {
        if method == "initialize" {
            if !i.init_sent {
                i.init_sent = true;
                let root = self.root.display().to_string();
                let mut params = m.get("params").cloned().unwrap_or_else(|| json!({}));
                if !params.is_object() {
                    params = json!({});
                }
                params["processId"] = json!(std::process::id());
                params["rootUri"] = json!(lsp::file_url(&root));
                params["rootPath"] = json!(root);
                let name = root
                    .rsplit('/')
                    .find(|s| !s.is_empty())
                    .unwrap_or(&root)
                    .to_owned();
                params["workspaceFolders"] = json!([{ "uri": lsp::file_url(&root), "name": name }]);
                self.to_server(
                    i,
                    json!({ "id": "hub-initialize", "method": "initialize", "params": params }),
                );
            }
            let mut rx = self.init.subscribe();
            let tab = tab.clone();
            tokio::spawn(async move {
                let Ok(r) = rx.wait_for(Option::is_some).await.map(|v| v.clone()) else {
                    return;
                };
                (tab.send)(match r {
                    Some(Ok(result)) => json!({ "id": id, "result": result }),
                    Some(Err(e)) => json!({ "id": id, "error": { "code": -32603, "message": e } }),
                    None => return,
                });
            });
            return;
        }
        if method == "shutdown" {
            // The server is everyone's; a tab leaving does not stop it.
            out.push((tab.clone(), json!({ "id": id, "result": Value::Null })));
            return;
        }
        // The server sees each document as it is now before it answers.
        self.flush_all(i);
        let hid = format!("{n}:{}", i.next_id);
        i.next_id += 1;
        i.waiting
            .insert(hid.clone(), (n, id, Some(method.to_owned()), now_ms()));
        let mut req = json!({ "id": hid, "method": method });
        if let Some(p) = m.get("params") {
            req["params"] = p.clone();
        }
        self.to_server(i, req);
    }

    fn notification(&self, i: &mut Inner, n: u32, method: &str, m: Value) {
        match method {
            "initialized" | "exit" | "textDocument/didChange" | "textDocument/willSave" => {}
            "textDocument/didOpen" => {
                let td = &m["params"]["textDocument"];
                let uri = td["uri"].as_str().map(str::to_owned);
                let language = td["languageId"].as_str().map(str::to_owned);
                if let Some(uri) = uri {
                    let me = self.me.clone();
                    tokio::spawn(async move {
                        if let Some(h) = me.upgrade() {
                            h.open_doc(n, uri, language).await;
                        }
                    });
                }
            }
            "textDocument/didClose" => {
                let uri = m["params"]["textDocument"]["uri"]
                    .as_str()
                    .unwrap_or("")
                    .to_owned();
                let empty = i
                    .docs
                    .get_mut(&uri)
                    .is_some_and(|d| d.tabs.remove(&n) && d.tabs.is_empty());
                if empty {
                    self.close_doc(i, &uri, true);
                }
            }
            "$/cancelRequest" => {
                let id = m["params"]["id"].clone();
                let hids: Vec<String> = i
                    .waiting
                    .iter()
                    .filter(|(_, w)| w.0 == n && w.1 == id)
                    .map(|(h, _)| h.clone())
                    .collect();
                for hid in hids {
                    self.to_server(
                        i,
                        json!({ "method": "$/cancelRequest", "params": { "id": hid } }),
                    );
                }
            }
            _ => {
                let mut fwd = json!({ "method": method });
                if let Some(p) = m.get("params") {
                    fwd["params"] = p.clone();
                }
                self.to_server(i, fwd);
            }
        }
    }

    fn server_message(&self, m: Value) {
        let mut out: Out = Vec::new();
        {
            let mut i = self.inner.lock().unwrap();
            let method = m.get("method").and_then(Value::as_str).map(str::to_owned);
            let id = m.get("id").filter(|v| !v.is_null()).cloned();
            if let Some(method) = &method {
                let kind = if id.is_some() {
                    "request"
                } else {
                    "notification"
                };
                Self::note(
                    &mut i,
                    "from",
                    kind,
                    Some(method),
                    id.as_ref().map(id_text),
                    None,
                    None,
                );
            }
            if method.as_deref() == Some("window/logMessage") {
                let kinds = ["", "error", "warning", "info", "log"];
                let kind = m["params"]["type"]
                    .as_u64()
                    .and_then(|t| kinds.get(t as usize).copied())
                    .filter(|k| !k.is_empty())
                    .unwrap_or("log");
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() % 86400)
                    .unwrap_or(0);
                let msg = m["params"]["message"].as_str().unwrap_or("");
                i.logged.push_str(&format!(
                    "[{:02}:{:02}:{:02} {kind}] {msg}\n",
                    now / 3600,
                    now / 60 % 60,
                    now % 60
                ));
                let over = i.logged.len().saturating_sub(64 << 10);
                if over > 0 {
                    let cut = (over..i.logged.len())
                        .find(|&c| i.logged.is_char_boundary(c))
                        .unwrap_or(i.logged.len());
                    i.logged.drain(..cut);
                }
            }
            match (method.as_deref(), id) {
                (Some(method), Some(sid)) => {
                    // A request the server's client answers, unless the hub
                    // does: registering for capabilities (file changes are
                    // sent regardless), its settings (none), and progress
                    // reports (not shown).
                    let answer = match method {
                        "client/registerCapability"
                        | "client/unregisterCapability"
                        | "window/workDoneProgress/create"
                        | "workspace/workspaceFolders" => Some(Value::Null),
                        "workspace/configuration" => Some(Value::Array(
                            m["params"]["items"]
                                .as_array()
                                .map(|items| items.iter().map(|_| Value::Null).collect())
                                .unwrap_or_default(),
                        )),
                        _ => None,
                    };
                    if let Some(result) = answer {
                        self.server.send(json!({ "id": sid, "result": result }));
                    } else if let Some((_, tab)) =
                        i.tabs.iter().next_back().map(|(k, t)| (*k, t.clone()))
                    {
                        // Put to a tab, the latest to attach.
                        let hid = format!("server:{}", i.next_id);
                        i.next_id += 1;
                        i.asked.insert(hid.clone(), sid);
                        let mut req = json!({ "id": hid, "method": method });
                        if let Some(p) = m.get("params") {
                            req["params"] = p.clone();
                        }
                        out.push((tab, req));
                    } else {
                        self.to_server(
                            &mut i,
                            json!({ "id": sid, "error": { "code": -32601, "message": "no client is attached" } }),
                        );
                    }
                }
                (Some(method), None) => {
                    if method == "textDocument/publishDiagnostics"
                        && let Some(uri) = m["params"]["uri"].as_str()
                    {
                        i.diagnostics.insert(uri.to_owned(), m.clone());
                    }
                    for t in i.tabs.values() {
                        out.push((t.clone(), m.clone()));
                    }
                }
                (None, Some(id)) if id == json!("hub-initialize") => {
                    let err = error_text(m.get("error"));
                    Self::note(
                        &mut i,
                        "from",
                        "response",
                        Some("initialize"),
                        Some("hub-initialize".into()),
                        None,
                        err.clone(),
                    );
                    i.server_info = m["result"].get("serverInfo").cloned();
                    match err {
                        Some(e) => {
                            self.init.send_replace(Some(Err(e)));
                        }
                        None => {
                            self.to_server(
                                &mut i,
                                json!({ "method": "initialized", "params": {} }),
                            );
                            self.init.send_replace(Some(Ok(m
                                .get("result")
                                .cloned()
                                .unwrap_or(Value::Null))));
                        }
                    }
                }
                (None, Some(id)) => {
                    if let Some((tab, tid, method, at)) = i.waiting.remove(&id_text(&id)) {
                        let err = error_text(m.get("error"));
                        Self::note(
                            &mut i,
                            "from",
                            "response",
                            method.as_deref(),
                            Some(id_text(&id)),
                            Some(now_ms() - at),
                            err,
                        );
                        if let Some(t) = i.tabs.get(&tab).cloned() {
                            let mut a = json!({ "id": tid });
                            for k in ["result", "error"] {
                                if let Some(v) = m.get(k) {
                                    a[k] = v.clone();
                                }
                            }
                            out.push((t, a));
                        }
                    }
                }
                _ => {}
            }
        }
        deliver(out);
    }

    async fn open_doc(self: Arc<Self>, n: u32, uri: String, language: Option<String>) {
        if !uri.starts_with("file:") {
            return;
        }
        let have = {
            let mut i = self.inner.lock().unwrap();
            match i.docs.get_mut(&uri) {
                Some(d) => {
                    d.tabs.insert(n);
                    true
                }
                None => false,
            }
        };
        if !have {
            let Some(path) = url_path(&uri) else { return };
            let Ok(doc) = self.documents.open(&path).await else {
                return;
            };
            let fresh = {
                let mut i = self.inner.lock().unwrap();
                if i.stopped || !i.tabs.contains_key(&n) {
                    return;
                }
                match i.docs.get_mut(&uri) {
                    Some(d) => {
                        d.tabs.insert(n);
                        false
                    }
                    None => {
                        let follow = self.follow(&uri, &doc);
                        i.docs.insert(
                            uri.clone(),
                            OpenDoc {
                                doc: doc.clone(),
                                language: language.unwrap_or_else(|| self.language.clone()),
                                version: 1,
                                tabs: [n].into(),
                                dirty: false,
                                pulse: 0,
                                follow,
                            },
                        );
                        true
                    }
                }
            };
            if fresh {
                let mut rx = self.init.subscribe();
                let ok = matches!(
                    rx.wait_for(Option::is_some).await.map(|v| v.clone()),
                    Ok(Some(Ok(_)))
                );
                let mut i = self.inner.lock().unwrap();
                let still = i.docs.get(&uri).is_some_and(|d| Arc::ptr_eq(&d.doc, &doc));
                if !ok || !still {
                    return;
                }
                let d = &i.docs[&uri];
                let msg = json!({ "method": "textDocument/didOpen", "params": { "textDocument": {
                    "uri": uri, "languageId": d.language, "version": d.version, "text": doc.text(),
                } } });
                self.to_server(&mut i, msg);
            }
        }
        let diag = {
            let i = self.inner.lock().unwrap();
            i.diagnostics
                .get(&uri)
                .cloned()
                .zip(i.tabs.get(&n).cloned())
        };
        if let Some((d, t)) = diag {
            (t.send)(d);
        }
    }

    /// Follows a document's edits, sending its text a moment after each run
    /// of them.
    fn follow(&self, uri: &str, doc: &Arc<SharedDoc>) -> JoinHandle<()> {
        let mut rx = doc.subscribe();
        let (me, uri) = (self.me.clone(), uri.to_owned());
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return,
                }
                let Some(h) = me.upgrade() else { return };
                let pulse = {
                    let mut i = h.inner.lock().unwrap();
                    let Some(d) = i.docs.get_mut(&uri) else {
                        return;
                    };
                    d.dirty = true;
                    d.pulse += 1;
                    d.pulse
                };
                let (me, uri) = (me.clone(), uri.clone());
                tokio::spawn(async move {
                    tokio::time::sleep(SYNC_AFTER).await;
                    let Some(h) = me.upgrade() else { return };
                    let mut i = h.inner.lock().unwrap();
                    if i.docs.get(&uri).is_some_and(|d| d.pulse == pulse) {
                        h.flush(&mut i, &uri);
                    }
                });
            }
        })
    }

    /// Sends a document's text, if it changed since it was last sent.
    fn flush(&self, i: &mut Inner, uri: &str) {
        let Some(d) = i.docs.get_mut(uri) else { return };
        if !d.dirty {
            return;
        }
        d.dirty = false;
        d.version += 1;
        let msg = json!({ "method": "textDocument/didChange", "params": {
            "textDocument": { "uri": uri, "version": d.version },
            "contentChanges": [{ "text": d.doc.text() }],
        } });
        self.to_server(i, msg);
    }

    fn flush_all(&self, i: &mut Inner) {
        let uris: Vec<String> = i.docs.keys().cloned().collect();
        for uri in uris {
            self.flush(i, &uri);
        }
    }

    fn close_doc(&self, i: &mut Inner, uri: &str, tell: bool) {
        let Some(d) = i.docs.remove(uri) else { return };
        d.follow.abort();
        if tell {
            self.to_server(i, json!({ "method": "textDocument/didClose", "params": { "textDocument": { "uri": uri } } }));
        }
    }
}

fn deliver(out: Out) {
    for (t, m) in out {
        (t.send)(m);
    }
}

fn id_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        v => v.to_string(),
    }
}

fn error_text(e: Option<&Value>) -> Option<String> {
    match e {
        None | Some(Value::Null) => None,
        Some(e) => Some(
            e.get("message")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| e.to_string()),
        ),
    }
}
