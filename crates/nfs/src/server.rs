//! The server: its clients, sessions and states, and a connection's calls,
//! each COMPOUND's operations run in turn as its caller.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime};

use crate::attr::LEASE_TIME;
use crate::fs::{Fd, Handles, Root, errno, split};
use crate::proto::*;
use crate::rpc::{self, Call, Cred, NOBODY};
use crate::sys::{self, User};
use crate::xdr::{Reader, Writer};

/// What one environment may reach: its file sets, each a directory in the
/// server's root named by the set's ID, and everything in them but the
/// files it is not trusted with.
pub trait View: Send + Sync {
    /// The IDs of the file sets the environment is given.
    fn sets(&self) -> Vec<String>;
    /// Whether a file in a set, by its path in the set, is kept from the
    /// environment.
    fn hidden(&self, set: &str, path: &[u8]) -> bool;
    /// Who a set's callers act as.
    fn squash(&self, set: &str) -> Squash;
}

/// Who a set's callers act as, as an NFS export's options say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Squash {
    /// Every caller is the server's user (Config's uid and gid), root among
    /// them; changing an owner is accepted and changes nothing.
    All,
    /// Root is nobody, and every other caller itself.
    Root,
    /// Every caller is itself, root included.
    None,
}

/// A server's.
pub struct Config<'a> {
    /// Holds the file sets, each a directory named by its ID.
    pub root: &'a Path,
    pub view: Arc<dyn View>,
    /// Who a set's callers act as when it squashes all of them: the
    /// environments' user.
    pub uid: u32,
    pub gid: u32,
}

/// As many calls at once as a session has slots.
pub const MAX_SLOTS: u32 = 64;
/// Bounds a reply kept for a retransmission.
const MAX_CACHED: usize = 64 << 10;

pub(crate) struct Client {
    pub id: u64,
    pub owner: Vec<u8>,
    pub verifier: [u8; 8],
    /// The last CREATE_SESSION's sequence, and its reply.
    pub seq: u32,
    pub reply: Option<Vec<u8>>,
    pub confirmed: bool,
    pub renewed: Instant,
}

pub(crate) struct Session {
    pub client: u64,
    pub slots: Vec<Slot>,
    pub max_ops: u32,
}

/// A session's slot: one compound at a time, its last reply kept for a
/// retransmission.
#[derive(Default)]
pub(crate) struct Slot {
    pub state: Mutex<SlotState>,
    pub free: Condvar,
}

#[derive(Default)]
pub(crate) struct SlotState {
    pub busy: bool,
    pub seq: u32,
    /// The last compound's whole reply, when it was cached.
    pub reply: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Open,
    Lock,
}

/// An open of a file, or a lock owner's locks on one.
pub(crate) struct State {
    pub seqid: u32,
    pub kind: Kind,
    pub client: u64,
    /// The open or lock owner.
    pub owner: Vec<u8>,
    pub path: Vec<u8>,
    /// An open's share access.
    pub access: u32,
    /// The open's, or the lock owner's, description. A READ or WRITE
    /// through one the open gives up for another keeps it open until it is
    /// done.
    pub file: Arc<Fd>,
    /// A lock's open.
    pub parent: Option<[u8; 12]>,
}

#[derive(Default)]
pub(crate) struct Inner {
    pub next_id: u64,
    pub clients: HashMap<u64, Client>,
    pub by_owner: HashMap<Vec<u8>, u64>,
    pub sessions: HashMap<[u8; 16], Arc<Session>>,
    pub states: HashMap<[u8; 12], State>,
    /// The verifiers of exclusive creates, by path.
    pub excl: HashMap<Vec<u8>, [u8; 8]>,
    pub closed: bool,
}

