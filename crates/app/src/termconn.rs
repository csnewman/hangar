//! A connection to one of an environment's terminal sessions: the client
//! API's terminal WebSocket (internal/clientapi/openapi.yaml), on a thread
//! of its own.

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;

use serde::Deserialize;
use tungstenite::Message;

use crate::api::Client;

/// What the session says.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Attached: the session, and how many bytes of the output that follows
    /// replay its recent past, which must not be answered.
    Attached {
        session: String,
        title: String,
        history: usize,
    },
    /// What the shell wrote.
    Output(Vec<u8>),
    /// The terminal's size, and whether this connection holds it.
    Size { cols: u16, rows: u16, owner: bool },
    /// The shell exited, with its status.
    Exited(i32),
    /// The connection failed or could not be made.
    Failed(String),
}

/// What is sent to the session.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Input(Vec<u8>),
    /// Take the size.
    Resize(u16, u16),
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

/// Attaches to an environment's terminal session, or starts a new one at
/// cols by rows, telling events what it says until it ends or events
/// returns false.
pub fn attach(
    client: &Client,
    env: &str,
    session: Option<&str>,
    cols: u16,
    rows: u16,
    events: impl Fn(Event) -> bool + Send + 'static,
) -> Connection {
    let (url, auth) = client.terminal_socket(env, session, cols, rows);
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("terminal".into())
        .spawn(move || {
            if let Err(e) = run(&url, &auth, &rx, &events) {
                events(Event::Failed(e));
            }
        })
        .expect("starting a terminal's connection");
    Connection { tx }
}

/// The attach's reply, the socket's first message.
#[derive(Deserialize)]
struct Reply {
    #[serde(default)]
    err: String,
    session: Option<Attached>,
    #[serde(default)]
    history: usize,
}

#[derive(Deserialize)]
struct Attached {
    id: String,
    #[serde(default)]
    title: String,
}

fn run(
    url: &str,
    auth: &str,
    rx: &Receiver<Command>,
    events: &impl Fn(Event) -> bool,
) -> Result<(), String> {
    let mut ws = crate::ws::connect(url, auth)?;
    loop {
        // What is to be sent first, then what has come.
        loop {
            match rx.try_recv() {
                Ok(Command::Input(b)) => frame(&mut ws, b'i', &b)?,
                Ok(Command::Resize(c, r)) => {
                    let mut p = c.to_be_bytes().to_vec();
                    p.extend_from_slice(&r.to_be_bytes());
                    frame(&mut ws, b'r', &p)?
                }
                Ok(Command::Close) | Err(TryRecvError::Disconnected) => {
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
        let event = match msg {
            Message::Text(t) => {
                let r: Reply = serde_json::from_str(&t).map_err(|e| e.to_string())?;
                if !r.err.is_empty() {
                    return Err(r.err);
                }
                match r.session {
                    Some(a) => Event::Attached {
                        session: a.id,
                        title: a.title,
                        history: r.history,
                    },
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
                        owner: p[6] == 1,
                    },
                    b'x' => {
                        let code = p
                            .get(..4)
                            .map_or(0, |c| i32::from_be_bytes([c[0], c[1], c[2], c[3]]));
                        events(Event::Exited(code));
                        return Ok(());
                    }
                    _ => continue,
                }
            }
            Message::Close(_) => return Ok(()),
            _ => continue,
        };
        if !events(event) {
            let _ = ws.close(None);
            return Ok(());
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
