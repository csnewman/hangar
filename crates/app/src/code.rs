//! A connection to an environment's Code service (crates/agent's code
//! module), through the client API's Code socket: JSON requests answered
//! by id, and the service's events, on a thread of its own; and the
//! language servers' messages it relays, each tagged with the server's
//! number. A connection that drops, or stops answering (as one does across
//! the machine's sleep), is dialled again, and the service greeted again:
//! what was open on it (documents, language servers) is opened again by
//! its owner, as on the first Ready.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tungstenite::Message;

use crate::api::Client;
use crate::ws::{ConnectError, Liveness};

/// What the connection tells its owner.
#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    /// The service answered hello: the folder it works in, and more. Told
    /// again each time the connection is made again.
    Ready(Value),
    /// One of the service's events: fs.changed, git.changed, ...
    Event(String, Value),
    /// A language server's message, by the number lsp.start gave it.
    Lsp(u32, Value),
    /// The connection was lost, and is dialled again in the time given.
    /// Calls fail until the next Ready.
    Lost { why: String, retry_in: Duration },
    /// The connection ended for good: the server refused it.
    Closed(String),
}

enum Out {
    Frame(Vec<u8>),
    Close,
}

type Pending = Arc<Mutex<HashMap<u64, async_channel::Sender<Result<Value, String>>>>>;

/// Where the connection is: calls made before the first Ready wait for it;
/// after a loss, they fail until the next, since what they name (a
/// document, a language server) belonged to the connection lost.
const FIRST: u8 = 0;
const UP: u8 = 1;
const DOWN: u8 = 2;

struct Inner {
    tx: Sender<Out>,
    next: AtomicU64,
    pending: Pending,
    state: Arc<AtomicU8>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        let _ = self.tx.send(Out::Close);
    }
}

/// A connection, shared by its clones and ended when the last goes.
#[derive(Clone)]
pub struct Code(Arc<Inner>);

/// The first wait before dialling again, and the longest.
const RETRY_FIRST: Duration = Duration::from_millis(500);
const RETRY_MAX: Duration = Duration::from_secs(30);

/// How a connection ended.
enum End {
    /// Closed here, or its owner is gone.
    Done,
    /// Lost: whether it had been greeted, and why.
    Lost { ready: bool, why: String },
}