/// Serves one environment's view of the root.
pub struct Server {
    pub(crate) view: Arc<dyn View>,
    pub(crate) uid: u32,
    pub(crate) gid: u32,
    /// The server's own identity, when it does not run as root (a worker on
    /// macOS): it cannot act as anyone else, so every caller is squashed,
    /// and what it owns is the squashed user's to its clients.
    pub(crate) self_user: Option<User>,
    pub(crate) root: Root,
    pub(crate) h: Handles,
    pub(crate) started: (i64, u32),
    pub(crate) verf: [u8; 8],
    pub(crate) inner: Mutex<Inner>,
    done: Arc<(Mutex<bool>, Condvar)>,
}

pub(crate) fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    let mut f = std::fs::File::open("/dev/urandom").expect("/dev/urandom");
    f.read_exact(&mut b).expect("/dev/urandom");
    b
}

/// A COMPOUND's state: its current and saved files, and stateid.
#[derive(Default)]
pub(crate) struct Cstate {
    pub cred: Option<Cred>,
    pub cur: Vec<u8>,
    pub save: Vec<u8>,
    pub has_cur: bool,
    pub has_save: bool,
    pub cur_state: Stateid,
    pub session: Option<Arc<Session>>,
    pub slot: Option<usize>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Stateid {
    pub seqid: u32,
    pub other: [u8; 12],
}

pub(crate) fn read_stateid(r: &mut Reader) -> Stateid {
    let seqid = r.u32();
    let mut other = [0u8; 12];
    other.copy_from_slice(&{
        let f = r.fixed(12);
        if f.len() == 12 {
            f.to_vec()
        } else {
            vec![0; 12]
        }
    });
    Stateid { seqid, other }
}

pub(crate) fn write_stateid(w: &mut Writer, id: Stateid) {
    w.u32(id.seqid);
    w.fixed(&id.other);
}

/// A stream a client is served on: read on one thread while replies are
/// written on others.
pub trait Stream: Read + Write + Send + Sized + 'static {
    fn try_clone(&self) -> io::Result<Self>;
    /// Ends the stream both ways, so a read waiting on it returns.
    fn shutdown(&self);
}

impl Stream for std::os::unix::net::UnixStream {
    fn try_clone(&self) -> io::Result<Self> {
        std::os::unix::net::UnixStream::try_clone(self)
    }

    fn shutdown(&self) {
        let _ = std::os::unix::net::UnixStream::shutdown(self, std::net::Shutdown::Both);
    }
}

impl Server {
    /// Starts a server for cfg's view, rooted at cfg's root.
    pub fn new(cfg: Config<'_>) -> io::Result<Arc<Server>> {
        use std::os::unix::ffi::OsStrExt;
        let fd = sys::open_root(cfg.root.as_os_str().as_bytes()).map_err(|e| {
            io::Error::new(e.kind(), format!("opening {}: {e}", cfg.root.display()))
        })?;
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
        let boot: [u8; 8] = random();
        let self_user = (!sys::privileged()).then(|| User {
            // SAFETY: geteuid and getegid have no preconditions.
            uid: unsafe { libc::geteuid() },
            gid: unsafe { libc::getegid() },
            groups: vec![],
        });
        let s = Arc::new(Server {
            view: cfg.view,
            uid: cfg.uid,
            gid: cfg.gid,
            self_user,
            root: Root { fd },
            h: Handles::new(boot),
            started: (now.as_secs() as i64, now.subsec_nanos()),
            verf: boot,
            inner: Mutex::new(Inner {
                next_id: now.as_secs() << 32,
                ..Default::default()
            }),
            done: Arc::default(),
        });
        let weak = Arc::downgrade(&s);
        let done = s.done.clone();
        std::thread::Builder::new()
            .name("nfs-expire".into())
            .spawn(move || expire(weak, done))?;
        Ok(s)
    }

