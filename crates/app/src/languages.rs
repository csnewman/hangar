//! The environment's language servers, as the app uses them: one per
//! project and language, started through the project's Code connection
//! (lsp.start), which shares it with every browser tab using it and keeps
//! it told of the shared documents' text. The app opens the files its
//! editors show in it, keeps the diagnostics it publishes for every file,
//! and asks it things for the editors.

use std::collections::{BTreeMap, HashMap};

use gpui::{Context, Entity, EventEmitter};
use serde_json::{Value, json};

use crate::code::Code;
use crate::workspace::{self, Workspace};

/// The LSP language ID for a file, from its name, or None for one no
/// server is known for (the web UI's lsp.ts).
pub fn language_id(path: &str) -> Option<&'static str> {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name == "go.mod" || name == "go.work" {
        return Some("go.mod");
    }
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())?;
    Some(match ext.as_str() {
        "go" => "go",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "typescriptreact",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "py" => "python",
        "rs" => "rust",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" => "cpp",
        "sh" | "bash" => "shellscript",
        "yaml" | "yml" => "yaml",
        "json" => "json",
        "java" => "java",
        "zig" => "zig",
        _ => return None,
    })
}

/// The server a language's files go to: go.mod's is gopls.
pub fn server_language(id: &str) -> &str {
    if id == "go.mod" { "go" } else { id }
}

