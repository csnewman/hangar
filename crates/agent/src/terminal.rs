//! Interactive shells that outlive the connections watching them, served on
//! vsock port 8101 in the protocol `internal/terminal` defines for the
//! worker.
//!
//! A session is a shell on a pseudo-terminal. It is started by the first
//! attach that asks for one and runs until its shell exits; connections come
//! and go. Any number may be attached at once -- two browser tabs, or a tab
//! refreshed while the old one is still closing -- and each sees the same
//! output and may type.
//!
//! One connection holds the size, as tmux's window-size latest has it: the
//! first to attach, then whichever last asked for it, as a window does when
//! someone types into it. Every connection says what size its window would
//! have; the holder's is the terminal's, and the others are told the
//! terminal's size, so they draw it as it is rather than wrapped to their
//! own, and whether they hold it. Watching, or a window opening, takes
//! nothing from whoever is using it.
//!
//! A connection that attaches is first sent the session's recent output, so
//! a refreshed page shows what was there. That is a replay of bytes, not a
//! picture of the screen: a full-screen program is also told the window
//! changed, which makes it draw itself again.
//!
//! Live output is passed through untouched, so everything a terminal on the
//! other end understands -- images, the kitty keyboard protocol, clipboard
//! and hyperlink sequences -- reaches it exactly as the program wrote it.
//!
//! A connection starts with one JSON line, a [`Request`], answered by one
//! JSON line, a [`Reply`]. After a successful attach both directions carry
//! frames: a type byte, a four-byte big-endian length, and that many bytes.

use std::collections::HashMap;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::clock::rfc3339;
use crate::fdio::FdStream;
use crate::user::{self, User};

/// The vsock port terminals are served on. The host dials it: a terminal is
/// opened when someone asks for one, not when the guest boots.
pub const PORT: u32 = 8101;

/// How much of a session's recent output is kept, to replay to a connection
/// that attaches.
pub const HISTORY_BYTES: usize = 2 << 20;

/// The largest payload a frame may carry.
pub const MAX_FRAME: usize = 1 << 20;

/// Names the session a process runs in, so what runs there can say which it
/// is, as a coding agent's hooks do (`agents`).
pub const TERMINAL_ENV: &str = "HANGAR_TERMINAL_SESSION";

/// Keystrokes and pastes for the shell.
pub const FRAME_INPUT: u8 = b'i';
/// Asks for the terminal's size, columns then rows, two bytes each. The
/// connection holds the size from then on.
pub const FRAME_RESIZE: u8 = b'r';
/// The size the connection's window would have, as FRAME_RESIZE has it,
/// which is the terminal's only while the connection holds the size.
pub const FRAME_PREFER: u8 = b'p';
/// The terminal's size, how many connections are attached and whether this
/// one holds the size ([`size_payload`]), sent on attaching and whenever any
/// of it changes.
pub const FRAME_SIZE: u8 = b's';
/// What the shell wrote.
pub const FRAME_OUTPUT: u8 = b'o';
/// The shell exited, with its status as four bytes: the connection's last
/// frame.
pub const FRAME_EXIT: u8 = b'x';

/// What a connection asks first.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Request {
    #[serde(default)]
    pub op: String,
    /// The session to attach to or close. Attaching with none starts one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub session: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cols: u16,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub rows: u16,
    /// Whose shell a new session is, the environment's usual user if none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub user: String,
    /// Where a new session's shell starts, the user's home if none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub dir: String,
}

/// One session, as a list or an attach describes it.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SessionInfo {
    pub id: String,
    /// What the shell or the program in it last set the window title to.
    pub title: String,
    /// When it started, in RFC 3339.
    pub created: String,
    /// How many connections are attached.
    pub clients: usize,
    pub cols: u16,
    pub rows: u16,
}

/// The answer to a [`Request`]: `err` if it failed, otherwise the session
/// attached to, or every session for a list.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Reply {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub err: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<SessionInfo>,
    /// For an attach, how many bytes of the output that follows replay the
    /// session's recent output, before the live output. A terminal must not
    /// answer what the replay asks of it -- its size, its version -- since
    /// the program that asked may be long gone, and the answer would be
    /// typed into whatever reads the input by then.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub history: usize,
    /// For an attach, whether this connection holds the size.
    #[serde(default, skip_serializing_if = "is_false")]
    pub owner: bool,
}

fn is_zero<T: Default + PartialEq>(n: &T) -> bool {
    *n == T::default()
}

fn is_false(b: &bool) -> bool {
    !b
}

