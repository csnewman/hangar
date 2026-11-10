//! A connection to one of an environment's terminal sessions: the client
//! API's terminal WebSocket (internal/clientapi/openapi.yaml), on a thread
//! of its own. A connection that drops, or stops answering (as one does
//! across the machine's sleep), is dialled again to the same session, which
//! runs on in the environment meanwhile.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;
use tungstenite::Message;

use crate::api::Client;
use crate::ws::{ConnectError, Liveness};

/// What the session says.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Attached: the session, and how many bytes of the output that follows
    /// replay its recent past, which must not be answered. Attached again
    /// after a reconnection, the past is replayed again.
    Attached {
        session: String,
        title: String,
        history: usize,
        /// Whether this connection holds the size.
        owner: bool,
    },
    /// What the shell wrote.
    Output(Vec<u8>),
    /// The terminal's size, how many windows have it open, and whether
    /// this connection holds the size.
    Size {
        cols: u16,
        rows: u16,
        viewers: u16,
        owner: bool,
    },
    /// The connection was lost, and is dialled again in the time given.
    Reconnecting { why: String, retry_in: Duration },
    /// The shell exited, with its status.
    Exited(i32),
    /// The session cannot be reached: the server refused it.
    Failed(String),
}

/// What is sent to the session.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Input(Vec<u8>),
    /// Take the size, at this window's.
    Resize(u16, u16),
    /// The size this window would have the terminal: its size while this
    /// connection holds it.
    Prefer(u16, u16),
    Close,
}

/// A session's connection, ending when this is dropped.
pub struct Connection {
    tx: Sender<Command>,
}

impl Connection {
    pub fn send(&self, c: Command) {
        let _ = self.tx.send(c);
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Close);
    }
}

/// The first wait before dialling again, and the longest: a terminal is
/// in front of someone, so it is tried often.
const RETRY_FIRST: Duration = Duration::from_millis(500);
const RETRY_MAX: Duration = Duration::from_secs(10);

/// How a connection ended.
enum End {
    /// Closed here, or the shell exited: nothing more to do.
    Done,
    /// Lost, after it had attached or before: worth dialling again.
    Lost { attached: bool, why: String },
}

/// Attaches to an environment's terminal session, or starts a new one at
/// cols by rows, telling events what it says until it ends or events
/// returns false, and attaching again to the same session whenever the
/// connection is lost.
pub fn attach(
    client: &Client,
    env: &str,
    session: Option<&str>,
    cols: u16,
    rows: u16,
    events: impl Fn(Event) -> bool + Send + 'static,
) -> Connection {
    let (client, env) = (client.clone(), env.to_string());
    let mut session = session.map(str::to_string);
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("terminal".into())
        .spawn(move || {
            let mut size = (cols, rows);
            let mut wait = RETRY_FIRST;
            loop {
                let (url, auth) = client.terminal_socket(&env, session.as_deref(), size.0, size.1);
                let why = match run(&url, &auth, &rx, &events, &mut session, &mut size) {
                    Ok(End::Done) => return,
                    Ok(End::Lost { attached, why }) => {
                        if attached {
                            wait = RETRY_FIRST;
                        }
                        why
                    }
                    Err(e) if e.passes() => e.to_string(),
                    Err(e) => {
                        events(Event::Failed(e.to_string()));
                        return;
                    }
                };
                if !events(Event::Reconnecting {
                    why,
                    retry_in: wait,
                }) || !pause(wait, &rx, &mut size)
                {
                    return;
                }
                wait = (wait * 2).min(RETRY_MAX);
            }
        })
        .expect("starting a terminal's connection");
    Connection { tx }
}

/// Waits before dialling again, keeping the size asked for; what is typed
/// meanwhile is dropped, not sent late. False when the connection is closed.
fn pause(wait: Duration, rx: &Receiver<Command>, size: &mut (u16, u16)) -> bool {
    let until = Instant::now() + wait;
    loop {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return true;
        }
        match rx.recv_timeout(left) {
            Ok(Command::Resize(c, r) | Command::Prefer(c, r)) => *size = (c, r),
            Ok(Command::Input(_)) => {}
            Ok(Command::Close) | Err(RecvTimeoutError::Disconnected) => return false,
            Err(RecvTimeoutError::Timeout) => return true,
        }
    }
}

