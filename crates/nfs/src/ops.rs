//! The operations: sessions, and the namespace -- looking up, listing,
//! making, removing and renaming files. Opens, locks and I/O are state.rs's.

use std::sync::Arc;
use std::time::Instant;

use crate::attr::{Node, change, decode_attrs, ftype};
use crate::fs::{Fd, cstr, dir_of, fstatat, join, split, valid_name};
use crate::proto::*;
use crate::server::{
    Client, Cstate, MAX_SLOTS, SLOT_WAIT, Server, Session, Slot, Stateid, known, random,
    read_stateid,
};
use crate::xdr::{Reader, Writer};

const AUTH_SYS: u32 = 1;

/// The most a CREATE_SESSION may ask of a fore channel.
const MAX_RECORD: u32 = MAX_IO + (64 << 10);
const MAX_OPS: u32 = 32;
const MAX_CACHED: u32 = 64 << 10;

impl Server {
    /// Runs one operation, reading its arguments from r and writing its
    /// result to w. A SEQUENCE retransmitted gives the reply it gave before.
    pub(crate) fn op(
        &self,
        cs: &mut Cstate,
        op: u32,
        r: &mut Reader,
        w: &mut Writer,
    ) -> (u32, Option<Vec<u8>>) {
        let st = match op {
            OP_EXCHANGE_ID => self.exchange_id(r, w),
            OP_CREATE_SESSION => self.create_session(r, w),
            OP_DESTROY_SESSION => self.destroy_session(r),
            OP_DESTROY_CLIENT_ID => self.destroy_client_id(r),
            OP_BIND_CONN_TO_SESSION => self.bind_conn(r, w),
            OP_SEQUENCE => return self.sequence(cs, r, w),
            OP_RECLAIM_COMPLETE => {
                r.bool();
                NFS_OK
            }
            OP_SEC_INFO_NO_NAME => {
                r.u32();
                cs.has_cur = false;
                secinfo(w);
                NFS_OK
            }
            OP_SEC_INFO => {
                r.opaque(MAX_NAME_LEN);
                if !cs.has_cur {
                    ERR_NO_FILE_HANDLE
                } else {
                    cs.has_cur = false;
                    secinfo(w);
                    NFS_OK
                }
            }
            OP_PUT_ROOT_FH | OP_PUT_PUB_FH => {
                cs.cur.clear();
                cs.has_cur = true;
                NFS_OK
            }
            OP_PUT_FH => match self.h.path(r.opaque(128)) {
                Ok(p) => match self.visible(&p) {
                    NFS_OK => {
                        cs.cur = p;
                        cs.has_cur = true;
                        NFS_OK
                    }
                    st => st,
                },
                Err(st) => st,
            },
            OP_GET_FH => {
                if !cs.has_cur {
                    ERR_NO_FILE_HANDLE
                } else {
                    w.opaque(&self.h.handle(&cs.cur));
                    NFS_OK
                }
            }
            OP_SAVE_FH => {
                if !cs.has_cur {
                    ERR_NO_FILE_HANDLE
                } else {
                    cs.save = cs.cur.clone();
                    cs.has_save = true;
                    NFS_OK
                }
            }
            OP_RESTORE_FH => {
                if !cs.has_save {
                    ERR_RESTORE_FH
                } else {
                    cs.cur = cs.save.clone();
                    cs.has_cur = true;
                    NFS_OK
                }
            }
            OP_LOOKUP => {
                let name = r.string(MAX_NAME_LEN + 1);
                self.lookup(cs, &name)
            }
            OP_LOOKUP_P => {
                if !cs.has_cur {
                    ERR_NO_FILE_HANDLE
                } else if cs.cur.is_empty() {
                    ERR_NO_ENT
                } else {
                    cs.cur = dir_of(&cs.cur).to_vec();
                    NFS_OK
                }
            }
            OP_GETATTR => {
                let req = r.bitmap();
                self.getattr(cs, &req, w)
            }
            OP_SETATTR => self.setattr(cs, r, w),
            OP_ACCESS => {
                let want = r.u32();
                self.access(cs, want, w)
            }
            OP_READ_LINK => self.readlink(cs, w),
            OP_READ_DIR => self.readdir(cs, r, w),
            OP_CREATE => self.create(cs, r, w),
            OP_REMOVE => {
                let name = r.string(MAX_NAME_LEN + 1);
                self.remove(cs, &name, w)
            }
            OP_RENAME => {
                let from = r.string(MAX_NAME_LEN + 1);
                let to = r.string(MAX_NAME_LEN + 1);
                self.rename(cs, &from, &to, w)
            }
            OP_LINK => {
                let name = r.string(MAX_NAME_LEN + 1);
                self.link(cs, &name, w)
            }
            OP_OPEN => self.open(cs, r, w),
            OP_CLOSE => self.close_state(cs, r, w),
            OP_OPEN_DOWNGRADE => self.open_downgrade(cs, r, w),
            OP_READ => self.read(cs, r, w),
            OP_WRITE => self.write(cs, r, w),
            OP_COMMIT => self.commit(cs, r, w),
            OP_ALLOCATE => self.allocate(cs, r, false),
            OP_DEALLOCATE => self.allocate(cs, r, true),
            OP_LOCK => self.lock(cs, r, w),
            OP_LOCK_T => self.lockt(cs, r, w),
            OP_LOCK_U => self.locku(cs, r, w),
            OP_FREE_STATE_ID => {
                let id = read_stateid(r);
                self.free_stateid(id)
            }
            OP_TEST_STATE_ID => {
                let n = r.count(1024);
                let ids: Vec<Stateid> = (0..n).map(|_| read_stateid(r)).collect();
                w.u32(n as u32);
                let inner = self.inner.lock().unwrap();
                for id in ids {
                    w.u32(if inner.states.contains_key(&id.other) {
                        NFS_OK
                    } else {
                        ERR_BAD_STATE_ID
                    });
                }
                NFS_OK
            }
            OP_DELEG_RETURN => {
                read_stateid(r);
                NFS_OK
            }
            OP_DELEG_PURGE => {
                r.u64();
                NFS_OK
            }
            OP_BACKCHANNEL_CTL => ERR_NOT_SUPP,
            op if known(op) => ERR_NOT_SUPP,
            _ => ERR_OP_ILLEGAL,
        };
        (st, None)
    }

