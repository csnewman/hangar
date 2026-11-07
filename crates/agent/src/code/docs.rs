//! Files open for editing: one shared document each, whoever has them open,
//! edited through one authority (`hangar_proto::collab`). A file is the one
//! its links lead to: a link and what it names, open side by side, are one
//! document, so neither saves over the other.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use hangar_proto::collab::Authority;
use hangar_proto::{Change, ChangeSet, Text};
use notify::{RecursiveMode, Watcher};
use ropey::Rope;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::error::{Error, Result, at};
use super::frame;
use super::paths::resolve;

/// The largest file opened for editing.
const MAX_SIZE: u64 = 8 << 20;
/// How long after the last edit a document is written to disk.
const SAVE_AFTER: Duration = Duration::from_millis(600);
/// How long a document nobody has open is kept, for a window coming back.
const KEEP: Duration = Duration::from_secs(60);
/// Who the file's own changes on disk are from.
const DISK: &str = "disk";

pub struct Documents {
    root: PathBuf,
    docs: tokio::sync::Mutex<HashMap<PathBuf, Arc<SharedDoc>>>,
}

impl Documents {
    pub fn new(root: &Path) -> Arc<Self> {
        Arc::new(Documents {
            root: root.to_path_buf(),
            docs: tokio::sync::Mutex::new(HashMap::new()),
        })
    }

    pub async fn open(self: &Arc<Self>, path: &str) -> Result<Arc<SharedDoc>> {
        let abs = resolve(&self.root, Path::new(path));
        let real = tokio::fs::canonicalize(&abs).await.unwrap_or(abs);
        let mut docs = self.docs.lock().await;
        if let Some(d) = docs.get(&real) {
            return Ok(d.clone());
        }
        let d = SharedDoc::load(&real, Arc::downgrade(self)).await?;
        docs.insert(real, d.clone());
        Ok(d)
    }

    async fn forget(&self, path: &Path, doc: &Arc<SharedDoc>) {
        let mut docs = self.docs.lock().await;
        if docs.get(path).is_some_and(|d| Arc::ptr_eq(d, doc)) {
            docs.remove(path);
        }
    }

    /// Writes every document with unsaved edits, before the service stops.
    pub async fn save_all(&self) {
        let docs: Vec<_> = self.docs.lock().await.values().cloned().collect();
        for d in docs {
            // The file went away; nothing to save it to.
            let _ = d.save().await;
        }
    }
}

/// One tab's place in a document: a tab may have one file open under two
/// names, a link's and its target's.
struct Member {
    conn: u64,
    n: u32,
    /// The name it opened it by.
    path: String,
    /// Its client in the document's changes and presence.
    client: String,
    out: mpsc::UnboundedSender<Vec<u8>>,
}

struct Inner {
    authority: Authority,
    members: Vec<Member>,
    /// Each client's presence: its carets and selections, at a version.
    presence: HashMap<String, (u64, Value)>,
    /// What the file held when last read or written, so a change on disk
    /// that is our own write is recognised.
    on_disk: String,
    /// Whether the file used CRLF line endings. The document holds LF, which
    /// is what editors work in, and the file gets its own endings back.
    crlf: bool,
    /// Whether the file is gone from disk. A deleted file's document is not
    /// saved, which would bring the file back; it follows the file again if
    /// it reappears.
    deleted: bool,
    /// Bumped by each edit and each departure, so a timer set before a later
    /// one does nothing.
    save_gen: u64,
    drop_gen: u64,
    watcher: Option<notify::RecommendedWatcher>,
}

/// One file as a shared document, kept in step with the file on disk both
/// ways: edits are written back after a short idle, and changes to the file
/// from anywhere else are applied to the document as edits. Its path is the
/// file's own, with no links in it.
pub struct SharedDoc {
    pub path: PathBuf,
    docs: Weak<Documents>,
    inner: Mutex<Inner>,
}

/// What a tab is given as it joins a document.
pub struct Joined {
    pub version: u64,
    pub text: String,
    pub client: String,
    pub presence: Vec<Value>,
}

impl SharedDoc {
    async fn load(path: &Path, docs: Weak<Documents>) -> Result<Arc<Self>> {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let m = tokio::fs::metadata(path).await.map_err(at(path))?;
        if !m.is_file() {
            return Err(Error::msg(format!("{name} is not a file")));
        }
        if m.len() > MAX_SIZE {
            return Err(Error::msg(format!("{name} is too large to edit here")));
        }
        let bytes = tokio::fs::read(path).await.map_err(at(path))?;
        if bytes[..bytes.len().min(8192)].contains(&0) {
            return Err(Error::msg(format!("{name} is not a text file")));
        }
        let content = String::from_utf8_lossy(&bytes).into_owned();
        let crlf = content.contains("\r\n");
        let text = from_disk(&content, crlf);
        let d = Arc::new(SharedDoc {
            path: path.to_path_buf(),
            docs,
            inner: Mutex::new(Inner {
                authority: Authority::new(Rope::from_str(&text)),
                members: Vec::new(),
                presence: HashMap::new(),
                on_disk: content,
                crlf,
                deleted: false,
                save_gen: 0,
                drop_gen: 0,
                watcher: None,
            }),
        });
        let watcher = d.watch();
        d.inner.lock().unwrap().watcher = watcher;
        Ok(d)
    }