    /// Ends every client's state, closing its files and so letting go of
    /// its locks. Connections still served are the caller's to close.
    pub fn close(&self) {
        let mut inner = self.inner.lock().unwrap();
        if inner.closed {
            return;
        }
        inner.closed = true;
        let ids: Vec<u64> = inner.clients.keys().copied().collect();
        for id in ids {
            drop_client(&mut inner, id);
        }
        drop(inner);
        let (lock, cv) = &*self.done;
        *lock.lock().unwrap() = true;
        cv.notify_all();
    }

    /// Answers calls on stream until it fails or closes. Calls are answered
    /// as they finish, not in order, as RPC allows, as many at once as a
    /// session has slots.
    pub fn serve<S: Stream>(self: &Arc<Self>, mut stream: S) -> io::Result<()> {
        let writer = Arc::new(Mutex::new(stream.try_clone()?));
        let failed = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let rx = Arc::new(Mutex::new(rx));
        let busy = Arc::new(AtomicUsize::new(0));
        let mut workers: Vec<std::thread::JoinHandle<()>> = vec![];
        let result = (|| loop {
            let Some(rec) = rpc::read_record(&mut stream)? else {
                return Ok(());
            };
            if failed.load(Ordering::Relaxed) {
                return Ok(());
            }
            // Another worker when every one is busy, up to the most calls
            // at once.
            if busy.load(Ordering::SeqCst) >= workers.len() && workers.len() < MAX_SLOTS as usize {
                let (s, rx, writer, failed, busy) = (
                    self.clone(),
                    rx.clone(),
                    writer.clone(),
                    failed.clone(),
                    busy.clone(),
                );
                workers.push(std::thread::spawn(move || {
                    loop {
                        let Ok(rec) = rx.lock().unwrap().recv() else {
                            return;
                        };
                        let reply = s.answer_record(&rec);
                        busy.fetch_sub(1, Ordering::SeqCst);
                        let mut w = writer.lock().unwrap();
                        if rpc::write_record(&mut *w, &reply).is_err() {
                            failed.store(true, Ordering::Relaxed);
                            w.shutdown();
                        }
                    }
                }));
            }
            busy.fetch_add(1, Ordering::SeqCst);
            if tx.send(rec).is_err() {
                return Ok(());
            }
        })();
        drop(tx);
        for w in workers {
            let _ = w.join();
        }
        result
    }

    /// A record's whole reply: a call answered, or one that does not parse
    /// refused.
    pub fn answer_record(&self, rec: &[u8]) -> Vec<u8> {
        match rpc::parse_call(rec) {
            Ok(c) => self.answer(c),
            Err(_) => {
                let xid = rec
                    .get(..4)
                    .map_or(0, |b| u32::from_be_bytes(b.try_into().unwrap()));
                let mut w = rpc::reply_header(xid);
                rpc::accepted(&mut w, rpc::ACCEPT_GARBAGE_ARGS);
                w.b
            }
        }
    }

    /// A call's whole reply.
    fn answer(&self, c: Call<'_>) -> Vec<u8> {
        let mut w = rpc::reply_header(c.xid);
        if let Some(why) = c.reject {
            rpc::reject(&mut w, why);
        } else if c.proc_ == rpc::PROC_NULL {
            rpc::accepted(&mut w, rpc::ACCEPT_SUCCESS);
        } else {
            match self.compound(&c) {
                None => rpc::accepted(&mut w, rpc::ACCEPT_GARBAGE_ARGS),
                // The reply to the same call, made before: the header's xid
                // is the retransmission's, the same.
                Some(Compound::Replay(r)) => {
                    rpc::accepted(&mut w, rpc::ACCEPT_SUCCESS);
                    w.b.extend_from_slice(&r);
                }
                Some(Compound::Body(b)) => {
                    rpc::accepted(&mut w, rpc::ACCEPT_SUCCESS);
                    w.b.extend_from_slice(&b);
                }
            }
        }
        w.b
    }

