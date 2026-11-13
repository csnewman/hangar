//! The server spoken to as Linux's client speaks to it: NFSv4.1 records on
//! a stream, one call at a time.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hangar_nfs::{Config, Server, Squash, View};

/// A fixed view.
#[derive(Default)]
struct Fixed {
    sets: Vec<String>,
    /// "set/path"
    hidden: HashSet<String>,
    squash: HashMap<String, Squash>,
}

impl View for Fixed {
    fn sets(&self) -> Vec<String> {
        self.sets.clone()
    }

    fn hidden(&self, set: &str, path: &[u8]) -> bool {
        self.hidden
            .contains(&format!("{set}/{}", String::from_utf8_lossy(path)))
    }

    fn squash(&self, set: &str) -> Squash {
        self.squash.get(set).copied().unwrap_or(Squash::All)
    }
}

fn sets(names: &[&str]) -> Fixed {
    Fixed {
        sets: names.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

/// XDR, as a client sends it.
#[derive(Default)]
struct Enc {
    b: Vec<u8>,
}

impl Enc {
    fn u32(&mut self, v: u32) -> &mut Self {
        self.b.extend_from_slice(&v.to_be_bytes());
        self
    }

    fn u64(&mut self, v: u64) -> &mut Self {
        self.b.extend_from_slice(&v.to_be_bytes());
        self
    }

    fn fixed(&mut self, v: &[u8]) -> &mut Self {
        self.b.extend_from_slice(v);
        while !self.b.len().is_multiple_of(4) {
            self.b.push(0);
        }
        self
    }

    fn opaque(&mut self, v: &[u8]) -> &mut Self {
        self.u32(v.len() as u32).fixed(v)
    }

    fn str(&mut self, v: &str) -> &mut Self {
        self.opaque(v.as_bytes())
    }
}

/// XDR, as a client reads it.
struct Dec {
    b: Vec<u8>,
    at: usize,
}

impl Dec {
    fn u32(&mut self) -> u32 {
        let b = self.b.get(self.at..self.at + 4).expect("a reply too short");
        self.at += 4;
        u32::from_be_bytes(b.try_into().unwrap())
    }

    fn u64(&mut self) -> u64 {
        (self.u32() as u64) << 32 | self.u32() as u64
    }

    fn fixed(&mut self, n: usize) -> Vec<u8> {
        let p = n.div_ceil(4) * 4;
        let b = self.b.get(self.at..self.at + p).expect("a reply too short");
        self.at += p;
        b[..n].to_vec()
    }

    fn opaque(&mut self) -> Vec<u8> {
        let n = self.u32() as usize;
        self.fixed(n)
    }

    fn bool(&mut self) -> bool {
        self.u32() != 0
    }
}

/// A server on a fresh root, closed when the test ends.
struct Fixture {
    srv: Arc<Server>,
    root: PathBuf,
}

impl Fixture {
    fn new(name: &str, v: Fixed) -> Fixture {
        let root = scratch(name);
        let srv = Server::new(Config {
            root: &root,
            view: Arc::new(v),
            uid: 1000,
            gid: 1000,
        })
        .unwrap();
        Fixture { srv, root }
    }

    fn path(&self, p: &str) -> PathBuf {
        self.root.join(p)
    }

    /// Makes a set's directory as the server would: the environments'
    /// user's, where the test is run as root to act as them.
    fn make_set(&self, set: &str) {
        let d = self.path(set);
        std::fs::create_dir_all(&d).unwrap();
        let _ = std::os::unix::fs::chown(&d, Some(1000), Some(1000));
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.srv.close();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hangar-nfs-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Speaks NFSv4.1 to a server over a socket pair, one call at a time.
struct Client {
    conn: UnixStream,
    xid: u32,
    session: Vec<u8>,
    seq: u32,
    id: u64,
    /// Who calls are from, as AUTH_SYS says: 1000 unless set, or root
    /// with as_root.
    uid: u32,
    as_root: bool,
}

fn connect(f: &Fixture, name: &str) -> Client {
    let (a, b) = UnixStream::pair().unwrap();
    let srv = f.srv.clone();
    std::thread::spawn(move || {
        let _ = srv.serve(b);
    });
    let mut c = Client {
        conn: a,
        xid: 0,
        session: vec![],
        seq: 0,
        id: 0,
        uid: 0,
        as_root: false,
    };

    // EXCHANGE_ID, then CREATE_SESSION.
    let mut ops = Enc::default();
    ops.u32(42).fixed(&[0; 8]).str(name).u32(0).u32(0).u32(0);
    let mut d = c.compound(&ops.b, 1);
    expect_ok(&mut d, 42);
    c.id = d.u64();
    let seq = d.u32();

    let mut ops = Enc::default();
    ops.u32(43).u64(c.id).u32(seq).u32(0);
    for _ in 0..2 {
        // The fore and back channels' attributes.
        ops.u32(0)
            .u32(1 << 20)
            .u32(1 << 20)
            .u32(4096)
            .u32(16)
            .u32(8)
            .u32(0);
    }
    ops.u32(0x4000_0000).u32(1).u32(0); // callback program; AUTH_NONE
    let mut d = c.compound(&ops.b, 1);
    expect_ok(&mut d, 43);
    c.session = d.fixed(16);
    c
}

impl Client {
    /// A call's whole record, as sent.
    fn call(&self, xid: u32, ops: &[u8], n: u32) -> Vec<u8> {
        let mut call = Enc::default();
        call.u32(xid).u32(0).u32(2).u32(100003).u32(4).u32(1);
        // AUTH_SYS as the caller, and no verifier.
        let uid = if self.uid == 0 && !self.as_root {
            1000
        } else {
            self.uid
        };
        let mut cred = Enc::default();
        cred.u32(0).str("test").u32(uid).u32(uid).u32(0);
        call.u32(1).opaque(&cred.b).u32(0).u32(0);
        call.str("").u32(1).u32(n);
        call.b.extend_from_slice(ops);
        let mut rec = (1u32 << 31 | call.b.len() as u32).to_be_bytes().to_vec();
        rec.extend_from_slice(&call.b);
        rec
    }

    /// Sends a record and reads the reply's.
    fn send(&mut self, rec: &[u8]) -> Vec<u8> {
        self.conn.write_all(rec).unwrap();
        let mut rh = [0u8; 4];
        self.conn.read_exact(&mut rh).unwrap();
        let mut body = vec![0u8; (u32::from_be_bytes(rh) & !(1 << 31)) as usize];
        self.conn.read_exact(&mut body).unwrap();
        body
    }

    /// Sends ops, n of them, and gives the reply after its status, tag
    /// and count.
    fn compound(&mut self, ops: &[u8], n: u32) -> Dec {
        self.xid += 1;
        let rec = self.call(self.xid, ops, n);
        let body = self.send(&rec);
        self.reply(body)
    }

    fn reply(&self, body: Vec<u8>) -> Dec {
        let mut d = Dec { b: body, at: 0 };
        let (xid, kind, accepted) = (d.u32(), d.u32(), d.u32());
        assert!(
            xid == self.xid && kind == 1 && accepted == 0,
            "not an accepted reply to the call"
        );
        d.u32();
        d.opaque();
        assert_eq!(d.u32(), 0, "RPC status");
        d.u32(); // the compound's status
        d.opaque();
        d.u32();
        d
    }

    /// A SEQUENCE, first in a compound.
    fn seq_op(&mut self, e: &mut Enc) {
        self.seq += 1;
        e.u32(53)
            .fixed(&self.session.clone())
            .u32(self.seq)
            .u32(0)
            .u32(0)
            .u32(0);
    }

    /// Sends SEQUENCE and the ops ops writes, as many as it says, and gives
    /// the reply after SEQUENCE's.
    fn run(&mut self, ops: impl FnOnce(&mut Enc) -> u32) -> Dec {
        let mut e = Enc::default();
        self.seq_op(&mut e);
        let n = ops(&mut e);
        let mut d = self.compound(&e.b, n + 1);
        skip_sequence(&mut d);
        d
    }

    /// Creates and opens name in set to read and write, giving its stateid.
    fn open(&mut self, set: &str, name: &str) -> Vec<u8> {
        let id = self.id;
        let mut d = self.run(|e| {
            path(e, &[set]);
            // OPEN: seqid, both access, no deny, owner, create unchecked
            // with no attributes, by name.
            e.u32(18).u32(0).u32(3).u32(0).u64(id).str("owner");
            e.u32(1).u32(0).u32(0).u32(0).u32(0).str(name);
            3
        });
        assert_eq!(status(&mut d), 0, "PUTROOTFH");
        assert_eq!(status(&mut d), 0, "LOOKUP {set}");
        assert_eq!(status(&mut d), 0, "OPEN {name}");
        d.fixed(16)
    }
}

fn expect_ok(d: &mut Dec, op: u32) {
    let (got, st) = (d.u32(), d.u32());
    assert!(got == op && st == 0, "op {op}: status {st} (op {got})");
}

fn skip_sequence(d: &mut Dec) {
    expect_ok(d, 53);
    d.fixed(16);
    for _ in 0..5 {
        d.u32();
    }
}

/// One op's result header, giving its status.
fn status(d: &mut Dec) -> u32 {
    d.u32();
    d.u32()
}

fn path(e: &mut Enc, parts: &[&str]) {
    e.u32(24); // PUTROOTFH
    for p in parts {
        e.u32(15).str(p); // LOOKUP
    }
}

/// The names READDIR gives for a directory, asking for no attributes.
fn list(c: &mut Client, parts: &[&str]) -> Vec<String> {
    let mut d = c.run(|e| {
        path(e, parts);
        e.u32(26).u64(0).fixed(&[0; 8]).u32(8192).u32(32768).u32(0);
        parts.len() as u32 + 2
    });
    for _ in 0..=parts.len() {
        assert_eq!(status(&mut d), 0, "the path to {parts:?}");
    }
    assert_eq!(status(&mut d), 0, "READDIR {parts:?}");
    d.fixed(8);
    let mut names = vec![];
    while d.bool() {
        d.u64();
        names.push(String::from_utf8(d.opaque()).unwrap());
        for _ in 0..d.u32() {
            d.u32();
        }
        d.opaque();
    }
    assert!(d.bool(), "READDIR {parts:?} did not reach the end");
    names
}

#[test]
fn files_through_the_view() {
    let mut v = sets(&["mine"]);
    v.hidden.insert("mine/secret".into());
    let f = Fixture::new("view", v);
    f.make_set("theirs");
    std::fs::write(f.path("theirs/f"), "x").unwrap();
    f.make_set("mine");
    std::fs::write(f.path("mine/secret"), "x").unwrap();
    let mut c = connect(&f, "a");

    // Another's set, and a hidden file, are not there.
    for p in [&["theirs"][..], &["mine", "secret"]] {
        let mut d = c.run(|e| {
            path(e, p);
            p.len() as u32 + 1
        });
        status(&mut d);
        for i in 0..p.len() {
            let st = status(&mut d);
            if i == p.len() - 1 {
                assert_eq!(st, 2, "{p:?}: not NOENT");
            }
        }
    }
    assert_eq!(list(&mut c, &[]), ["mine"]);

    // Written and read back.
    let sid = c.open("mine", "f");
    let mut d = c.run(|e| {
        path(e, &["mine", "f"]);
        e.u32(38).fixed(&sid).u64(0).u32(2).opaque(b"hello");
        e.u32(25).fixed(&sid).u64(0).u32(100);
        5
    });
    for _ in 0..3 {
        status(&mut d);
    }
    assert_eq!(status(&mut d), 0, "WRITE");
    assert_eq!(d.u32(), 5, "written");
    d.u32();
    d.fixed(8);
    assert_eq!(status(&mut d), 0, "READ");
    assert!(d.bool(), "a read of the whole file is not at its end");
    assert_eq!(d.opaque(), b"hello");
    assert_eq!(std::fs::read(f.path("mine/f")).unwrap(), b"hello");
    assert_eq!(list(&mut c, &["mine"]), ["f"], "the hidden file is listed");
}

// A lock is refused another client while one holds it, and free once that
// one closes the file; making a directory is exclusive too.
#[test]
fn locks_between_clients() {
    let f = Fixture::new("locks", sets(&["s"]));
    let (mut a, mut b) = (connect(&f, "a"), connect(&f, "b"));
    let (sa, sb) = (a.open("s", "f"), b.open("s", "f"));

    let lock = |c: &mut Client, sid: &[u8]| {
        let id = c.id;
        let mut d = c.run(|e| {
            path(e, &["s", "f"]);
            // LOCK: write, no reclaim, the whole file, a new lock owner.
            e.u32(12).u32(2).u32(0).u64(0).u64(u64::MAX).u32(1).u32(0);
            e.fixed(sid).u32(0).u64(id).str("lockowner");
            4
        });
        for _ in 0..3 {
            status(&mut d);
        }
        status(&mut d)
    };
    assert_eq!(lock(&mut a, &sa), 0, "the first lock");
    assert_eq!(
        lock(&mut b, &sb),
        10010,
        "a second client's lock is not DENIED"
    );
    let mut d = a.run(|e| {
        path(e, &["s", "f"]);
        e.u32(4).u32(0).fixed(&sa);
        4
    });
    for _ in 0..3 {
        status(&mut d);
    }
    assert_eq!(status(&mut d), 0, "CLOSE");
    assert_eq!(lock(&mut b, &sb), 0, "the lock after the holder closed");

    let mkdir = |c: &mut Client| {
        let mut d = c.run(|e| {
            path(e, &["s"]);
            e.u32(6).u32(2).str("lockdir").u32(0).u32(0);
            3
        });
        status(&mut d);
        status(&mut d);
        status(&mut d)
    };
    assert_eq!(mkdir(&mut a), 0, "mkdir");
    assert_eq!(mkdir(&mut b), 17, "a second mkdir is not EXIST");
}

// A link made in a set does not lead out of it.
#[test]
fn links_stay_in_the_root() {
    let f = Fixture::new("links", sets(&["s"]));
    let outside = scratch("links-outside");
    std::fs::write(outside.join("f"), "outside").unwrap();
    f.make_set("s");
    std::os::unix::fs::symlink(&outside, f.path("s/out")).unwrap();
    let mut c = connect(&f, "a");
    let mut d = c.run(|e| {
        path(e, &["s", "out", "f"]);
        4
    });
    for _ in 0..3 {
        status(&mut d);
    }
    assert_ne!(
        status(&mut d),
        0,
        "looked a file up through a link out of the root"
    );
    let _ = std::fs::remove_dir_all(outside);
}

// ALLOCATE grows a file to hold the range, as fallocate does; DEALLOCATE
// punches a hole in it, keeping its size.
#[test]
fn allocate() {
    let f = Fixture::new("allocate", sets(&["s"]));
    f.make_set("s");
    let mut c = connect(&f, "a");
    let sid = c.open("s", "f");
    let mut op = |code: u32, offset: u64, length: u64| {
        let mut d = c.run(|e| {
            path(e, &["s", "f"]);
            e.u32(code).fixed(&sid).u64(offset).u64(length);
            4
        });
        for _ in 0..3 {
            status(&mut d);
        }
        status(&mut d)
    };
    assert_eq!(op(59, 0, 8192), 0, "ALLOCATE");
    assert_eq!(std::fs::metadata(f.path("s/f")).unwrap().len(), 8192);
    std::fs::write(f.path("s/f"), vec![1u8; 8192]).unwrap();
    assert_eq!(op(62, 0, 4096), 0, "DEALLOCATE");
    let b = std::fs::read(f.path("s/f")).unwrap();
    assert_eq!(b.len(), 8192, "a hole changed the size");
    assert!(
        b[..4096].iter().all(|x| *x == 0),
        "the hole does not read as zeros"
    );
    assert!(
        b[4096..].iter().all(|x| *x == 1),
        "the hole reached past its range"
    );
}

// A retransmitted call gets the reply the first one got, not the answer a
// second run of it would give.
#[test]
fn retransmission_replays() {
    let f = Fixture::new("replay", sets(&["s"]));
    f.make_set("s");
    let mut c = connect(&f, "a");
    let mut e = Enc::default();
    c.seq_op(&mut e);
    path(&mut e, &["s"]);
    e.u32(6).u32(2).str("once").u32(0).u32(0);
    c.xid += 1;
    let rec = c.call(c.xid, &e.b, 4);
    let first = c.send(&rec);
    let again = c.send(&rec);
    assert_eq!(first, again, "the retransmission's reply differs");
    let mut d = c.reply(again);
    skip_sequence(&mut d);
    status(&mut d);
    status(&mut d);
    assert_eq!(status(&mut d), 0, "the CREATE replayed");
    assert!(Path::new(&f.path("s/once")).is_dir());
}

// A rename stays in its set, and the file is found by its new name.
#[test]
fn rename() {
    let f = Fixture::new("rename", sets(&["s", "t"]));
    f.make_set("s");
    f.make_set("t");
    std::fs::write(f.path("s/a"), "x").unwrap();
    let mut c = connect(&f, "a");
    let mut mv = |from: &str, to: &str| {
        let mut d = c.run(|e| {
            path(e, &[from]);
            e.u32(32); // SAVEFH
            path(e, &[to]);
            e.u32(29).str("a").str("b"); // RENAME
            6
        });
        for _ in 0..5 {
            assert_eq!(status(&mut d), 0);
        }
        status(&mut d)
    };
    assert_eq!(mv("s", "t"), 18, "a rename across sets is not XDEV");
    assert_eq!(mv("s", "s"), 0, "RENAME");
    assert!(!f.path("s/a").exists() && f.path("s/b").exists());
    assert_eq!(list(&mut c, &["s"]), ["b"]);
}

// Who a set's callers act as: one owner for everything, root as nobody,
// or each as themselves. Only a server run as root acts as anyone else.
#[test]
fn squash() {
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipped: acting as another user needs root");
        return;
    }
    let names = ["all", "root", "none"];
    let mut v = sets(&["all", "root", "none", "fresh"]);
    v.squash = HashMap::from([
        ("all".into(), Squash::All),
        ("root".into(), Squash::Root),
        ("none".into(), Squash::None),
        ("fresh".into(), Squash::None),
    ]);
    let f = Fixture::new("squash", v);
    for set in names {
        std::fs::create_dir_all(f.path(set)).unwrap();
        std::fs::set_permissions(
            f.path(set),
            std::os::unix::fs::PermissionsExt::from_mode(0o777),
        )
        .unwrap();
    }
    let owner = |set: &str, name: &str| {
        std::fs::symlink_metadata(f.path(set).join(name))
            .unwrap()
            .uid()
    };
    // chown asks a SETATTR of the owner, and gives its status.
    let chown = |c: &mut Client, set: &str, name: &str, uid: u32| {
        let mut d = c.run(|e| {
            path(e, &[set, name]);
            let mut attrs = Enc::default();
            attrs.str(&uid.to_string());
            e.u32(34)
                .fixed(&[0; 16])
                .u32(2)
                .u32(0)
                .u32(1 << (36 - 32))
                .opaque(&attrs.b);
            4
        });
        for _ in 0..3 {
            status(&mut d);
        }
        status(&mut d)
    };

    let mut root_caller = connect(&f, "root");
    root_caller.as_root = true;
    for (set, want) in [("all", 1000), ("root", 65534), ("none", 0)] {
        root_caller.open(set, "byroot");
        assert_eq!(owner(set, "byroot"), want, "{set}: a file root made");
    }
    assert_eq!(
        chown(&mut root_caller, "none", "byroot", 4321),
        0,
        "root changing an owner with no squash"
    );
    assert_eq!(owner("none", "byroot"), 4321);
    assert_eq!(
        chown(&mut root_caller, "all", "byroot", 4321),
        0,
        "changing an owner squashed to one"
    );
    assert_eq!(
        owner("all", "byroot"),
        1000,
        "squashed to one owner, the owner changed"
    );

    // A set with no directory yet has one made, the environments' user's,
    // for a caller to make files in.
    let mut user = connect(&f, "user");
    user.open("fresh", "f");
    assert_eq!(owner("fresh", "f"), 1000, "a file made in a new set");
    assert_eq!(owner("fresh", "."), 1000, "a new set's directory");

    // A user is refused another's file, with no squash.
    assert_eq!(
        chown(&mut user, "none", "byroot", 1000),
        1,
        "a user taking another's file is not PERM"
    );
}