    /// Adds a tab's document `n`, opened by the name `path`.
    pub fn join(
        &self,
        conn: u64,
        n: u32,
        path: &str,
        out: mpsc::UnboundedSender<Vec<u8>>,
    ) -> Joined {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let client = format!("c{}", NEXT.fetch_add(1, Ordering::Relaxed));
        let mut inner = self.inner.lock().unwrap();
        inner.drop_gen += 1;
        inner.members.push(Member {
            conn,
            n,
            path: path.to_owned(),
            client: client.clone(),
            out,
        });
        Joined {
            version: inner.authority.version(),
            text: inner.authority.text().to_string(),
            client,
            presence: inner
                .presence
                .iter()
                .map(|(c, (v, s))| json!({ "client": c, "version": v, "state": s }))
                .collect(),
        }
    }

    pub fn leave(self: &Arc<Self>, conn: u64, n: u32) {
        let mut inner = self.inner.lock().unwrap();
        let Some(i) = inner
            .members
            .iter()
            .position(|m| m.conn == conn && m.n == n)
        else {
            return;
        };
        let m = inner.members.remove(i);
        if inner.presence.remove(&m.client).is_some() {
            let version = inner.authority.version();
            broadcast(
                &inner.members,
                |n| json!({ "event": "doc.presence", "params": { "doc": n, "client": m.client, "version": version, "state": Value::Null } }),
            );
        }
        if inner.members.is_empty() {
            inner.drop_gen += 1;
            let generation = inner.drop_gen;
            let me = self.clone();
            tokio::spawn(async move {
                tokio::time::sleep(KEEP).await;
                if me.inner.lock().unwrap().drop_gen == generation {
                    me.drop_now().await;
                }
            });
        }
    }

    /// Takes a tab's unconfirmed changes, the first made at `version`; every
    /// member, the sender too, is sent what was accepted.
    pub fn push(
        self: &Arc<Self>,
        conn: u64,
        n: u32,
        version: u64,
        changes: &[Value],
    ) -> Result<()> {
        let changes: Vec<ChangeSet> = changes
            .iter()
            .map(ChangeSet::from_json)
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| Error::msg(e.to_string()))?;
        let mut inner = self.inner.lock().unwrap();
        let Some(client) = inner
            .members
            .iter()
            .find(|m| m.conn == conn && m.n == n)
            .map(|m| m.client.clone())
        else {
            return Err(Error::msg("the document is not open"));
        };
        let before = inner.authority.version();
        inner
            .authority
            .push(version, &client, &changes)
            .map_err(|e| Error::msg(format!("the document moved on without this tab: {e}")))?;
        announce(&mut inner, before);
        inner.save_gen += 1;
        let generation = inner.save_gen;
        drop(inner);
        let me = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(SAVE_AFTER).await;
            if me.inner.lock().unwrap().save_gen == generation
                && let Err(e) = me.save().await
            {
                eprintln!("hangar-code: saving {}: {e}", me.path.display());
            }
        });
        Ok(())
    }

    /// Relays a tab's carets and selections, made at `version`, to the
    /// others.
    pub fn presence(&self, conn: u64, n: u32, version: u64, state: Value) {
        let mut inner = self.inner.lock().unwrap();
        let Some(client) = inner
            .members
            .iter()
            .find(|m| m.conn == conn && m.n == n)
            .map(|m| m.client.clone())
        else {
            return;
        };
        inner
            .presence
            .insert(client.clone(), (version, state.clone()));
        let others: Vec<&Member> = inner
            .members
            .iter()
            .filter(|m| m.client != client)
            .collect();
        for m in others {
            send(
                m,
                &json!({ "event": "doc.presence", "params": { "doc": m.n, "client": client, "version": version, "state": state } }),
            );
        }
    }

    pub async fn save(&self) -> Result<()> {
        let content = {
            let mut inner = self.inner.lock().unwrap();
            if inner.deleted {
                return Ok(());
            }
            let content = to_disk(&inner.authority.text().to_string(), inner.crlf);
            if content == inner.on_disk {
                return Ok(());
            }
            inner.on_disk = content.clone();
            content
        };
        tokio::fs::write(&self.path, content.as_bytes())
            .await
            .map_err(at(&self.path))
    }

    /// Follows the file's directory, not the file: an editor that saves by
    /// writing a new file and renaming it over the old one would end a watch
    /// on the file itself. Without a watch, the document still saves; it
    /// just won't follow changes made elsewhere.
    fn watch(self: &Arc<Self>) -> Option<notify::RecommendedWatcher> {
        let dir = self.path.parent()?.to_path_buf();
        let name = self.path.file_name()?.to_owned();
        let (tx, mut rx) = mpsc::unbounded_channel::<()>();
        let mut watcher = notify::recommended_watcher(move |ev: notify::Result<notify::Event>| {
            if let Ok(ev) = ev
                && super::files::changes(&ev)
                && ev.paths.iter().any(|p| p.file_name() == Some(&name))
            {
                let _ = tx.send(());
            }
        })
        .ok()?;
        watcher.watch(&dir, RecursiveMode::NonRecursive).ok()?;
        let me = Arc::downgrade(self);
        tokio::spawn(async move {
            while rx.recv().await.is_some() {
                let Some(d) = me.upgrade() else { return };
                d.reload().await;
            }
        });
        Some(watcher)
    }

    async fn reload(&self) {
        let read = tokio::fs::read(&self.path).await;
        let mut inner = self.inner.lock().unwrap();
        let content = match read {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound && !inner.deleted {
                    inner.deleted = true;
                    inner.save_gen += 1;
                    for m in &inner.members {
                        send(
                            m,
                            &json!({ "event": "doc.deleted", "params": { "doc": m.n, "path": m.path } }),
                        );
                    }
                }
                return;
            }
        };
        if inner.deleted {
            // Back: restored from the trash, or made again.
            inner.deleted = false;
            for m in &inner.members {
                send(
                    m,
                    &json!({ "event": "doc.restored", "params": { "doc": m.n, "path": m.path } }),
                );
            }
        }
        if content == inner.on_disk {
            return;
        }
        // The file's line endings are its own, changed on disk too: a file
        // converted to LF is saved as LF from then on.
        inner.crlf = content.contains("\r\n");
        let next: Vec<u16> = from_disk(&content, inner.crlf).encode_utf16().collect();
        inner.on_disk = content;
        let cur: Vec<u16> = Text::from(inner.authority.text()).units().to_vec();
        if next == cur {
            return;
        }
        let Some(change) = least_edit(&cur, &next) else {
            return;
        };
        let Ok(set) = ChangeSet::of([change], cur.len()) else {
            return;
        };
        let before = inner.authority.version();
        if inner.authority.push(before, DISK, &[set]).is_ok() {
            announce(&mut inner, before);
        }
    }

    async fn drop_now(self: &Arc<Self>) {
        if !self.inner.lock().unwrap().members.is_empty() {
            return;
        }
        // Nowhere left to save it.
        let _ = self.save().await;
        self.inner.lock().unwrap().watcher = None;
        if let Some(docs) = self.docs.upgrade() {
            docs.forget(&self.path, self).await;
        }
    }

    /// The document's text, as an editor has it.
    #[allow(dead_code)]
    pub fn text(&self) -> String {
        self.inner.lock().unwrap().authority.text().to_string()
    }
}