    fn exchange_id(&self, r: &mut Reader, w: &mut Writer) -> u32 {
        let mut verifier = [0u8; 8];
        verifier.copy_from_slice(&pad8(r.fixed(8)));
        let owner = r.opaque(1024).to_vec();
        r.u32(); // flags
        match r.u32() {
            SP4_MACH_CRED => {
                r.bitmap();
                r.bitmap();
            }
            SP4_SSV => return ERR_NOT_SUPP,
            _ => {}
        }
        if r.count(1) == 1 {
            r.opaque(1024);
            r.opaque(1024);
            r.i64();
            r.u32();
        }
        if r.failed {
            return ERR_BAD_XDR;
        }
        let mut inner = self.inner.lock().unwrap();
        let mut id = inner.by_owner.get(&owner).copied();
        if let Some(i) = id
            && inner.clients[&i].verifier != verifier
        {
            // The client restarted: what it held before is gone.
            crate::server::drop_client(&mut inner, i);
            id = None;
        }
        let id = match id {
            Some(i) => i,
            None => {
                let i = Server::new_id(&mut inner);
                inner.clients.insert(
                    i,
                    Client {
                        id: i,
                        owner: owner.clone(),
                        verifier,
                        seq: 0,
                        reply: None,
                        confirmed: false,
                        renewed: Instant::now(),
                    },
                );
                inner.by_owner.insert(owner, i);
                i
            }
        };
        let c = &inner.clients[&id];
        let mut flags = EXCHGID_USE_NON_PNFS;
        if c.confirmed {
            flags |= EXCHGID_CONFIRMED_R;
        }
        w.u64(c.id);
        w.u32(c.seq.wrapping_add(1));
        drop(inner);
        w.u32(flags);
        w.u32(SP4_NONE);
        w.u64(0); // server_owner.so_minor_id
        w.opaque(b"hangar"); // so_major_id
        w.opaque(b"hangar"); // server scope
        w.u32(0); // no implementation ID
        NFS_OK
    }