/// Starts a new session's shell: the command for the request, which the
/// manager runs on the pseudo-terminal, leading a session of its own with
/// the terminal as its controlling one.
pub type Shell = Box<dyn Fn(&Request) -> io::Result<Command> + Send + Sync>;

/// A machine's sessions.
pub struct Manager {
    shell: Shell,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
}

impl Manager {
    pub fn new(shell: Shell) -> Arc<Self> {
        Arc::new(Self {
            shell,
            sessions: Mutex::new(HashMap::new()),
        })
    }

    /// Handles one connection: reads the request, answers it, and for an
    /// attach carries frames until either end goes away. The session
    /// outlives the connection.
    pub async fn serve<S>(self: &Arc<Self>, conn: S)
    where
        S: AsyncRead + AsyncWrite + Send + 'static,
    {
        let (rd, mut wr) = tokio::io::split(conn);
        let mut rd = BufReader::new(rd);
        let mut line = Vec::new();
        if !matches!(rd.read_until(b'\n', &mut line).await, Ok(n) if n > 0 && line.ends_with(b"\n"))
        {
            return;
        }
        let req: Request = match serde_json::from_slice(&line) {
            Ok(r) => r,
            Err(e) => {
                let _ = write_reply(&mut wr, &failed(format!("malformed request: {e}"))).await;
                return;
            }
        };
        let reply = match req.op.as_str() {
            "list" => Reply {
                sessions: self.list(),
                ..Reply::default()
            },
            "close" => match self.close(&req.session) {
                Ok(()) => Reply::default(),
                Err(e) => failed(e.to_string()),
            },
            "attach" => return self.attach(rd, wr, req).await,
            op => failed(format!("unknown operation {op:?}")),
        };
        let _ = write_reply(&mut wr, &reply).await;
    }

    /// Every session, oldest first.
    pub fn list(&self) -> Vec<SessionInfo> {
        let mut all: Vec<_> = self.sessions.lock().unwrap().values().cloned().collect();
        all.sort_by_key(|s| s.created);
        all.iter().map(|s| s.describe()).collect()
    }

    /// Ends a session: its shell is hung up on, as when a terminal window
    /// closes.
    pub fn close(&self, id: &str) -> io::Result<()> {
        let s = self.sessions.lock().unwrap().get(id).cloned();
        let s = s.ok_or_else(no_session)?;
        // The shell leads a session of its own, so its process ID is its
        // group's, and everything it started hears the hang-up too.
        // SAFETY: kill(2) of a process group.
        unsafe { libc::kill(-s.pid, libc::SIGHUP) };
        Ok(())
    }

    async fn attach<R, W>(self: &Arc<Self>, mut rd: R, mut wr: W, req: Request)
    where
        R: AsyncRead + Unpin + Send,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let s = match self.find(&req) {
            Ok(s) => s,
            Err(e) => {
                let _ = write_reply(&mut wr, &failed(e.to_string())).await;
                return;
            }
        };
        let id = s.next_client.fetch_add(1, Ordering::Relaxed);
        let (tx, mut rx) = mpsc::channel::<Frame>(256);
        let history = s.join(id, tx, req.cols, req.rows);
        let reply = Reply {
            session: Some(s.describe()),
            history: history.len(),
            owner: s.owns(id),
            ..Reply::default()
        };
        if write_reply(&mut wr, &reply).await.is_err() {
            s.leave(id);
            return;
        }

        // Output has a task of its own, so a slow reader never holds up the
        // session: it is fed from a bounded channel, and a connection that
        // lets the channel fill is dropped rather than stall everyone else.
        let writer_session = s.clone();
        let mut writer = tokio::spawn(async move {
            for chunk in history.chunks(64 << 10) {
                if write_frame(&mut wr, FRAME_OUTPUT, chunk).await.is_err() {
                    return;
                }
            }
            while let Some(f) = rx.recv().await {
                if f.typ == FRAME_EXIT {
                    let code = writer_session.state().code as u32;
                    let _ = write_frame(&mut wr, FRAME_EXIT, &code.to_be_bytes()).await;
                    return;
                }
                if write_frame(&mut wr, f.typ, &f.payload).await.is_err() {
                    return;
                }
            }
            // Dropped for falling behind: the connection closing tells the
            // other end, which attaches again.
        });