impl Code {
    /// Connects to the environment's Code service in root, telling notices
    /// what happens until it ends or notices returns false, and connecting
    /// again whenever the connection is lost.
    pub fn connect(
        client: &Client,
        env: &str,
        root: &str,
        notices: impl Fn(Notice) -> bool + Send + 'static,
    ) -> Code {
        let (url, auth) = client.code_socket(env, root);
        let (tx, rx) = mpsc::channel();
        let pending: Pending = Arc::default();
        let state = Arc::new(AtomicU8::new(FIRST));
        let code = Code(Arc::new(Inner {
            tx,
            next: AtomicU64::new(1),
            pending: pending.clone(),
            state: state.clone(),
        }));
        let root = root.to_string();
        thread::Builder::new()
            .name("code".into())
            .spawn(move || {
                let mut wait = RETRY_FIRST;
                loop {
                    let hello = request(0, "hello", json!({ "root": root }));
                    let ended = run(&url, &auth, hello, &rx, &pending, &state, &notices);
                    // What was asked of the connection lost is not answered.
                    state.store(DOWN, Ordering::SeqCst);
                    for (_, waiting) in pending.lock().unwrap().drain() {
                        let _ = waiting.send_blocking(Err("the connection was lost".into()));
                    }
                    let why = match ended {
                        Ok(End::Done) => return,
                        Ok(End::Lost { ready, why }) => {
                            if ready {
                                wait = RETRY_FIRST;
                            }
                            why
                        }
                        Err(e) if e.passes() => e.to_string(),
                        Err(e) => {
                            notices(Notice::Closed(e.to_string()));
                            return;
                        }
                    };
                    if !notices(Notice::Lost {
                        why,
                        retry_in: wait,
                    }) || !pause(wait, &rx)
                    {
                        return;
                    }
                    wait = (wait * 2).min(RETRY_MAX);
                }
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
        if self.0.state.load(Ordering::SeqCst) == DOWN {
            let _ = tx.try_send(Err("not connected to the Code service".into()));
        } else {
            self.0.pending.lock().unwrap().insert(id, tx);
            if self
                .0
                .tx
                .send(Out::Frame(request(id, method, params)))
                .is_err()
            {
                self.0.pending.lock().unwrap().remove(&id);
            }
        }
        async move {
            rx.recv()
                .await
                .unwrap_or_else(|_| Err("the connection ended".into()))
        }
    }
}

impl Code {
    /// Sends a JSON-RPC message to a language server started on this
    /// connection; nothing, while it is lost.
    pub fn send_lsp(&self, server: u32, message: &Value) {
        if self.0.state.load(Ordering::SeqCst) == DOWN {
            return;
        }
        let mut b = vec![b'l'];
        b.extend_from_slice(&server.to_be_bytes());
        b.extend_from_slice(message.to_string().as_bytes());
        let _ = self.0.tx.send(Out::Frame(b));
    }
}

/// Waits before dialling again, dropping what was sent for the connection
/// lost. False when the connection is closed.
fn pause(wait: Duration, rx: &Receiver<Out>) -> bool {
    let until = Instant::now() + wait;
    loop {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return true;
        }
        match rx.recv_timeout(left) {
            Ok(Out::Frame(_)) => {}
            Ok(Out::Close) | Err(RecvTimeoutError::Disconnected) => return false,
            Err(RecvTimeoutError::Timeout) => return true,
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

/// One connection, until it ends; one that cannot be made is the Err.
fn run(
    url: &str,
    auth: &str,
    hello: Vec<u8>,
    rx: &Receiver<Out>,
    pending: &Pending,
    state: &AtomicU8,
    notices: &impl Fn(Notice) -> bool,
) -> Result<End, ConnectError> {
    let mut ws = crate::ws::connect(url, auth)?;
    // What was sent for a connection lost before this one is not sent on it.
    if state.load(Ordering::SeqCst) == DOWN {
        loop {
            match rx.try_recv() {
                Ok(Out::Frame(_)) => {}
                Ok(Out::Close) | Err(TryRecvError::Disconnected) => return Ok(End::Done),
                Err(TryRecvError::Empty) => break,
            }
        }
    }
    let mut ready = false;
    let lost = |ready: bool, why: String| Ok(End::Lost { ready, why });
    if let Err(e) = ws.write(Message::Binary(hello.into())) {
        return lost(ready, e.to_string());
    }
    let mut live = Liveness::default();
    loop {
        loop {
            match rx.try_recv() {
                Ok(Out::Frame(f)) => {
                    if let Err(e) = ws.write(Message::Binary(f.into())) {
                        return lost(ready, e.to_string());
                    }
                }
                Ok(Out::Close) | Err(TryRecvError::Disconnected) => {
                    let _ = ws.close(None);
                    let _ = ws.flush();
                    return Ok(End::Done);
                }
                Err(TryRecvError::Empty) => break,
            }
        }
        if let Err(why) = live.check(&mut ws) {
            return lost(ready, why);
        }
        let _ = ws.flush();
        let msg = match ws.read() {
            Ok(m) => m,
            Err(e) if crate::ws::idle(&e) => continue,
            Err(e) => return lost(ready, e.to_string()),
        };
        live.heard();
        let b = match msg {
            Message::Binary(b) if b.first() == Some(&b'l') && b.len() >= 5 => {
                let server = u32::from_be_bytes([b[1], b[2], b[3], b[4]]);
                if let Ok(v) = serde_json::from_slice::<Value>(&b[5..])
                    && !notices(Notice::Lsp(server, v))
                {
                    return Ok(End::Done);
                }
                continue;
            }
            Message::Binary(b) if b.first() == Some(&b'r') => b,
            Message::Close(f) => {
                let why = match f {
                    Some(f) if !f.reason.is_empty() => f.reason.to_string(),
                    _ => "the service closed the connection".into(),
                };
                return lost(ready, why);
            }
            _ => continue,
        };
        let Ok(v) = serde_json::from_slice::<Value>(&b[1..]) else {
            continue;
        };
        if let Some(event) = v.get("event").and_then(Value::as_str) {
            let params = v.get("params").cloned().unwrap_or(Value::Null);
            if !notices(Notice::Event(event.to_string(), params)) {
                return Ok(End::Done);
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
            match answer {
                Ok(hello) => {
                    ready = true;
                    state.store(UP, Ordering::SeqCst);
                    if !notices(Notice::Ready(hello)) {
                        return Ok(End::Done);
                    }
                }
                Err(why) => return Err(ConnectError::Refused(0, why)),
            }
        } else if let Some(waiting) = pending.lock().unwrap().remove(&id) {
            let _ = waiting.send_blocking(answer);
        }
    }
}