    /// Runs a COMPOUND's operations in turn, until one fails.
    fn compound(&self, c: &Call<'_>) -> Option<Compound> {
        let mut r = Reader::new(c.args);
        let tag = r.opaque(1024).to_vec();
        let minor = r.u32();
        let mut n = r.count(1 << 16);
        if r.failed {
            return None;
        }
        let mut cs = Cstate {
            cred: Some(c.cred.clone()),
            ..Default::default()
        };
        let mut res = Writer::default();
        let mut status = NFS_OK;
        let mut done = 0u32;
        if minor != 1 && minor != 2 {
            status = ERR_MINOR_VERS_MIS;
            n = 0;
        }
        for i in 0..n {
            let mut op = r.u32();
            if r.failed {
                status = ERR_BAD_XDR;
                break;
            }
            if i == 0 && op != OP_SEQUENCE && !sessionless(op) {
                status = ERR_OP_NOT_IN_SESSION;
            } else if i > 0 && sessionless(op) && op != OP_DESTROY_SESSION {
                status = ERR_NOT_ONLY_OP;
            } else if cs.session.as_ref().is_some_and(|s| i as u32 >= s.max_ops) {
                status = ERR_TOO_MANY_OPS;
            }
            let mut body = Writer::default();
            if status == NFS_OK {
                let caller = self.caller(&cs);
                let (st, replay) = sys::act_as(&caller, || self.op(&mut cs, op, &mut r, &mut body));
                if let Some(replay) = replay {
                    return Some(Compound::Replay(replay));
                }
                status = st;
                if r.failed && status == NFS_OK {
                    status = ERR_BAD_XDR;
                }
            }
            if !known(op) {
                op = OP_ILLEGAL;
            }
            res.u32(op);
            res.u32(status);
            res.b.extend_from_slice(&body.b);
            done += 1;
            if status != NFS_OK {
                break;
            }
        }
        let mut out = Writer::default();
        out.u32(status);
        out.opaque(&tag);
        out.u32(done);
        out.b.extend_from_slice(&res.b);
        if let (Some(se), Some(slot)) = (&cs.session, cs.slot) {
            // Kept for a retransmission, which gets this same reply.
            let sl = &se.slots[slot];
            let mut st = sl.state.lock().unwrap();
            st.reply = (out.b.len() <= MAX_CACHED).then(|| out.b.clone());
            st.busy = false;
            sl.free.notify_all();
        }
        Some(Compound::Body(out.b))
    }

    /// A fresh client or session ID's part.
    pub(crate) fn new_id(inner: &mut Inner) -> u64 {
        inner.next_id += 1;
        inner.next_id
    }

    /// Who a compound's operation acts as: its caller, squashed as the set
    /// it works in says, or as every caller is in all of them.
    fn caller(&self, cs: &Cstate) -> User {
        let mut set = split(&cs.cur).0;
        if set.is_empty() {
            set = split(&cs.save).0;
        }
        let cred = cs.cred.clone().unwrap_or(NOBODY);
        match self.squash(set) {
            Squash::None => User {
                uid: cred.uid,
                gid: cred.gid,
                groups: cred.gids,
            },
            Squash::Root if cred.uid == 0 => User {
                uid: NOBODY.uid,
                gid: NOBODY.gid,
                groups: vec![],
            },
            Squash::Root => User {
                uid: cred.uid,
                gid: cred.gid,
                groups: cred.gids,
            },
            Squash::All => User {
                uid: self.uid,
                gid: self.gid,
                groups: vec![self.gid],
            },
        }
    }

    /// How a set the environment is given squashes its callers.
    pub(crate) fn squash(&self, set: &[u8]) -> Squash {
        let Ok(set) = std::str::from_utf8(set) else {
            return Squash::All;
        };
        if set.is_empty() || self.self_user.is_some() || !self.view.sets().iter().any(|s| s == set)
        {
            return Squash::All;
        }
        self.view.squash(set)
    }