/// Sends every member the updates since `before`.
fn announce(inner: &mut Inner, before: u64) {
    let Ok(updates) = inner.authority.since(before) else {
        return;
    };
    let updates = serde_json::to_value(updates).unwrap_or(Value::Null);
    broadcast(
        &inner.members,
        |n| json!({ "event": "doc.changes", "params": { "doc": n, "version": before, "updates": updates } }),
    );
}

fn broadcast(members: &[Member], event: impl Fn(u32) -> Value) {
    for m in members {
        send(m, &event(m.n));
    }
}

fn send(m: &Member, v: &Value) {
    if let Ok(body) = serde_json::to_vec(v) {
        let _ = m.out.send(frame::encode(frame::JSON, &[&body]));
    }
}

fn from_disk(s: &str, crlf: bool) -> String {
    if crlf {
        s.replace("\r\n", "\n")
    } else {
        s.to_owned()
    }
}

fn to_disk(s: &str, crlf: bool) -> String {
    if crlf {
        s.replace('\n', "\r\n")
    } else {
        s.to_owned()
    }
}

/// The smallest single edit that turns `cur` into `next`: the part both
/// share at the start and the end stays, so carets and anyone's typing
/// elsewhere in the document are undisturbed. Neither end splits a
/// character outside the BMP.
fn least_edit(cur: &[u16], next: &[u16]) -> Option<Change> {
    let high = |u: u16| (0xD800..0xDC00).contains(&u);
    let low = |u: u16| (0xDC00..0xE000).contains(&u);
    let mut start = cur.iter().zip(next).take_while(|(a, b)| a == b).count();
    if start > 0 && high(cur[start - 1]) {
        start -= 1;
    }
    let (mut end_cur, mut end_next) = (cur.len(), next.len());
    while end_cur > start && end_next > start && cur[end_cur - 1] == next[end_next - 1] {
        end_cur -= 1;
        end_next -= 1;
    }
    if end_cur < cur.len() && low(cur[end_cur]) {
        end_cur += 1;
        end_next += 1;
    }
    if start == end_cur && start == end_next {
        return None;
    }
    Some(Change {
        from: start,
        to: end_cur,
        insert: Text::from_units(next[start..end_next].to_vec()),
    })
}