/// A file's URI, each part of its path percent-encoded as the browser's
/// encodeURIComponent does.
pub fn file_uri(abs: &str) -> String {
    let mut out = String::from("file://");
    for (i, part) in abs.split('/').enumerate() {
        if i > 0 {
            out.push('/');
        }
        for b in part.bytes() {
            if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
                out.push(b as char);
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

/// A file URI's path, decoded.
pub fn uri_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.find('/').map_or(rest, |i| &rest[i..]);
    let b = rest.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&rest[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// A server, by its project's root and its language.
type Key = (String, String);

enum Server {
    /// Asked for; the files to open in it once it is ready.
    Starting {
        n: Option<u32>,
        waiting: Vec<String>,
    },
    Ready {
        n: u32,
    },
    /// Not to be had: none installed, the folder not trusted, or it
    /// exited. Says why.
    Unavailable(String),
}

/// What the language servers tell the app's views.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A file's diagnostics changed, by its absolute path.
    Diagnostics(String),
    /// A server started, could not, or stopped.
    Servers,
}

impl EventEmitter<Event> for Languages {}

pub struct Languages {
    workspace: Entity<Workspace>,
    servers: HashMap<Key, Server>,
    /// The files the editors show, by absolute path, and how many show
    /// each.
    wanted: HashMap<String, usize>,
    /// Those opened in their server.
    opened: HashMap<String, Key>,
    /// Each file's diagnostics, by absolute path.
    diagnostics: BTreeMap<String, Vec<lsp_types::Diagnostic>>,
    /// Requests sent, by server and id, awaiting their answers.
    pending: HashMap<(Key, u64), async_channel::Sender<Result<Value, String>>>,
    next_id: u64,
    /// The characters each server asks to be shown completions after,
    /// from its answer to initialize.
    triggers: HashMap<Key, Vec<String>>,
    _sub: gpui::Subscription,
}

impl Languages {
    pub fn new(workspace: Entity<Workspace>, cx: &mut Context<Self>) -> Self {
        let sub = cx.subscribe(&workspace, |l, _, e: &workspace::Event, cx| match e {
            workspace::Event::Projects => l.attach_all(cx),
            workspace::Event::Lost { root } => l.lost(root, cx),
            workspace::Event::Lsp {
                root,
                server,
                message,
            } => l.heard(root, *server, message, cx),
            workspace::Event::Heard {
                root,
                event,
                params,
            } if event == "lsp.exited" => {
                let n = params["server"].as_u64().map(|n| n as u32);
                let why = params["why"].as_str().unwrap_or("it exited").to_string();
                l.exited(root, n, why, cx);
            }
            _ => {}
        });
        Languages {
            workspace,
            servers: HashMap::new(),
            wanted: HashMap::new(),
            opened: HashMap::new(),
            diagnostics: BTreeMap::new(),
            pending: HashMap::new(),
            next_id: 1,
            triggers: HashMap::new(),
            _sub: sub,
        }
    }

    /// Every file's diagnostics, by absolute path.
    pub fn diagnostics(&self) -> &BTreeMap<String, Vec<lsp_types::Diagnostic>> {
        &self.diagnostics
    }

    pub fn diagnostics_of(&self, abs: &str) -> &[lsp_types::Diagnostic] {
        self.diagnostics.get(abs).map_or(&[], |d| d.as_slice())
    }

    /// Why each language has no server in a project, where it has none.
    pub fn unavailable(&self) -> Vec<(String, String, String)> {
        let mut out: Vec<_> = self
            .servers
            .iter()
            .filter_map(|((root, lang), s)| match s {
                Server::Unavailable(why) => Some((root.clone(), lang.clone(), why.clone())),
                _ => None,
            })
            .collect();
        out.sort();
        out
    }

    /// Opens a file an editor shows in its language's server.
    pub fn open(&mut self, abs: &str, cx: &mut Context<Self>) {
        *self.wanted.entry(abs.to_string()).or_default() += 1;
        self.attach(abs, cx);
    }

    /// Closes a file an editor stopped showing, once none shows it.
    pub fn close(&mut self, abs: &str, cx: &mut Context<Self>) {
        let Some(n) = self.wanted.get_mut(abs) else {
            return;
        };
        *n -= 1;
        if *n > 0 {
            return;
        }
        self.wanted.remove(abs);
        if let Some(key) = self.opened.remove(abs) {
            self.notify(
                &key,
                "textDocument/didClose",
                json!({ "textDocument": { "uri": file_uri(abs) } }),
                cx,
            );
        }
    }

    /// Forgets the servers of a project whose connection was lost: they
    /// are started again, and its files opened in them, once it is back.
    fn lost(&mut self, root: &str, cx: &mut Context<Self>) {
        self.servers.retain(|(r, _), _| r != root);
        self.triggers.retain(|(r, _), _| r != root);
        self.opened.retain(|_, (r, _)| r != root);
        self.pending.retain(|((r, _), _), waiting| {
            let keep = r != root;
            if !keep {
                let _ = waiting.try_send(Err("the connection was lost".into()));
            }
            keep
        });
        cx.emit(Event::Servers);
        cx.notify();
    }

    fn attach_all(&mut self, cx: &mut Context<Self>) {
        let files: Vec<String> = self
            .wanted
            .keys()
            .filter(|f| !self.opened.contains_key(*f))
            .cloned()
            .collect();
        for f in files {
            self.attach(&f, cx);
        }
    }

    /// Opens a wanted file in its server, starting the server first if
    /// need be.
    fn attach(&mut self, abs: &str, cx: &mut Context<Self>) {
        if self.opened.contains_key(abs) {
            return;
        }
        let Some(lang) = language_id(abs) else {
            return;
        };
        let Some(root) = self
            .workspace
            .read(cx)
            .project_of(abs)
            .map(|f| f.root.clone())
        else {
            return;
        };
        let key = (root, server_language(lang).to_string());
        match self.servers.get_mut(&key) {
            Some(Server::Ready { .. }) => self.did_open(&key, abs, cx),
            Some(Server::Starting { waiting, .. }) => waiting.push(abs.to_string()),
            Some(Server::Unavailable(_)) => {}
            None => {
                self.servers.insert(
                    key.clone(),
                    Server::Starting {
                        n: None,
                        waiting: vec![abs.to_string()],
                    },
                );
                self.start(key, cx);
            }
        }
    }

    fn did_open(&mut self, key: &Key, abs: &str, cx: &mut Context<Self>) {
        let lang = language_id(abs).unwrap_or(&key.1);
        // The service gives the server the shared document's text, and
        // keeps it told of its changes.
        self.notify(
            key,
            "textDocument/didOpen",
            json!({ "textDocument": {
                "uri": file_uri(abs), "languageId": lang, "version": 1, "text": "",
            } }),
            cx,
        );
        self.opened.insert(abs.to_string(), key.clone());
    }

    fn code(&self, key: &Key, cx: &gpui::App) -> Option<Code> {
        self.workspace.read(cx).code(&key.0)
    }

    /// Starts a project's server for a language and initializes it.
    fn start(&mut self, key: Key, cx: &mut Context<Self>) {
        let Some(code) = self.code(&key, cx) else {
            self.servers.insert(
                key,
                Server::Unavailable("The project is not connected.".into()),
            );
            return;
        };
        let call = code.call("lsp.start", json!({ "language": key.1 }));
        cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |l, cx| l.started(key, result, cx));
        })
        .detach();
    }

    fn started(&mut self, key: Key, result: Result<Value, String>, cx: &mut Context<Self>) {
        let why = match &result {
            Err(e) => Some(e.clone()),
            Ok(v) if v.get("untrusted").is_some() => Some(format!(
                "{} is not trusted to run its code, which its language server would.",
                v["untrusted"].as_str().unwrap_or(&key.0)
            )),
            Ok(v) if v.get("missing").is_some() => {
                let install = v["install"]
                    .as_array()
                    .and_then(|a| a.first())
                    .and_then(Value::as_str)
                    .map(|i| format!(" Install it with: {i}"))
                    .unwrap_or_default();
                Some(format!(
                    "{}.{install}",
                    v["missing"].as_str().unwrap_or("No server")
                ))
            }
            Ok(v) if v["server"].as_u64().is_none() => Some("The service gave no server.".into()),
            Ok(_) => None,
        };
        if let Some(why) = why {
            self.servers.insert(key, Server::Unavailable(why));
            cx.emit(Event::Servers);
            cx.notify();
            return;
        }
        let n = result.ok().and_then(|v| v["server"].as_u64()).unwrap_or(0) as u32;
        if let Some(Server::Starting { n: at, .. }) = self.servers.get_mut(&key) {
            *at = Some(n);
        }
        let root = key.0.clone();
        let init = self.request_on(
            &key,
            "initialize",
            json!({
                "processId": null,
                "rootUri": file_uri(&root),
                "capabilities": capabilities(),
                "clientInfo": { "name": "Hangar" },
            }),
            cx,
        );
        cx.spawn(async move |this, cx| {
            let result = init.await;
            let _ = this.update(cx, |l, cx| {
                let waiting = match l.servers.remove(&key) {
                    Some(Server::Starting { waiting, .. }) => waiting,
                    other => {
                        if let Some(s) = other {
                            l.servers.insert(key, s);
                        }
                        return;
                    }
                };
                let caps = match result {
                    Ok(v) => v["capabilities"].clone(),
                    Err(e) => {
                        l.servers.insert(key, Server::Unavailable(e));
                        cx.emit(Event::Servers);
                        return;
                    }
                };
                let triggers = caps["completionProvider"]["triggerCharacters"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|c| c.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                l.triggers.insert(key.clone(), triggers);
                l.servers.insert(key.clone(), Server::Ready { n });
                l.notify(&key, "initialized", json!({}), cx);
                for abs in waiting {
                    if l.wanted.contains_key(&abs) {
                        l.did_open(&key, &abs, cx);
                    }
                }
                cx.emit(Event::Servers);
            });
        })
        .detach();
    }

    fn number(&self, key: &Key) -> Option<u32> {
        match self.servers.get(key)? {
            Server::Ready { n } | Server::Starting { n: Some(n), .. } => Some(*n),
            _ => None,
        }
    }

    fn notify(&self, key: &Key, method: &str, params: Value, cx: &gpui::App) {
        if let (Some(code), Some(n)) = (self.code(key, cx), self.number(key)) {
            code.send_lsp(
                n,
                &json!({ "jsonrpc": "2.0", "method": method, "params": params }),
            );
        }
    }

    fn request_on(
        &mut self,
        key: &Key,
        method: &str,
        params: Value,
        cx: &gpui::App,
    ) -> impl std::future::Future<Output = Result<Value, String>> + use<> {
        let (tx, rx) = async_channel::bounded(1);
        match (self.code(key, cx), self.number(key)) {
            (Some(code), Some(n)) => {
                let id = self.next_id;
                self.next_id += 1;
                self.pending.insert((key.clone(), id), tx);
                code.send_lsp(
                    n,
                    &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
                );
            }
            _ => {
                let _ = tx.try_send(Err("No language server.".into()));
            }
        }
        async move {
            rx.recv()
                .await
                .unwrap_or_else(|_| Err("The language server went away.".into()))
        }
    }

    /// Asks the server of an open file something about it.
    pub fn request(
        &mut self,
        abs: &str,
        method: &str,
        params: Value,
        cx: &gpui::App,
    ) -> impl std::future::Future<Output = Result<Value, String>> + use<> {
        let key = self.opened.get(abs).cloned().unwrap_or_default();
        self.request_on(&key, method, params, cx)
    }

    /// Asks every running server for the symbols matching query, as
    /// IntelliJ's Go to Symbol: each answer is a list of SymbolInformation
    /// or WorkspaceSymbol.
    pub fn workspace_symbols(
        &mut self,
        query: &str,
        cx: &gpui::App,
    ) -> Vec<impl std::future::Future<Output = Result<Value, String>> + use<>> {
        let keys: Vec<Key> = self
            .servers
            .iter()
            .filter(|(_, s)| matches!(s, Server::Ready { .. }))
            .map(|(k, _)| k.clone())
            .collect();
        keys.iter()
            .map(|k| self.request_on(k, "workspace/symbol", json!({ "query": query }), cx))
            .collect()
    }

    /// The characters an open file's server shows completions after.
    pub fn triggers(&self, abs: &str) -> &[String] {
        self.opened
            .get(abs)
            .and_then(|k| self.triggers.get(k))
            .map_or(&[], |t| t.as_slice())
    }

    fn key_of(&self, root: &str, n: u32) -> Option<Key> {
        self.servers.iter().find_map(|(k, s)| match s {
            Server::Ready { n: m } | Server::Starting { n: Some(m), .. }
                if *m == n && k.0 == root =>
            {
                Some(k.clone())
            }
            _ => None,
        })
    }

    fn heard(&mut self, root: &str, n: u32, m: &Value, cx: &mut Context<Self>) {
        let Some(key) = self.key_of(root, n) else {
            return;
        };
        let method = m["method"].as_str();
        let id = m.get("id").filter(|i| !i.is_null());
        match (method, id) {
            // An answer to the app's request.
            (None, Some(id)) => {
                let Some(id) = id.as_u64() else { return };
                if let Some(tx) = self.pending.remove(&(key, id)) {
                    let answer = match m.get("error") {
                        Some(e) => Err(e["message"]
                            .as_str()
                            .unwrap_or("the server failed")
                            .to_string()),
                        None => Ok(m.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    let _ = tx.try_send(answer);
                }
            }
            // The server asking the app: its configuration (none is
            // set), or to register or report progress, which needs no
            // more than an answer.
            (Some(method), Some(id)) => {
                let result = if method == "workspace/configuration" {
                    let items = m["params"]["items"].as_array().map_or(0, Vec::len);
                    Value::Array(vec![Value::Null; items])
                } else {
                    Value::Null
                };
                if let Some(code) = self.code(&key, cx) {
                    code.send_lsp(n, &json!({ "jsonrpc": "2.0", "id": id, "result": result }));
                }
            }
            (Some("textDocument/publishDiagnostics"), None) => {
                let Some(abs) = m["params"]["uri"].as_str().and_then(uri_path) else {
                    return;
                };
                let list: Vec<lsp_types::Diagnostic> =
                    serde_json::from_value(m["params"]["diagnostics"].clone()).unwrap_or_default();
                if list.is_empty() {
                    self.diagnostics.remove(&abs);
                } else {
                    self.diagnostics.insert(abs.clone(), list);
                }
                cx.emit(Event::Diagnostics(abs));
                cx.notify();
            }
            _ => {}
        }
    }

    fn exited(&mut self, root: &str, n: Option<u32>, why: String, cx: &mut Context<Self>) {
        let Some(key) = n.and_then(|n| self.key_of(root, n)) else {
            return;
        };
        self.servers.insert(key.clone(), Server::Unavailable(why));
        self.opened.retain(|_, k| k != &key);
        self.pending.retain(|(k, _), _| k != &key);
        cx.emit(Event::Servers);
        cx.notify();
    }
}

/// What the app can do with what a server says.
fn capabilities() -> Value {
    json!({
        "general": { "positionEncodings": ["utf-16"] },
        "textDocument": {
            "synchronization": { "didSave": false, "dynamicRegistration": false },
            "publishDiagnostics": { "relatedInformation": false, "versionSupport": false },
            "hover": { "contentFormat": ["markdown", "plaintext"] },
            "definition": { "linkSupport": true },
            "completion": {
                "completionItem": { "snippetSupport": false, "documentationFormat": ["markdown", "plaintext"] },
            },
        },
        "workspace": { "configuration": true, "workspaceFolders": true },
        "window": { "workDoneProgress": false },
    })
}