    fn create_session(&self, r: &mut Reader, w: &mut Writer) -> u32 {
        let client_id = r.u64();
        let seq = r.u32();
        r.u32(); // flags: no persistence, no back channel, are given
        let mut fore = read_channel(r);
        let mut back = read_channel(r);
        r.u32(); // callback program
        let n = r.count(16);
        for _ in 0..n {
            match r.u32() {
                AUTH_SYS => {
                    r.u32();
                    r.opaque(255);
                    r.u32();
                    r.u32();
                    for _ in 0..r.count(16) {
                        r.u32();
                    }
                }
                6 => {
                    // RPCSEC_GSS
                    r.u32();
                    r.opaque(1024);
                    r.opaque(1024);
                }
                _ => {}
            }
        }
        if r.failed {
            return ERR_BAD_XDR;
        }
        let mut inner = self.inner.lock().unwrap();
        let Some(c) = inner.clients.get(&client_id) else {
            return ERR_STALE_CLIENT_ID;
        };
        if seq == c.seq
            && let Some(reply) = &c.reply
        {
            w.b.extend_from_slice(reply);
            return NFS_OK;
        }
        if seq != c.seq.wrapping_add(1) {
            return ERR_SEQ_MISORDERED;
        }
        let id: [u8; 16] = random();
        fore[0] = 0;
        fore[1] = fore[1].min(MAX_RECORD);
        fore[2] = fore[2].min(MAX_RECORD);
        fore[3] = fore[3].min(MAX_CACHED);
        fore[4] = fore[4].min(MAX_OPS);
        fore[5] = fore[5].clamp(1, MAX_SLOTS);
        back[0] = 0;
        back[5] = 1;
        let se = Session {
            client: client_id,
            slots: (0..fore[5]).map(|_| Slot::default()).collect(),
            max_ops: fore[4],
        };
        inner.sessions.insert(id, Arc::new(se));
        let c = inner.clients.get_mut(&client_id).unwrap();
        c.confirmed = true;
        c.renewed = Instant::now();
        c.seq = seq;
        let mut res = Writer::default();
        res.fixed(&id);
        res.u32(seq);
        res.u32(0); // flags: no back channel
        write_channel(&mut res, &fore);
        write_channel(&mut res, &back);
        c.reply = Some(res.b.clone());
        w.b.extend_from_slice(&res.b);
        NFS_OK
    }

    fn destroy_session(&self, r: &mut Reader) -> u32 {
        let id = id16(r);
        let mut inner = self.inner.lock().unwrap();
        if inner.sessions.remove(&id).is_none() {
            return ERR_BAD_SESSION;
        }
        NFS_OK
    }

    fn destroy_client_id(&self, r: &mut Reader) -> u32 {
        let id = r.u64();
        let mut inner = self.inner.lock().unwrap();
        if !inner.clients.contains_key(&id) {
            return ERR_STALE_CLIENT_ID;
        }
        if inner.sessions.values().any(|se| se.client == id) {
            return ERR_CLIENT_ID_BUSY;
        }
        crate::server::drop_client(&mut inner, id);
        NFS_OK
    }

    fn bind_conn(&self, r: &mut Reader, w: &mut Writer) -> u32 {
        let id = id16(r);
        r.u32(); // direction
        r.bool();
        if !self.inner.lock().unwrap().sessions.contains_key(&id) {
            return ERR_BAD_SESSION;
        }
        w.fixed(&id);
        w.u32(1); // the fore channel: there is no back channel
        w.bool(false);
        NFS_OK
    }