        let writer_done = tokio::select! {
            _ = read_frames(&mut rd, &s, id) => false,
            _ = &mut writer => true,
        };
        s.leave(id);
        if !writer_done {
            let _ = writer.await;
        }
    }

    /// The session a request names, or a new one if it names none.
    fn find(self: &Arc<Self>, req: &Request) -> io::Result<Arc<Session>> {
        if !req.session.is_empty() {
            let s = self.sessions.lock().unwrap().get(&req.session).cloned();
            return s.ok_or_else(no_session);
        }

        let mut cmd = (self.shell)(req)?;
        let sid = random_id();
        cmd.env(TERMINAL_ENV, &sid);
        let (cols, rows) = if req.cols == 0 || req.rows == 0 {
            (80, 24)
        } else {
            (req.cols, req.rows)
        };
        let (master, slave) = open_pty(cols, rows)?;
        cmd.stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave));
        // SAFETY: setsid and ioctl are async-signal-safe.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let program = cmd.get_program().to_string_lossy().into_owned();
        let mut cmd = tokio::process::Command::from(cmd);
        let child = cmd
            .spawn()
            .map_err(|e| io::Error::new(e.kind(), format!("starting the shell: {e}")))?;
        // Its copies of the terminal's other side go, so the terminal reads
        // as ended once the shell and what it started have closed theirs.
        drop(cmd);
        let s = Arc::new(Session {
            id: sid.clone(),
            created: SystemTime::now(),
            pid: child.id().unwrap_or(0) as i32,
            master: FdStream::new(master)?,
            write: tokio::sync::Mutex::new(()),
            next_client: AtomicU64::new(0),
            state: Mutex::new(State {
                cols,
                rows,
                title: String::new(),
                alt: false,
                history: Vec::new(),
                carry: Vec::new(),
                clients: HashMap::new(),
                owner: None,
                ended: false,
                code: 0,
            }),
        });
        self.sessions.lock().unwrap().insert(sid.clone(), s.clone());
        eprintln!("hangar-agent-rs: terminal session {sid} started: {program}");
        let manager = self.clone();
        tokio::spawn(s.clone().pump(child, move |code| {
            manager.sessions.lock().unwrap().remove(&sid);
            eprintln!("hangar-agent-rs: terminal session {sid} ended: {code}");
        }));
        Ok(s)
    }
}

fn no_session() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "no such session")
}

fn failed(err: String) -> Reply {
    Reply {
        err,
        ..Reply::default()
    }
}

async fn read_frames<R: AsyncRead + Unpin>(rd: &mut R, s: &Session, id: u64) {
    while let Ok((typ, payload)) = read_frame(rd).await {
        match typ {
            FRAME_INPUT => s.input(&payload).await,
            FRAME_RESIZE => {
                if let Some((cols, rows)) = parse_resize(&payload) {
                    s.claim(id, cols, rows);
                }
            }
            FRAME_PREFER => {
                if let Some((cols, rows)) = parse_resize(&payload) {
                    s.prefer(id, cols, rows);
                }
            }
            _ => {}
        }
    }
}

/// One frame for a connection. FRAME_EXIT's status is added as it is sent.
struct Frame {
    typ: u8,
    payload: Arc<[u8]>,
}

impl Frame {
    fn exit() -> Self {
        Frame {
            typ: FRAME_EXIT,
            payload: Arc::new([]),
        }
    }
}

/// One attached connection.
struct Client {
    /// Frames for the connection: output, sizes and the exit, in the order
    /// they happened.
    tx: mpsc::Sender<Frame>,
    /// The size the connection's window would have.
    cols: u16,
    rows: u16,
}

struct Session {
    id: String,
    created: SystemTime,
    pid: i32,
    master: FdStream,
    /// Keeps writes to the terminal whole, whichever connection sends them.
    write: tokio::sync::Mutex<()>,
    next_client: AtomicU64,
    state: Mutex<State>,
}

struct State {
    cols: u16,
    rows: u16,
    title: String,
    alt: bool,
    history: Vec<u8>,
    /// The end of the previous output, so a sequence split across two
    /// reads is still recognised.
    carry: Vec<u8>,
    clients: HashMap<u64, Client>,
    /// The connection whose window's size the terminal has; none once it
    /// has gone, until another takes the size.
    owner: Option<u64>,
    ended: bool,
    code: i32,
}

impl Session {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    fn describe(&self) -> SessionInfo {
        let st = self.state();
        SessionInfo {
            id: self.id.clone(),
            title: st.title.clone(),
            created: rfc3339(self.created),
            clients: st.clients.len(),
            cols: st.cols,
            rows: st.rows,
        }
    }

