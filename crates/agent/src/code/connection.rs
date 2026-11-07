use std::path::Path;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::{Map, Value, json};
use tokio::net::UnixStream;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use super::error::{Error, Result};
use super::files::{Listening, Run};
use super::frame;
use super::paths::resolve;
use super::{Folder, Services};

/// One browser tab's Code view: its requests, and the events it is sent. It
/// works in the service's folder unless its hello names another, for that
/// tab alone.
pub struct Connection {
    s: Arc<Services>,
    out: mpsc::UnboundedSender<Vec<u8>>,
    state: Mutex<State>,
}

struct State {
    folder: Arc<Folder>,
    /// Forwards the folder's file changes to the tab.
    watching: Option<(JoinHandle<()>, Listening)>,
}

impl Drop for State {
    fn drop(&mut self) {
        if let Some((task, _)) = self.watching.take() {
            task.abort();
        }
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
    let conn = Arc::new(Connection {
        s: s.clone(),
        out: out.clone(),
        state: Mutex::new(State {
            folder,
            watching: None,
        }),
    });
    conn.watch();
    let forwards = [
        forward(s.settings.subscribe(), out.clone(), "settings.changed"),
        forward(s.workspace.subscribe(), out.clone(), "workspace.changed"),
    ];

    while let Some(f) = frame::read(&mut rd).await {
        if f.first() == Some(&frame::JSON) {
            let Ok(req) = serde_json::from_slice::<Request>(&f[1..]) else {
                continue;
            };
            let conn = conn.clone();
            tokio::spawn(async move { conn.handle(req).await });
        }
    }

    for t in forwards {
        t.abort();
    }
    conn.state.lock().unwrap().watching = None;
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
fn forward<T: Serialize + Clone + Send + 'static>(
    mut rx: broadcast::Receiver<T>,
    out: mpsc::UnboundedSender<Vec<u8>>,
    event: &'static str,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(v) => {
                    if send_json(&out, &json!({ "event": event, "params": v })).is_err() {
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

    /// Announces every change under the tab's folder; the browser decides
    /// what to reload.
    fn watch(&self) {
        let folder = self.folder();
        let (mut rx, listening) = folder.files.listen();
        let out = self.out.clone();
        let task = tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(dirs) => {
                        if send_json(
                            &out,
                            &json!({ "event": "fs.changed", "params": { "dirs": dirs } }),
                        )
                        .is_err()
                        {
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        });
        let mut st = self.state.lock().unwrap();
        if let Some((old, _)) = st.watching.replace((task, listening)) {
            old.abort();
        }
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
        }
        Ok(json!({
            "root": self.folder().root.display().to_string(),
            "home": self.s.root.display().to_string(),
            "settings": self.s.settings.path().display().to_string(),
        }))
    }

    async fn call(&self, method: &str, p: Params<'_>) -> Result<Value> {
        let f = self.folder();
        let files = &f.files;
        Ok(match method {
            "hello" => self.hello(p.get("root")).await?,
            // Answered at once, so the browser can time the way here and back.
            "ping" => Value::Null,
            "fs.list" => to_value(files.list(p.str("path")?).await?),
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
            _ => return Err(Error::msg(format!("no such method: {method}"))),
        })
    }
}

fn to_value<T: Serialize>(v: T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}
