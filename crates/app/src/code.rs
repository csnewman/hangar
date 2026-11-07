//! A connection to an environment's Code service (crates/agent's code
//! module), through the client API's Code socket: JSON requests answered
//! by id, and the service's events, on a thread of its own.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::{Value, json};
use tungstenite::Message;

use crate::api::Client;

/// What the connection tells its owner.
#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    /// The service answered hello: the folder it works in, and more.
    Ready(Value),
    /// One of the service's events: fs.changed, git.changed, ...
    Event(String, Value),
    /// The connection ended, or could not be made.
    Closed(String),
}

enum Out {
    Frame(Vec<u8>),
    Close,
}

type Pending = Arc<Mutex<HashMap<u64, async_channel::Sender<Result<Value, String>>>>>;

struct Inner {
    tx: Sender<Out>,
    next: AtomicU64,
    pending: Pending,
}

impl Drop for Inner {
    fn drop(&mut self) {
        let _ = self.tx.send(Out::Close);
    }
}

/// A connection, shared by its clones and ended when the last goes.
#[derive(Clone)]
pub struct Code(Arc<Inner>);

impl Code {
    /// Connects to the environment's Code service in root, telling notices
    /// what happens until it ends or notices returns false.
    pub fn connect(
        client: &Client,
        env: &str,
        root: &str,
        notices: impl Fn(Notice) -> bool + Send + 'static,
    ) -> Code {
        let (url, auth) = client.code_socket(env, root);
        let (tx, rx) = mpsc::channel();
        let pending: Pending = Arc::default();
        let code = Code(Arc::new(Inner {
            tx,
            next: AtomicU64::new(1),
            pending: pending.clone(),
        }));
        let hello = request(0, "hello", json!({ "root": root }));
        thread::Builder::new()
            .name("code".into())
            .spawn(move || {
                let why = run(&url, &auth, hello, &rx, &pending, &notices)
                    .err()
                    .unwrap_or_else(|| "the connection ended".into());
                for (_, waiting) in pending.lock().unwrap().drain() {
                    let _ = waiting.send_blocking(Err(why.clone()));
                }
                notices(Notice::Closed(why));
            })
            .expect("starting a Code connection");
        code
    }

    /// Asks the service something, answered with its result or its error.
    pub fn call(
        &self,
        method: &str,
        params: Value,
    ) -> impl std::future::Future<Output = Result<Value, String>> + use<> {
        let id = self.0.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = async_channel::bounded(1);
        self.0.pending.lock().unwrap().insert(id, tx);
        if self
            .0
            .tx
            .send(Out::Frame(request(id, method, params)))
            .is_err()
        {
            self.0.pending.lock().unwrap().remove(&id);
        }
        async move {
            rx.recv()
                .await
                .unwrap_or_else(|_| Err("the connection ended".into()))
        }
    }
}

fn request(id: u64, method: &str, params: Value) -> Vec<u8> {
    let mut b = vec![b'r'];
    b.extend_from_slice(
        json!({ "id": id, "method": method, "params": params })
            .to_string()
            .as_bytes(),
    );
    b
}

fn run(
    url: &str,
    auth: &str,
    hello: Vec<u8>,
    rx: &Receiver<Out>,
    pending: &Pending,
    notices: &impl Fn(Notice) -> bool,
) -> Result<(), String> {
    let mut ws = crate::ws::connect(url, auth)?;
    ws.write(Message::Binary(hello.into()))
        .map_err(|e| e.to_string())?;
    loop {
        loop {
            match rx.try_recv() {
                Ok(Out::Frame(f)) => ws
                    .write(Message::Binary(f.into()))
                    .map_err(|e| e.to_string())?,
                Ok(Out::Close) | Err(TryRecvError::Disconnected) => {
                    let _ = ws.close(None);
                    let _ = ws.flush();
                    return Ok(());
                }
                Err(TryRecvError::Empty) => break,
            }
        }
        let _ = ws.flush();
        let msg = match ws.read() {
            Ok(m) => m,
            Err(e) if crate::ws::idle(&e) => continue,
            Err(tungstenite::Error::ConnectionClosed) => return Ok(()),
            Err(e) => return Err(e.to_string()),
        };
        let b = match msg {
            Message::Binary(b) if b.first() == Some(&b'r') => b,
            Message::Close(f) => {
                return match f {
                    Some(f) if !f.reason.is_empty() => Err(f.reason.to_string()),
                    _ => Ok(()),
                };
            }
            _ => continue,
        };
        let Ok(v) = serde_json::from_slice::<Value>(&b[1..]) else {
            continue;
        };
        if let Some(event) = v.get("event").and_then(Value::as_str) {
            let params = v.get("params").cloned().unwrap_or(Value::Null);
            if !notices(Notice::Event(event.to_string(), params)) {
                return Ok(());
            }
            continue;
        }
        let Some(id) = v.get("id").and_then(Value::as_u64) else {
            continue;
        };
        let answer = match v.get("error") {
            Some(e) => Err(e.as_str().unwrap_or("the service failed").to_string()),
            None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
        };
        if id == 0 {
            if !notices(Notice::Ready(answer?)) {
                return Ok(());
            }
        } else if let Some(waiting) = pending.lock().unwrap().remove(&id) {
            let _ = waiting.send_blocking(answer);
        }
    }
}