    /// Attaches a connection and returns the history to replay to it. Both
    /// happen under the lock, so no output falls between the replay and the
    /// live output, and none is sent twice. With nobody holding the size,
    /// the connection takes it.
    ///
    /// A full-screen program is told to draw itself again: by the size
    /// changing, or otherwise by a signal, since the replay is a record of
    /// bytes, and a program that has been redrawing one corner of the
    /// screen for an hour cannot be rebuilt from the last two megabytes of
    /// it.
    fn join(&self, id: u64, tx: mpsc::Sender<Frame>, cols: u16, rows: u16) -> Vec<u8> {
        let mut st = self.state();
        if st.ended {
            let _ = tx.try_send(Frame::exit());
            return st.history.clone();
        }
        st.clients.insert(id, Client { tx, cols, rows });
        let mut changed = false;
        if st.owner.is_none() && cols > 0 && rows > 0 {
            st.owner = Some(id);
            changed = self.set_size(&mut st, cols, rows);
        }
        if !changed && st.alt {
            self.signal_foreground();
        }
        announce(&st);
        st.history.clone()
    }

    fn leave(&self, id: u64) {
        let mut st = self.state();
        if st.clients.remove(&id).is_some() {
            if st.owner == Some(id) {
                st.owner = None;
            }
            announce(&st);
        }
    }

    fn owns(&self, id: u64) -> bool {
        self.state().owner == Some(id)
    }

    /// Types what a connection sent. It takes nothing: a terminal answers a
    /// program's queries, and reports focus and the mouse, of itself, in
    /// every window watching it, so input is no sign of anyone using it. A
    /// window someone types into asks for the size (claim) as they do.
    async fn input(&self, p: &[u8]) {
        let _whole = self.write.lock().await;
        let _ = self.master.write_all(p).await;
    }

    /// Gives a connection the size, at the size it asks for.
    fn claim(&self, id: u64, cols: u16, rows: u16) {
        if cols == 0 || rows == 0 {
            return;
        }
        let mut st = self.state();
        let Some(c) = st.clients.get_mut(&id) else {
            return;
        };
        (c.cols, c.rows) = (cols, rows);
        st.owner = Some(id);
        self.set_size(&mut st, cols, rows);
        announce(&st);
    }

    /// Records the size a connection's window would have, which is the
    /// terminal's if the connection holds the size.
    fn prefer(&self, id: u64, cols: u16, rows: u16) {
        if cols == 0 || rows == 0 {
            return;
        }
        let mut st = self.state();
        if let Some(c) = st.clients.get_mut(&id) {
            (c.cols, c.rows) = (cols, rows);
        }
        if st.owner == Some(id) && self.set_size(&mut st, cols, rows) {
            announce(&st);
        }
    }

    /// Sets the terminal's size, saying whether it changed; the kernel tells
    /// the program in it.
    fn set_size(&self, st: &mut State, cols: u16, rows: u16) -> bool {
        if st.ended || (st.cols == cols && st.rows == rows) {
            return false;
        }
        (st.cols, st.rows) = (cols, rows);
        let ws = winsize(cols, rows);
        // SAFETY: TIOCSWINSZ with a winsize, on the terminal this session
        // owns.
        unsafe {
            libc::ioctl(
                self.master.raw(),
                libc::TIOCSWINSZ as _,
                &ws as *const libc::winsize,
            )
        };
        true
    }

    /// Tells whatever program has the terminal that its window changed,
    /// which a full-screen program answers by drawing itself again.
    fn signal_foreground(&self) {
        let mut pgrp: libc::pid_t = 0;
        // SAFETY: TIOCGPGRP into a pid_t, then kill(2) of that group.
        unsafe {
            if libc::ioctl(
                self.master.raw(),
                libc::TIOCGPGRP as _,
                &mut pgrp as *mut libc::pid_t,
            ) == 0
                && pgrp > 0
            {
                libc::kill(-pgrp, libc::SIGWINCH);
            }
        }
    }

    /// Copies the terminal's output to the history and every connection
    /// until the shell exits.
    async fn pump(self: Arc<Self>, mut child: tokio::process::Child, ended: impl FnOnce(i32)) {
        let mut buf = vec![0; 32 << 10];
        loop {
            match self.master.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => self.output(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                // EIO, once every holder of the terminal's other side has
                // closed it.
                Err(_) => break,
            }
        }
        let code = match child.wait().await {
            Ok(status) => status.code().unwrap_or(-1),
            Err(_) => -1,
        };
        // Gone from the list before anyone hears it ended, so nobody told
        // can attach to it again.
        ended(code);
        let mut st = self.state();
        st.ended = true;
        st.code = code;
        for c in st.clients.values() {
            let _ = c.tx.try_send(Frame::exit());
        }
    }