    fn sequence(&self, cs: &mut Cstate, r: &mut Reader, w: &mut Writer) -> (u32, Option<Vec<u8>>) {
        let id = id16(r);
        let (seq, slot_id) = (r.u32(), r.u32());
        r.u32(); // highest slot the client uses
        r.bool(); // whether to cache: every reply that fits is
        if r.failed {
            return (ERR_BAD_XDR, None);
        }
        let se = {
            let mut inner = self.inner.lock().unwrap();
            let se = inner.sessions.get(&id).cloned();
            if let Some(se) = &se
                && let Some(c) = inner.clients.get_mut(&se.client)
            {
                c.renewed = Instant::now();
            }
            se
        };
        let Some(se) = se else {
            return (ERR_BAD_SESSION, None);
        };
        let Some(sl) = se.slots.get(slot_id as usize) else {
            return (ERR_BAD_SLOT, None);
        };
        // One compound at a time on a slot: a retransmission racing its
        // original waits for it.
        let mut st = sl.state.lock().unwrap();
        let deadline = Instant::now() + SLOT_WAIT;
        while st.busy {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return (ERR_DELAY, None);
            }
            st = sl.free.wait_timeout(st, left).unwrap().0;
        }
        if seq == st.seq && st.seq != 0 {
            return match st.reply.clone() {
                Some(replay) => (NFS_OK, Some(replay)),
                None => (ERR_RETRY_UNCACHED, None),
            };
        }
        if seq != st.seq.wrapping_add(1) {
            return (ERR_SEQ_MISORDERED, None);
        }
        st.seq = seq;
        st.busy = true;
        drop(st);
        let slots = se.slots.len() as u32;
        cs.session = Some(se);
        cs.slot = Some(slot_id as usize);
        w.fixed(&id);
        w.u32(seq);
        w.u32(slot_id);
        w.u32(slots - 1);
        w.u32(slots - 1);
        w.u32(0); // status flags
        (NFS_OK, None)
    }

    /// A path's attributes, the root's among them.
    pub(crate) fn node(&self, p: &[u8]) -> Result<Node, u32> {
        if p.is_empty() {
            // SAFETY: a zeroed stat stands for the root, whose own are
            // made up when encoded.
            return Ok(Node {
                root: true,
                st: unsafe { std::mem::zeroed() },
                fh: self.h.handle(b""),
            });
        }
        let st = self.root.lstat(p).map_err(|e| Server::status(&e))?;
        Ok(Node {
            root: false,
            st,
            fh: self.h.handle(p),
        })
    }

    fn lookup(&self, cs: &mut Cstate, name: &[u8]) -> u32 {
        if !cs.has_cur {
            return ERR_NO_FILE_HANDLE;
        }
        let st = valid_name(name);
        if st != NFS_OK {
            return st;
        }
        let p = join(&cs.cur, name);
        if cs.cur.is_empty() {
            let set = String::from_utf8_lossy(name);
            if !self.view.sets().iter().any(|s| *s == set) {
                return ERR_NO_ENT;
            }
            if let Err(e) = self.ensure_set(name) {
                return Server::status(&e);
            }
        } else {
            let n = match self.node(&cs.cur) {
                Ok(n) => n,
                Err(st) => return st,
            };
            match ftype(n.st.st_mode as u32) {
                NF4_DIR => {}
                NF4_LNK => return ERR_SYMLINK,
                _ => return ERR_NOT_DIR,
            }
            let st = self.visible(&p);
            if st != NFS_OK {
                return st;
            }
        }
        if let Err(e) = self.root.lstat(&p) {
            return Server::status(&e);
        }
        cs.cur = p;
        NFS_OK
    }

    fn getattr(&self, cs: &Cstate, req: &crate::xdr::Bitmap, w: &mut Writer) -> u32 {
        if !cs.has_cur {
            return ERR_NO_FILE_HANDLE;
        }
        let n = match self.node(&cs.cur) {
            Ok(n) => n,
            Err(st) => return st,
        };
        let (mask, vals) = self.encode_attrs(req, &n);
        w.bitmap(&mask);
        w.opaque(&vals);
        NFS_OK
    }

    fn setattr(&self, cs: &Cstate, r: &mut Reader, w: &mut Writer) -> u32 {
        read_stateid(r);
        let (set, st) = match decode_attrs(r) {
            Err(st) => (Default::default(), st),
            Ok(_) if !cs.has_cur => (Default::default(), ERR_NO_FILE_HANDLE),
            Ok(_) if cs.cur.is_empty() => (Default::default(), ERR_ACCESS),
            Ok(a) => self.apply(&cs.cur, &a),
        };
        // The attributes set are given whether or not it succeeded.
        w.bitmap(&set);
        st
    }

    /// Which of the asked-for accesses the caller has, by the file's mode
    /// bits, as a server checking permissions would.
    fn access(&self, cs: &Cstate, want: u32, w: &mut Writer) -> u32 {
        if !cs.has_cur {
            return ERR_NO_FILE_HANDLE;
        }
        let n = match self.node(&cs.cur) {
            Ok(n) => n,
            Err(st) => return st,
        };
        // The kernel's answer, for the caller as the operation runs: their
        // filesystem identity is the thread's.
        let mut mode = 0o5;
        if !n.root {
            mode = 0;
            let (fd, name) = match self.root.at(&cs.cur) {
                Ok(x) => x,
                Err(e) => return Server::status(&e),
            };
            let Ok(c) = cstr(name) else { return ERR_INVAL };
            for bit in [libc::R_OK, libc::W_OK, libc::X_OK] {
                // SAFETY: c is a valid NUL-terminated name.
                if unsafe { libc::faccessat(fd.0, c.as_ptr(), bit, libc::AT_EACCESS) } == 0 {
                    mode |= bit;
                }
            }
        }
        let dir = n.root || ftype(n.st.st_mode as u32) == NF4_DIR;
        let mut got = 0;
        if mode & 4 != 0 {
            got |= ACCESS_READ;
        }
        if mode & 2 != 0 {
            got |= ACCESS_MODIFY | ACCESS_EXTEND;
            if dir {
                got |= ACCESS_DELETE;
            }
        }
        if mode & 1 != 0 {
            got |= if dir { ACCESS_LOOKUP } else { ACCESS_EXECUTE };
        }
        w.u32(want & ACCESS_ALL);
        w.u32(want & got);
        NFS_OK
    }

    fn readlink(&self, cs: &Cstate, w: &mut Writer) -> u32 {
        if !cs.has_cur {
            return ERR_NO_FILE_HANDLE;
        }
        if cs.cur.is_empty() {
            return ERR_INVAL;
        }
        let (fd, name) = match self.root.at(&cs.cur) {
            Ok(x) => x,
            Err(e) => return Server::status(&e),
        };
        let Ok(c) = cstr(name) else { return ERR_INVAL };
        let mut buf = vec![0u8; 4096];
        // SAFETY: buf is writable for its length.
        let n = unsafe { libc::readlinkat(fd.0, c.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 {
            return Server::status(&std::io::Error::last_os_error());
        }
        w.opaque(&buf[..n as usize]);
        NFS_OK
    }

    /// A directory's entries, by name, but those hidden.
    fn names(&self, dir: &[u8]) -> Result<Vec<Vec<u8>>, u32> {
        if dir.is_empty() {
            let mut sets: Vec<Vec<u8>> = self
                .view
                .sets()
                .into_iter()
                .map(String::into_bytes)
                .collect();
            sets.sort();
            return Ok(sets);
        }
        let fd = self
            .root
            .open_dir_read(dir)
            .map_err(|e| Server::status(&e))?;
        let mut names = read_names(fd).map_err(|_| ERR_IO)?;
        names.sort();
        let (set, rest) = split(dir);
        let set = String::from_utf8_lossy(set).into_owned();
        names.retain(|n| !self.view.hidden(&set, &join(rest, n)));
        Ok(names)
    }

    fn readdir(&self, cs: &Cstate, r: &mut Reader, w: &mut Writer) -> u32 {
        let cookie = r.u64();
        r.fixed(8); // the cookie verifier: a cookie is a position in the sorted names
        let dircount = r.u32() as usize;
        let maxcount = (r.u32()).min(MAX_IO) as usize;
        let req = r.bitmap();
        if r.failed {
            return ERR_BAD_XDR;
        }
        if !cs.has_cur {
            return ERR_NO_FILE_HANDLE;
        }
        if !cs.cur.is_empty() {
            match self.node(&cs.cur) {
                Ok(n) if ftype(n.st.st_mode as u32) != NF4_DIR => return ERR_NOT_DIR,
                Ok(_) => {}
                Err(st) => return st,
            }
        }
        let names = match self.names(&cs.cur) {
            Ok(n) => n,
            Err(st) => return st,
        };
        if cookie != 0 && cookie < 3 {
            return ERR_BAD_COOKIE;
        }
        let start = if cookie >= 3 {
            (cookie - 2) as usize
        } else {
            0
        };
        if start > names.len() {
            return ERR_BAD_COOKIE;
        }
        let mut body = Writer::default();
        body.fixed(&[0u8; 8]); // the cookie verifier
        let mut used = 0;
        let mut eof = true;
        let mut entries = 0;
        for (i, name) in names.iter().enumerate().skip(start) {
            let p = join(&cs.cur, name);
            if cs.cur.is_empty() && self.ensure_set(name).is_err() {
                continue;
            }
            // Gone since it was listed.
            let Ok(n) = self.node(&p) else { continue };
            let mut e = Writer::default();
            e.bool(true);
            e.u64(i as u64 + 3);
            e.opaque(name);
            let (mask, vals) = self.encode_attrs(&req, &n);
            e.bitmap(&mask);
            e.opaque(&vals);
            used += name.len() + 24;
            // The reply's header and the end of the list take the rest.
            if body.b.len() + e.b.len() + 32 > maxcount
                || (dircount > 0 && used > dircount && entries > 0)
            {
                eof = false;
                break;
            }
            body.b.extend_from_slice(&e.b);
            entries += 1;
        }
        if entries == 0 && !eof {
            return ERR_TOO_SMALL;
        }
        body.bool(false);
        body.bool(eof);
        w.b.extend_from_slice(&body.b);
        NFS_OK
    }

    /// A directory's change attribute, for change_info.
    pub(crate) fn change_of(&self, dir: &[u8]) -> u64 {
        if dir.is_empty() {
            return self.started.0 as u64;
        }
        self.root.lstat(dir).map(|st| change(&st)).unwrap_or(0)
    }

    /// Checks a name to make in the current directory, a set's.
    pub(crate) fn child(&self, cs: &Cstate, name: &[u8]) -> Result<Vec<u8>, u32> {
        if !cs.has_cur {
            return Err(ERR_NO_FILE_HANDLE);
        }
        if cs.cur.is_empty() {
            // The root is the sets', which are given, not made.
            return Err(ERR_ACCESS);
        }
        let st = valid_name(name);
        if st != NFS_OK {
            return Err(st);
        }
        let p = join(&cs.cur, name);
        if self.visible(&p) != NFS_OK {
            return Err(ERR_ACCESS);
        }
        Ok(p)
    }

    fn create(&self, cs: &mut Cstate, r: &mut Reader, w: &mut Writer) -> u32 {
        let typ = r.u32();
        let mut target = vec![];
        match typ {
            NF4_LNK => target = r.string(4096),
            NF4_BLK | NF4_CHR => {
                r.u32();
                r.u32();
            }
            _ => {}
        }
        let name = r.string(MAX_NAME_LEN + 1);
        let a = match decode_attrs(r) {
            Ok(a) => a,
            Err(st) => return st,
        };
        if r.failed {
            return ERR_BAD_XDR;
        }
        let p = match self.child(cs, &name) {
            Ok(p) => p,
            Err(st) => return st,
        };
        let fd = match self.root.open_dir(&cs.cur) {
            Ok(fd) => fd,
            Err(e) => return Server::status(&e),
        };
        let before = self.change_of(&cs.cur);
        let mode = a.mode.unwrap_or(0o755);
        let Ok(c) = cstr(&name) else { return ERR_INVAL };
        // SAFETY: c and the target are valid NUL-terminated strings.
        let made = unsafe {
            match typ {
                NF4_DIR => libc::mkdirat(fd.0, c.as_ptr(), mode as libc::mode_t),
                NF4_LNK => {
                    let Ok(t) = cstr(&target) else {
                        return ERR_INVAL;
                    };
                    libc::symlinkat(t.as_ptr(), fd.0, c.as_ptr())
                }
                NF4_FIFO | NF4_SOCK => {
                    let kind = if typ == NF4_FIFO {
                        libc::S_IFIFO
                    } else {
                        libc::S_IFSOCK
                    };
                    match crate::sys::mknod_at(fd.0, &name, kind as u32 | mode) {
                        Ok(()) => 0,
                        Err(e) => return Server::status(&e),
                    }
                }
                NF4_BLK | NF4_CHR => return ERR_PERM,
                _ => return ERR_BAD_TYPE,
            }
        };
        if made < 0 {
            return Server::status(&std::io::Error::last_os_error());
        }
        // The mode is set as given, not as the server's umask leaves it.
        let (set, st) = self.apply(&p, &a);
        if st != NFS_OK {
            return st;
        }
        write_change_info(w, before, self.change_of(&cs.cur));
        w.bitmap(&set);
        cs.cur = p;
        NFS_OK
    }

    fn remove(&self, cs: &Cstate, name: &[u8], w: &mut Writer) -> u32 {
        let p = match self.child(cs, name) {
            Ok(p) => p,
            Err(st) => return st,
        };
        let fd = match self.root.open_dir(&cs.cur) {
            Ok(fd) => fd,
            Err(e) => return Server::status(&e),
        };
        let lst = match fstatat(fd.0, name) {
            Ok(st) => st,
            Err(e) => return Server::status(&e),
        };
        let before = self.change_of(&cs.cur);
        let flags = if lst.st_mode as u32 & libc::S_IFMT as u32 == libc::S_IFDIR as u32 {
            libc::AT_REMOVEDIR
        } else {
            0
        };
        let Ok(c) = cstr(name) else { return ERR_INVAL };
        // SAFETY: c is a valid NUL-terminated name.
        if unsafe { libc::unlinkat(fd.0, c.as_ptr(), flags) } < 0 {
            return Server::status(&std::io::Error::last_os_error());
        }
        self.h.removed(&p);
        write_change_info(w, before, self.change_of(&cs.cur));
        NFS_OK
    }

    fn rename(&self, cs: &Cstate, old: &[u8], new: &[u8], w: &mut Writer) -> u32 {
        if !cs.has_save {
            return ERR_NO_FILE_HANDLE;
        }
        let src = Cstate {
            cur: cs.save.clone(),
            has_cur: true,
            ..Default::default()
        };
        let from = match self.child(&src, old) {
            Ok(p) => p,
            Err(st) => return st,
        };
        let to = match self.child(cs, new) {
            Ok(p) => p,
            Err(st) => return st,
        };
        if split(&from).0 != split(&to).0 {
            return ERR_X_DEV; // a set's files stay in the set
        }
        let (ofd, nfd) = match (self.root.open_dir(&cs.save), self.root.open_dir(&cs.cur)) {
            (Ok(o), Ok(n)) => (o, n),
            (Err(e), _) | (_, Err(e)) => return Server::status(&e),
        };
        let (src_before, dst_before) = (self.change_of(&cs.save), self.change_of(&cs.cur));
        let (Ok(oc), Ok(nc)) = (cstr(old), cstr(new)) else {
            return ERR_INVAL;
        };
        // SAFETY: both names are valid NUL-terminated strings.
        if unsafe { libc::renameat(ofd.0, oc.as_ptr(), nfd.0, nc.as_ptr()) } < 0 {
            return Server::status(&std::io::Error::last_os_error());
        }
        self.h.moved(&from, &to);
        write_change_info(w, src_before, self.change_of(&cs.save));
        write_change_info(w, dst_before, self.change_of(&cs.cur));
        NFS_OK
    }

    fn link(&self, cs: &Cstate, name: &[u8], w: &mut Writer) -> u32 {
        if !cs.has_save {
            return ERR_NO_FILE_HANDLE;
        }
        let to = match self.child(cs, name) {
            Ok(p) => p,
            Err(st) => return st,
        };
        if cs.save.is_empty() {
            return ERR_IS_DIR;
        }
        if split(&cs.save).0 != split(&to).0 {
            return ERR_X_DEV;
        }
        let (ofd, oname) = match self.root.at(&cs.save) {
            Ok(x) => x,
            Err(e) => return Server::status(&e),
        };
        let nfd = match self.root.open_dir(&cs.cur) {
            Ok(fd) => fd,
            Err(e) => return Server::status(&e),
        };
        let before = self.change_of(&cs.cur);
        let (Ok(oc), Ok(nc)) = (cstr(oname), cstr(name)) else {
            return ERR_INVAL;
        };
        // SAFETY: both names are valid NUL-terminated strings.
        if unsafe { libc::linkat(ofd.0, oc.as_ptr(), nfd.0, nc.as_ptr(), 0) } < 0 {
            return Server::status(&std::io::Error::last_os_error());
        }
        write_change_info(w, before, self.change_of(&cs.cur));
        NFS_OK
    }
}

/// The longest name, and so the most a name's argument is read to.
const MAX_NAME_LEN: usize = crate::fs::MAX_NAME;

/// The one flavour this server takes: AUTH_SYS.
fn secinfo(w: &mut Writer) {
    w.u32(1);
    w.u32(AUTH_SYS);
}

fn pad8(b: &[u8]) -> [u8; 8] {
    let mut out = [0u8; 8];
    out[..b.len().min(8)].copy_from_slice(&b[..b.len().min(8)]);
    out
}

fn id16(r: &mut Reader) -> [u8; 16] {
    let mut id = [0u8; 16];
    let b = r.fixed(16);
    id[..b.len()].copy_from_slice(b);
    id
}

/// A channel's attributes: header padding, the most a request and a reply
/// may be, the most a cached reply may be, operations and requests.
fn read_channel(r: &mut Reader) -> [u32; 6] {
    let a = [r.u32(), r.u32(), r.u32(), r.u32(), r.u32(), r.u32()];
    if r.count(1) == 1 {
        r.u32();
    }
    a
}

fn write_channel(w: &mut Writer, a: &[u32; 6]) {
    for v in a {
        w.u32(*v);
    }
    w.u32(0); // no RDMA
}

pub(crate) fn write_change_info(w: &mut Writer, before: u64, after: u64) {
    w.bool(false);
    w.u64(before);
    w.u64(after);
}

/// A directory's entries' names, from a descriptor opened to read it.
fn read_names(fd: Fd) -> std::io::Result<Vec<Vec<u8>>> {
    use std::os::unix::ffi::OsStrExt;
    // SAFETY: fdopendir takes over the descriptor, closed by closedir.
    let dir = unsafe { libc::fdopendir(fd.0) };
    if dir.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    std::mem::forget(fd);
    let mut out = vec![];
    loop {
        // SAFETY: dir is open; readdir's entry is valid until the next call.
        let ent = unsafe { libc::readdir(dir) };
        if ent.is_null() {
            break;
        }
        let name = unsafe { std::ffi::CStr::from_ptr((*ent).d_name.as_ptr()) };
        let b = std::ffi::OsStr::from_bytes(name.to_bytes()).as_bytes();
        if b != b"." && b != b".." {
            out.push(b.to_vec());
        }
    }
    // SAFETY: dir is open, and closed once.
    unsafe { libc::closedir(dir) };
    Ok(out)
}