    /// Why a path may not be reached, or NFS_OK: its set is not the
    /// environment's, or the file is hidden from it.
    pub(crate) fn visible(&self, p: &[u8]) -> u32 {
        if p.is_empty() {
            return NFS_OK;
        }
        let (set, rest) = split(p);
        let Ok(set) = std::str::from_utf8(set) else {
            return ERR_NO_ENT;
        };
        if !self.view.sets().iter().any(|s| s == set) {
            return ERR_NO_ENT;
        }
        if !rest.is_empty() && self.view.hidden(set, rest) {
            return ERR_NO_ENT;
        }
        NFS_OK
    }

    /// Makes a set's directory if it is not there yet: a set is the
    /// environment's from when it is given, whether or not it has files.
    /// The server makes it, as no caller may make one in the root, and
    /// gives it to the environments' user.
    pub(crate) fn ensure_set(&self, set: &[u8]) -> io::Result<()> {
        let c = crate::fs::cstr(set)?;
        sys::act_as(&sys::root(), || {
            // SAFETY: c is a valid NUL-terminated name.
            if unsafe { libc::mkdirat(self.root.fd, c.as_ptr(), 0o755) } < 0 {
                let e = io::Error::last_os_error();
                return if e.raw_os_error() == Some(libc::EEXIST) {
                    Ok(())
                } else {
                    Err(e)
                };
            }
            // A server not run as root cannot, and leaves it its own.
            if unsafe {
                libc::fchownat(
                    self.root.fd,
                    c.as_ptr(),
                    self.uid,
                    self.gid,
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } < 0
            {
                let e = io::Error::last_os_error();
                if e.raw_os_error() != Some(libc::EPERM) {
                    return Err(e);
                }
            }
            Ok(())
        })
    }

    pub(crate) fn status(e: &io::Error) -> u32 {
        errno(e)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // SAFETY: the root's descriptor is this server's alone.
        unsafe { libc::close(self.root.fd) };
    }
}

pub(crate) enum Compound {
    Body(Vec<u8>),
    Replay(Vec<u8>),
}

/// The operations that may come without a SEQUENCE before them, alone in
/// their compound.
fn sessionless(op: u32) -> bool {
    matches!(
        op,
        OP_EXCHANGE_ID
            | OP_CREATE_SESSION
            | OP_DESTROY_SESSION
            | OP_BIND_CONN_TO_SESSION
            | OP_DESTROY_CLIENT_ID
    )
}

pub(crate) fn known(op: u32) -> bool {
    (OP_ACCESS..=75).contains(&op)
}

/// Ends a client: its sessions, opens and locks.
pub(crate) fn drop_client(inner: &mut Inner, id: u64) {
    inner.states.retain(|_, st| st.client != id);
    inner.sessions.retain(|_, se| se.client != id);
    if let Some(c) = inner.clients.remove(&id)
        && inner.by_owner.get(&c.owner) == Some(&id)
    {
        inner.by_owner.remove(&c.owner);
    }
}

/// Ends the state of clients that stopped renewing it.
fn expire(server: std::sync::Weak<Server>, done: Arc<(Mutex<bool>, Condvar)>) {
    let (lock, cv) = &*done;
    loop {
        let stop = lock.lock().unwrap();
        let (stop, _) = cv.wait_timeout(stop, LEASE_TIME / 3).unwrap();
        if *stop {
            return;
        }
        drop(stop);
        let Some(s) = server.upgrade() else {
            return;
        };
        let mut inner = s.inner.lock().unwrap();
        let gone: Vec<u64> = inner
            .clients
            .values()
            .filter(|c| c.renewed.elapsed() > LEASE_TIME * 3 / 2)
            .map(|c| c.id)
            .collect();
        for id in gone {
            drop_client(&mut inner, id);
        }
    }
}

/// How long a compound waits for its slot to be free: a client sends one
/// at a time on a slot, but a retransmission may race its original.
pub(crate) const SLOT_WAIT: Duration = Duration::from_secs(30);