    fn output(&self, p: &[u8]) {
        let mut st = self.state();
        track(&mut st, p);
        st.history.extend_from_slice(p);
        if st.history.len() > HISTORY_BYTES {
            // Cut at a line's end, so the replay does not start halfway
            // through an escape sequence and print its tail as text.
            let over = st.history.len() - HISTORY_BYTES;
            let cut = match st.history[over..].iter().position(|&b| b == b'\n') {
                Some(i) => over + i + 1,
                None => over,
            };
            st.history.drain(..cut);
        }
        let payload: Arc<[u8]> = p.into();
        let mut behind = vec![];
        for (&id, c) in &st.clients {
            let frame = Frame {
                typ: FRAME_OUTPUT,
                payload: payload.clone(),
            };
            if c.tx.try_send(frame).is_err() {
                behind.push(id);
            }
        }
        // Too far behind to catch up without holding up everyone else: its
        // connection is closed, and a refresh attaches again.
        if !behind.is_empty() {
            for id in behind {
                st.clients.remove(&id);
                if st.owner == Some(id) {
                    st.owner = None;
                }
            }
            announce(&st);
        }
    }
}

/// Tells every connection the terminal's size, how many are attached, and
/// whether it holds the size.
fn announce(st: &State) {
    for (&id, c) in &st.clients {
        let p = size_payload(st.cols, st.rows, st.clients.len(), st.owner == Some(id));
        // A connection this far behind is dropped by the output shortly.
        let _ = c.tx.try_send(Frame {
            typ: FRAME_SIZE,
            payload: p.into(),
        });
    }
}

const ALT_ON: [&[u8]; 3] = [b"\x1b[?1049h", b"\x1b[?1047h", b"\x1b[?47h"];
const ALT_OFF: [&[u8]; 3] = [b"\x1b[?1049l", b"\x1b[?1047l", b"\x1b[?47l"];

/// Follows the window title, and whether a full-screen program has the
/// alternate screen, from the output as it passes.
fn track(st: &mut State, p: &[u8]) {
    let mut buf = std::mem::take(&mut st.carry);
    buf.extend_from_slice(p);
    let (on, off) = (last_of(&buf, &ALT_ON), last_of(&buf, &ALT_OFF));
    if on > off {
        st.alt = true;
    } else if off > on {
        st.alt = false;
    }
    // OSC 0 or 2: ESC ] 0 ; title BEL, or ESC ] 2 ; title ST.
    for start in [b"\x1b]0;", b"\x1b]2;"] {
        if let Some(i) = last_index(&buf, start) {
            let rest = &buf[i + start.len()..];
            if let Some(end) = rest.iter().position(|&b| b == 0x07 || b == 0x1b)
                && end <= 256
            {
                st.title = String::from_utf8_lossy(&rest[..end]).into_owned();
            }
        }
    }
    if buf.len() > 300 {
        buf.drain(..buf.len() - 300);
    }
    st.carry = buf;
}

fn last_index(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).rposition(|w| w == needle)
}

fn last_of(hay: &[u8], needles: &[&[u8]]) -> Option<usize> {
    needles.iter().filter_map(|n| last_index(hay, n)).max()
}

/// A new session's shell: a login shell for the request's user, or the
/// environment's usual user, or whoever runs the agent if there is no such
/// user here. It is given the environment a terminal program would set --
/// TERM and COLORTERM, so programs use colour and what the browser's
/// terminal can do -- and nothing of the agent's own.
pub fn login_shell(req: &Request) -> io::Result<Command> {
    let name = if req.user.is_empty() {
        user::DEFAULT_USER
    } else {
        &req.user
    };
    let u = match User::lookup(name) {
        Some(u) => u,
        None if !req.user.is_empty() => {
            return Err(io::Error::other(format!("no user {:?}", req.user)));
        }
        None => User::current(),
    };
    let shell = u.login_shell();
    let dir = Some(PathBuf::from(&req.dir))
        .filter(|d| !req.dir.is_empty() && d.is_dir())
        .unwrap_or_else(|| u.home.clone());
    let mut cmd = Command::new(&shell);
    // argv[0] starting with "-" is how a shell knows it is a login shell,
    // and so reads the profile that sets PATH and the prompt.
    let base = Path::new(&shell)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    cmd.arg0(format!("-{base}"))
        .current_dir(dir)
        .env_clear()
        .env("HOME", &u.home)
        .env("SSH_AUTH_SOCK", user::SSH_AUTH_SOCK)
        .env("USER", &u.name)
        .env("LOGNAME", &u.name)
        .env("SHELL", &shell)
        .env("PATH", user::PATH)
        .env("TERM", "xterm-256color")
        .env("COLORTERM", "truecolor")
        .env("TERM_PROGRAM", "hangar")
        .env("LANG", "C.UTF-8")
        .envs(user::session_env());
    user::prepare(&mut cmd, Some(&u), user::oom::USER);
    Ok(cmd)
}