/// The attach's reply, the socket's first message.
#[derive(Deserialize)]
struct Reply {
    #[serde(default)]
    err: String,
    session: Option<Attached>,
    #[serde(default)]
    history: usize,
    #[serde(default)]
    owner: bool,
}

#[derive(Deserialize)]
struct Attached {
    id: String,
    #[serde(default)]
    title: String,
}

/// One connection, until it ends; a connection that cannot be made is the
/// Err.
fn run(
    url: &str,
    auth: &str,
    rx: &Receiver<Command>,
    events: &impl Fn(Event) -> bool,
    session: &mut Option<String>,
    size: &mut (u16, u16),
) -> Result<End, ConnectError> {
    let mut ws = crate::ws::connect(url, auth)?;
    let mut attached = false;
    let mut live = Liveness::default();
    let lost = |attached: bool, why: String| Ok(End::Lost { attached, why });
    loop {
        // What is to be sent first, then what has come.
        loop {
            let sent = match rx.try_recv() {
                Ok(Command::Input(b)) => frame(&mut ws, b'i', &b),
                Ok(Command::Resize(c, r)) => sized(&mut ws, b'r', (c, r), size),
                Ok(Command::Prefer(c, r)) => sized(&mut ws, b'p', (c, r), size),
                Ok(Command::Close) | Err(TryRecvError::Disconnected) => {
                    let _ = ws.close(None);
                    let _ = ws.flush();
                    return Ok(End::Done);
                }
                Err(TryRecvError::Empty) => break,
            };
            if let Err(why) = sent {
                return lost(attached, why);
            }
        }
        if let Err(why) = live.check(&mut ws) {
            return lost(attached, why);
        }
        let _ = ws.flush();
        let msg = match ws.read() {
            Ok(m) => m,
            Err(e) if crate::ws::idle(&e) => continue,
            Err(e) => return lost(attached, e.to_string()),
        };
        live.heard();
        let event = match msg {
            Message::Text(t) => {
                let r: Reply = match serde_json::from_str(&t) {
                    Ok(r) => r,
                    Err(e) => return lost(attached, e.to_string()),
                };
                if !r.err.is_empty() {
                    // Refused once attached: the session is gone.
                    return Err(ConnectError::Refused(0, r.err));
                }
                match r.session {
                    Some(a) => {
                        attached = true;
                        *session = Some(a.id.clone());
                        Event::Attached {
                            session: a.id,
                            title: a.title,
                            history: r.history,
                            owner: r.owner,
                        }
                    }
                    None => continue,
                }
            }
            Message::Binary(b) if !b.is_empty() => {
                let p = &b[1..];
                match b[0] {
                    b'o' => Event::Output(p.to_vec()),
                    b's' if p.len() == 7 => Event::Size {
                        cols: u16::from_be_bytes([p[0], p[1]]),
                        rows: u16::from_be_bytes([p[2], p[3]]),
                        viewers: u16::from_be_bytes([p[4], p[5]]),
                        owner: p[6] == 1,
                    },
                    b'x' => {
                        let code = p
                            .get(..4)
                            .map_or(0, |c| i32::from_be_bytes([c[0], c[1], c[2], c[3]]));
                        events(Event::Exited(code));
                        return Ok(End::Done);
                    }
                    _ => continue,
                }
            }
            Message::Close(_) => return lost(attached, "the server closed the connection".into()),
            _ => continue,
        };
        if !events(event) {
            let _ = ws.close(None);
            return Ok(End::Done);
        }
    }
}

fn frame(ws: &mut crate::ws::Socket, typ: u8, payload: &[u8]) -> Result<(), String> {
    let mut b = Vec::with_capacity(payload.len() + 1);
    b.push(typ);
    b.extend_from_slice(payload);
    ws.write(Message::Binary(b.into()))
        .map_err(|e| e.to_string())
}

/// Sends a size, to take (r) or as this window's (p), keeping it for the
/// next dial.
fn sized(
    ws: &mut crate::ws::Socket,
    typ: u8,
    (c, r): (u16, u16),
    size: &mut (u16, u16),
) -> Result<(), String> {
    *size = (c, r);
    let mut p = c.to_be_bytes().to_vec();
    p.extend_from_slice(&r.to_be_bytes());
    frame(ws, typ, &p)
}