pub(crate) fn winsize(cols: u16, rows: u16) -> libc::winsize {
    libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

/// A new pseudo-terminal of the size given: its master, and the other side
/// for the shell.
pub(crate) fn open_pty(cols: u16, rows: u16) -> io::Result<(OwnedFd, OwnedFd)> {
    let (mut master, mut slave) = (0, 0);
    let mut ws = winsize(cols, rows);
    // SAFETY: openpty fills in two descriptors, which are then owned here,
    // and both are kept from what the shell inherits.
    unsafe {
        if libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            // Linux takes a const pointer, macOS a mutable one.
            &raw mut ws,
        ) < 0
        {
            return Err(io::Error::last_os_error());
        }
        let (master, slave) = (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave));
        for fd in [&master, &slave] {
            libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
        }
        Ok((master, slave))
    }
}

/// Twelve random hex digits.
fn random_id() -> String {
    let mut b = [0u8; 6];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut b);
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

async fn write_reply<W: AsyncWrite + Unpin>(w: &mut W, r: &Reply) -> io::Result<()> {
    let mut b = serde_json::to_vec(r).map_err(io::Error::other)?;
    b.push(b'\n');
    w.write_all(&b).await
}

/// Writes one frame.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    w: &mut W,
    typ: u8,
    payload: &[u8],
) -> io::Result<()> {
    if payload.len() > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "terminal frame too large",
        ));
    }
    let mut b = Vec::with_capacity(5 + payload.len());
    b.push(typ);
    b.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    b.extend_from_slice(payload);
    w.write_all(&b).await
}

/// Reads one frame.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<(u8, Vec<u8>)> {
    let mut hdr = [0u8; 5];
    r.read_exact(&mut hdr).await?;
    let n = u32::from_be_bytes([hdr[1], hdr[2], hdr[3], hdr[4]]) as usize;
    if n > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "terminal frame too large",
        ));
    }
    let mut payload = vec![0; n];
    r.read_exact(&mut payload).await?;
    Ok((hdr[0], payload))
}

/// A size for FRAME_RESIZE or FRAME_PREFER.
pub fn resize_payload(cols: u16, rows: u16) -> [u8; 4] {
    let mut b = [0; 4];
    b[..2].copy_from_slice(&cols.to_be_bytes());
    b[2..].copy_from_slice(&rows.to_be_bytes());
    b
}

/// A FRAME_RESIZE or FRAME_PREFER payload's columns and rows.
pub fn parse_resize(b: &[u8]) -> Option<(u16, u16)> {
    let b: &[u8; 4] = b.try_into().ok()?;
    Some((
        u16::from_be_bytes([b[0], b[1]]),
        u16::from_be_bytes([b[2], b[3]]),
    ))
}

/// A FRAME_SIZE payload: columns, rows and connections, two bytes each,
/// then one byte, 1 if the connection holds the size.
pub fn size_payload(cols: u16, rows: u16, clients: usize, owner: bool) -> [u8; 7] {
    let mut b = [0; 7];
    b[..2].copy_from_slice(&cols.to_be_bytes());
    b[2..4].copy_from_slice(&rows.to_be_bytes());
    b[4..6].copy_from_slice(&(clients.min(u16::MAX as usize) as u16).to_be_bytes());
    b[6] = owner as u8;
    b
}

/// A FRAME_SIZE payload's columns, rows, connections and whether the
/// connection holds the size.
pub fn parse_size(b: &[u8]) -> Option<(u16, u16, usize, bool)> {
    let b: &[u8; 7] = b.try_into().ok()?;
    Some((
        u16::from_be_bytes([b[0], b[1]]),
        u16::from_be_bytes([b[2], b[3]]),
        u16::from_be_bytes([b[4], b[5]]) as usize,
        b[6] == 1,
    ))
}
