//! Opens, locks and I/O: the states a client holds, and the stateids that
//! name them.

use std::io;
use std::os::fd::RawFd;
use std::sync::Arc;

use crate::attr::{SetAttrs, decode_attrs};
use crate::fs::{Fd, MAX_NAME, dir_of, errno};
use crate::ops::write_change_info;
use crate::proto::*;
use crate::server::{
    Cstate, Inner, Kind, Server, State, Stateid, random, read_stateid, write_stateid,
};
use crate::sys::{self, OFD_GETLK, OFD_SETLK};
use crate::xdr::{Bitmap, Reader, Writer};

/// The special stateids: I/O without an open, I/O that bypasses locks
/// (which this server keeps none from), and the compound's current one.
const ANONYMOUS: Stateid = Stateid {
    seqid: 0,
    other: [0; 12],
};
const BYPASS: Stateid = Stateid {
    seqid: u32::MAX,
    other: [0xff; 12],
};
const CURRENT: Stateid = Stateid {
    seqid: 1,
    other: [0; 12],
};

/// The open(2) flags for a share access.
fn open_flags(access: u32) -> libc::c_int {
    match access & SHARE_ACCESS_MASK {
        SHARE_ACCESS_WRITE => libc::O_WRONLY,
        SHARE_ACCESS_BOTH => libc::O_RDWR,
        _ => libc::O_RDONLY,
    }
}

impl Inner {
    fn new_state(
        &mut self,
        kind: Kind,
        client: u64,
        owner: Vec<u8>,
        path: Vec<u8>,
        file: Fd,
    ) -> [u8; 12] {
        let other: [u8; 12] = random();
        self.states.insert(
            other,
            State {
                seqid: 1,
                kind,
                client,
                owner,
                path,
                access: 0,
                file: Arc::new(file),
                parent: None,
            },
        );
        other
    }
}

/// The state a stateid names, the compound's current one standing in for
/// the special "current stateid". None is a special stateid that names
/// none: I/O without an open.
fn resolve(inner: &Inner, cs: &Cstate, mut id: Stateid) -> Result<Option<[u8; 12]>, u32> {
    if id == CURRENT {
        id = cs.cur_state;
    }
    if id == ANONYMOUS || id == BYPASS {
        return Ok(None);
    }
    let Some(st) = inner.states.get(&id.other) else {
        return Err(ERR_BAD_STATE_ID);
    };
    // Zero is the latest. An older one is the client's view from before a
    // change it has yet to hear of -- an OPEN of the same file as it
    // closes it -- which it learns of from OLD_STATEID.
    if id.seqid == 0 || id.seqid == st.seqid {
        Ok(Some(id.other))
    } else if id.seqid < st.seqid {
        Err(ERR_OLD_STATE_ID)
    } else {
        Err(ERR_BAD_STATE_ID)
    }
}

/// A state a stateid names, of the kind wanted.
fn resolve_kind(inner: &Inner, cs: &Cstate, id: Stateid, kind: Kind) -> Result<[u8; 12], u32> {
    match resolve(inner, cs, id)? {
        Some(o) if inner.states[&o].kind == kind => Ok(o),
        _ => Err(ERR_BAD_STATE_ID),
    }
}

fn flock(typ: libc::c_short, start: i64, len: i64) -> libc::flock {
    // SAFETY: a zeroed flock is a valid one; OFD locks need l_pid 0.
    let mut fl: libc::flock = unsafe { std::mem::zeroed() };
    fl.l_type = typ;
    fl.l_whence = libc::SEEK_SET as libc::c_short;
    fl.l_start = start as libc::off_t;
    fl.l_len = len as libc::off_t;
    fl
}

/// Takes, lets go of or tests a lock on fd.
fn fcntl_lock(fd: RawFd, cmd: libc::c_int, fl: &mut libc::flock) -> io::Result<()> {
    // SAFETY: fl is a valid flock for the call to read and write.
    if unsafe { libc::fcntl(fd, cmd, fl as *mut libc::flock) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// A LOCK's range as fcntl takes it: a length of all ones runs to the end
/// of the file, which fcntl's zero does.
fn lock_range(offset: u64, length: u64) -> Result<(i64, i64), u32> {
    const MAX: u64 = i64::MAX as u64;
    if length == 0 {
        return Err(ERR_INVAL);
    }
    if length == u64::MAX {
        if offset > MAX {
            return Err(ERR_BAD_RANGE);
        }
        return Ok((offset as i64, 0));
    }
    if offset > MAX || length > MAX || offset + length > MAX {
        return Err(ERR_BAD_RANGE);
    }
    Ok((offset as i64, length as i64))
}

fn lock_type(t: u32) -> libc::c_short {
    if t == LOCK_READ || t == LOCK_READ_W {
        libc::F_RDLCK as libc::c_short
    } else {
        libc::F_WRLCK as libc::c_short
    }
}

/// Writes LOCK4denied for the lock in the way of fl.
fn denied(fd: RawFd, mut fl: libc::flock, w: &mut Writer) {
    let _ = fcntl_lock(fd, OFD_GETLK, &mut fl);
    w.u64(fl.l_start as u64);
    w.u64(if fl.l_len == 0 {
        u64::MAX
    } else {
        fl.l_len as u64
    });
    w.u32(if fl.l_type == libc::F_RDLCK as libc::c_short {
        LOCK_READ
    } else {
        LOCK_WRITE
    });
    // Whose it is: another environment's, or a program's, which NFS has no
    // name for.
    w.u64(0);
    w.opaque(&[]);
}

fn fstat(fd: RawFd) -> io::Result<libc::stat> {
    // SAFETY: st is written by fstat.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(st)
}

/// Reads at off until buf is full or the file ends.
fn read_at(fd: RawFd, buf: &mut [u8], off: u64) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        let at = off
            .checked_add(n as u64)
            .filter(|a| *a <= i64::MAX as u64)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
        // SAFETY: buf[n..] is writable for its length.
        let r = unsafe {
            libc::pread(
                fd,
                buf[n..].as_mut_ptr().cast(),
                buf.len() - n,
                at as libc::off_t,
            )
        };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        if r == 0 {
            break;
        }
        n += r as usize;
    }
    Ok(n)
}

/// Writes all of data at off.
fn write_at(fd: RawFd, data: &[u8], off: u64) -> io::Result<usize> {
    let mut n = 0;
    while n < data.len() {
        let at = off
            .checked_add(n as u64)
            .filter(|a| *a <= i64::MAX as u64)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
        // SAFETY: data[n..] is readable for its length.
        let r = unsafe {
            libc::pwrite(
                fd,
                data[n..].as_ptr().cast(),
                data.len() - n,
                at as libc::off_t,
            )
        };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        n += r as usize;
    }
    Ok(n)
}

impl Server {
    /// The file to read or write the current file through: the state's, or
    /// for none, one opened for the call alone.
    fn io_file(&self, cs: &Cstate, id: Stateid, write: bool) -> Result<Arc<Fd>, u32> {
        if !cs.has_cur || cs.cur.is_empty() {
            return Err(ERR_NO_FILE_HANDLE);
        }
        {
            let inner = self.inner.lock().unwrap();
            if let Some(other) = resolve(&inner, cs, id)? {
                let mut st = &inner.states[&other];
                if st.path != cs.cur {
                    return Err(ERR_BAD_STATE_ID);
                }
                if st.kind == Kind::Lock {
                    match st.parent.and_then(|p| inner.states.get(&p)) {
                        Some(open) => st = open,
                        None => return Err(ERR_BAD_STATE_ID),
                    }
                }
                let need = if write {
                    SHARE_ACCESS_WRITE
                } else {
                    SHARE_ACCESS_READ
                };
                if st.access & need == 0 {
                    return Err(ERR_OPEN_MODE);
                }
                return Ok(st.file.clone());
            }
        }
        let flags = if write {
            libc::O_WRONLY
        } else {
            libc::O_RDONLY
        };
        self.root
            .open_file(&cs.cur, flags, 0)
            .map(Arc::new)
            .map_err(|e| errno(&e))
    }

    pub(crate) fn open(&self, cs: &mut Cstate, r: &mut Reader, w: &mut Writer) -> u32 {
        r.u32(); // seqid: sessions order calls
        let access = r.u32() & SHARE_ACCESS_MASK;
        r.u32(); // share deny: not enforced
        let client_id = r.u64();
        let owner = r.opaque(1024).to_vec();
        let how = r.u32();
        let mut create_mode = CREATE_UNCHECKED;
        let mut attrs = SetAttrs::default();
        let mut verifier = [0u8; 8];
        if how == OPEN_CREATE {
            create_mode = r.u32();
            match create_mode {
                CREATE_UNCHECKED | CREATE_GUARDED => match decode_attrs(r) {
                    Ok(a) => attrs = a,
                    Err(st) => return st,
                },
                CREATE_EXCLUSIVE => verifier = verifier8(r),
                CREATE_EXCLUSIVE1 => {
                    verifier = verifier8(r);
                    match decode_attrs(r) {
                        Ok(a) => attrs = a,
                        Err(st) => return st,
                    }
                }
                _ => return ERR_INVAL,
            }
        }
        let claim = r.u32();
        let mut name = vec![];
        match claim {
            CLAIM_NULL => name = r.string(MAX_NAME + 1),
            CLAIM_FH => {}
            CLAIM_PREVIOUS => {
                r.u32();
                return ERR_NO_GRACE;
            }
            _ => return ERR_NOT_SUPP,
        }
        if r.failed {
            return ERR_BAD_XDR;
        }
        if access == 0 {
            return ERR_INVAL;
        }

        let (p, dir) = if claim == CLAIM_NULL {
            match self.child(cs, &name) {
                Ok(p) => (p, cs.cur.clone()),
                Err(st) => return st,
            }
        } else {
            if !cs.has_cur || cs.cur.is_empty() {
                return ERR_NO_FILE_HANDLE;
            }
            if how == OPEN_CREATE {
                return ERR_INVAL;
            }
            (cs.cur.clone(), dir_of(&cs.cur).to_vec())
        };

        let before = self.change_of(&dir);
        let mut flags = open_flags(access);
        let exclusive =
            how == OPEN_CREATE && matches!(create_mode, CREATE_EXCLUSIVE | CREATE_EXCLUSIVE1);
        if how == OPEN_CREATE {
            flags |= libc::O_CREAT;
            if create_mode != CREATE_UNCHECKED {
                flags |= libc::O_EXCL;
            }
        }
        let mode = attrs.mode.unwrap_or(0o644);
        let mut created = false;
        let fd = match self.root.open_file(&p, flags, mode) {
            Ok(fd) => {
                created = how == OPEN_CREATE && flags & libc::O_EXCL != 0;
                fd
            }
            // The same create retransmitted, made already.
            Err(e)
                if e.raw_os_error() == Some(libc::EEXIST)
                    && exclusive
                    && self.inner.lock().unwrap().excl.get(&p) == Some(&verifier) =>
            {
                match self.root.open_file(&p, open_flags(access), 0) {
                    Ok(fd) => fd,
                    Err(e) => return errno(&e),
                }
            }
            Err(e) => return errno(&e),
        };
        let fst = match fstat(fd.0) {
            Ok(st) => st,
            Err(e) => return errno(&e),
        };
        match fst.st_mode & libc::S_IFMT {
            libc::S_IFREG => {}
            libc::S_IFDIR => return ERR_IS_DIR,
            _ => return ERR_SYMLINK,
        }
        let mut set = Bitmap::default();
        if how == OPEN_CREATE {
            if exclusive {
                self.inner.lock().unwrap().excl.insert(p.clone(), verifier);
            }
            if !created {
                attrs.mode = None; // an existing file keeps its mode
            }
            let st;
            (set, st) = self.apply(&p, &attrs);
            if st != NFS_OK {
                return st;
            }
            if created {
                set.set(ATTR_MODE as usize);
            }
        }

        let mut inner = self.inner.lock().unwrap();
        let found = inner
            .states
            .iter()
            .find(|(_, o)| {
                o.kind == Kind::Open && o.client == client_id && o.owner == owner && o.path == p
            })
            .map(|(k, _)| *k);
        let id = match found {
            // The same owner opening the file again: one open, with every
            // access it asked for.
            Some(other) => {
                let st = inner.states.get_mut(&other).unwrap();
                if st.access | access != st.access {
                    match self.root.open_file(&p, open_flags(st.access | access), 0) {
                        Ok(nfd) => {
                            st.file = Arc::new(nfd);
                            st.access |= access;
                        }
                        Err(e) => return errno(&e),
                    }
                }
                st.seqid += 1;
                Stateid {
                    seqid: st.seqid,
                    other,
                }
            }
            None => {
                let other = inner.new_state(Kind::Open, client_id, owner, p.clone(), fd);
                inner.states.get_mut(&other).unwrap().access = access;
                Stateid { seqid: 1, other }
            }
        };
        drop(inner);

        cs.cur = p;
        cs.cur_state = id;
        write_stateid(w, id);
        write_change_info(w, before, self.change_of(&dir));
        w.u32(OPEN_RESULT_LOCKTYPE_POSIX);
        set.trim();
        w.bitmap(&set);
        w.u32(DELEGATE_NONE);
        NFS_OK
    }

    pub(crate) fn close_state(&self, cs: &mut Cstate, r: &mut Reader, w: &mut Writer) -> u32 {
        r.u32();
        let id = read_stateid(r);
        let mut inner = self.inner.lock().unwrap();
        let other = match resolve_kind(&inner, cs, id, Kind::Open) {
            Ok(o) => o,
            Err(st) => return st,
        };
        // Its lock owners' locks go with it.
        inner
            .states
            .retain(|k, l| *k != other && l.parent != Some(other));
        drop(inner);
        write_stateid(
            w,
            Stateid {
                seqid: u32::MAX,
                other: [0; 12],
            },
        );
        NFS_OK
    }

    pub(crate) fn open_downgrade(&self, cs: &mut Cstate, r: &mut Reader, w: &mut Writer) -> u32 {
        let id = read_stateid(r);
        r.u32();
        let access = r.u32() & SHARE_ACCESS_MASK;
        r.u32();
        let mut inner = self.inner.lock().unwrap();
        let other = match resolve_kind(&inner, cs, id, Kind::Open) {
            Ok(o) => o,
            Err(st) => return st,
        };
        let st = inner.states.get_mut(&other).unwrap();
        if access == 0 || access & !st.access != 0 {
            return ERR_INVAL;
        }
        if access != st.access {
            match self.root.open_file(&st.path, open_flags(access), 0) {
                Ok(fd) => {
                    st.file = Arc::new(fd);
                    st.access = access;
                }
                Err(e) => return errno(&e),
            }
        }
        st.seqid += 1;
        write_stateid(
            w,
            Stateid {
                seqid: st.seqid,
                other,
            },
        );
        NFS_OK
    }

    pub(crate) fn read(&self, cs: &mut Cstate, r: &mut Reader, w: &mut Writer) -> u32 {
        let id = read_stateid(r);
        let (offset, count) = (r.u64(), r.u32());
        if r.failed {
            return ERR_BAD_XDR;
        }
        let f = match self.io_file(cs, id, false) {
            Ok(f) => f,
            Err(st) => return st,
        };
        let mut buf = vec![0u8; count.min(MAX_IO) as usize];
        let n = match read_at(f.0, &mut buf, offset) {
            Ok(n) => n,
            Err(e) => return errno(&e),
        };
        let size = match fstat(f.0) {
            Ok(st) => st.st_size as u64,
            Err(e) => return errno(&e),
        };
        w.bool(offset.saturating_add(n as u64) >= size);
        w.opaque(&buf[..n]);
        NFS_OK
    }

    pub(crate) fn write(&self, cs: &mut Cstate, r: &mut Reader, w: &mut Writer) -> u32 {
        let id = read_stateid(r);
        let offset = r.u64();
        r.u32(); // stable_how: every write is answered as stable
        let data = r.opaque(MAX_IO as usize);
        if r.failed {
            return ERR_BAD_XDR;
        }
        let f = match self.io_file(cs, id, true) {
            Ok(f) => f,
            Err(st) => return st,
        };
        let n = match write_at(f.0, data, offset) {
            Ok(n) => n,
            Err(e) => return errno(&e),
        };
        // Written is kept: the server's root is a disk that outlives the
        // environment only as long as its worker does, which ends with it,
        // or an NFS mount that writes through. So a write asked to be stable
        // is, without a sync per write -- as an export with "async" answers.
        w.u32(n as u32);
        w.u32(FILE_SYNC);
        w.fixed(&self.verf);
        NFS_OK
    }

    /// ALLOCATE (space for a range, growing the file to hold it) and, if
    /// punch, DEALLOCATE (a hole punched in it, its size kept).
    pub(crate) fn allocate(&self, cs: &mut Cstate, r: &mut Reader, punch: bool) -> u32 {
        let id = read_stateid(r);
        let (offset, length) = (r.u64(), r.u64());
        if r.failed {
            return ERR_BAD_XDR;
        }
        const MAX: u64 = i64::MAX as u64;
        if offset > MAX || length > MAX - offset {
            return ERR_INVAL;
        }
        let f = match self.io_file(cs, id, true) {
            Ok(f) => f,
            Err(st) => return st,
        };
        match sys::allocate(f.0, punch, offset as i64, length as i64) {
            Ok(()) => NFS_OK,
            Err(e) => errno(&e),
        }
    }

    pub(crate) fn commit(&self, cs: &mut Cstate, r: &mut Reader, w: &mut Writer) -> u32 {
        r.u64();
        r.u32();
        if !cs.has_cur || cs.cur.is_empty() {
            return ERR_NO_FILE_HANDLE;
        }
        // Every WRITE is stable already.
        w.fixed(&self.verf);
        NFS_OK
    }

    pub(crate) fn lock(&self, cs: &mut Cstate, r: &mut Reader, w: &mut Writer) -> u32 {
        let typ = r.u32();
        r.bool(); // reclaim: there is no grace period to reclaim in
        let (offset, length) = (r.u64(), r.u64());
        let is_new = r.bool();
        let (mut open_id, mut lock_id) = (Stateid::default(), Stateid::default());
        let (mut owner, mut client_id) = (vec![], 0);
        if is_new {
            r.u32();
            open_id = read_stateid(r);
            r.u32();
            client_id = r.u64();
            owner = r.opaque(1024).to_vec();
        } else {
            lock_id = read_stateid(r);
            r.u32();
        }
        if r.failed {
            return ERR_BAD_XDR;
        }
        let (start, n) = match lock_range(offset, length) {
            Ok(x) => x,
            Err(st) => return st,
        };
        if !cs.has_cur || cs.cur.is_empty() {
            return ERR_NO_FILE_HANDLE;
        }

        let (other, file) = {
            let mut inner = self.inner.lock().unwrap();
            if is_new {
                let open = match resolve_kind(&inner, cs, open_id, Kind::Open) {
                    Ok(o) => o,
                    Err(st) => return st,
                };
                let (path, access) = {
                    let o = &inner.states[&open];
                    if o.path != cs.cur {
                        return ERR_BAD_STATE_ID;
                    }
                    (o.path.clone(), o.access)
                };
                let found = inner
                    .states
                    .iter()
                    .find(|(_, l)| {
                        l.kind == Kind::Lock && l.parent == Some(open) && l.owner == owner
                    })
                    .map(|(k, _)| *k);
                let k = match found {
                    Some(k) => k,
                    None => {
                        // The owner's own description: its locks are its
                        // own, and conflict with every other's.
                        let fd = match self.root.open_file(&path, open_flags(access), 0) {
                            Ok(fd) => fd,
                            Err(e) => return errno(&e),
                        };
                        let k = inner.new_state(Kind::Lock, client_id, owner, path, fd);
                        inner.states.get_mut(&k).unwrap().parent = Some(open);
                        k
                    }
                };
                (k, inner.states[&k].file.clone())
            } else {
                let k = match resolve_kind(&inner, cs, lock_id, Kind::Lock) {
                    Ok(k) => k,
                    Err(st) => return st,
                };
                let l = &inner.states[&k];
                if l.path != cs.cur {
                    return ERR_BAD_STATE_ID;
                }
                (k, l.file.clone())
            }
        };

        let mut fl = flock(lock_type(typ), start, n);
        if let Err(e) = fcntl_lock(file.0, OFD_SETLK, &mut fl) {
            if matches!(e.raw_os_error(), Some(libc::EAGAIN) | Some(libc::EACCES)) {
                denied(file.0, flock(lock_type(typ), start, n), w);
                return ERR_DENIED;
            }
            return errno(&e);
        }
        let mut inner = self.inner.lock().unwrap();
        let Some(ls) = inner.states.get_mut(&other) else {
            return ERR_BAD_STATE_ID;
        };
        ls.seqid += 1;
        let id = Stateid {
            seqid: ls.seqid,
            other,
        };
        drop(inner);
        cs.cur_state = id;
        write_stateid(w, id);
        NFS_OK
    }

    pub(crate) fn lockt(&self, cs: &mut Cstate, r: &mut Reader, w: &mut Writer) -> u32 {
        let typ = r.u32();
        let (offset, length) = (r.u64(), r.u64());
        r.u64();
        r.opaque(1024);
        if r.failed {
            return ERR_BAD_XDR;
        }
        let (start, n) = match lock_range(offset, length) {
            Ok(x) => x,
            Err(st) => return st,
        };
        if !cs.has_cur || cs.cur.is_empty() {
            return ERR_NO_FILE_HANDLE;
        }
        let f = match self.root.open_file(&cs.cur, libc::O_RDONLY, 0) {
            Ok(f) => f,
            Err(e) => return errno(&e),
        };
        let mut fl = flock(lock_type(typ), start, n);
        if let Err(e) = fcntl_lock(f.0, OFD_GETLK, &mut fl) {
            return errno(&e);
        }
        if fl.l_type == libc::F_UNLCK as libc::c_short {
            return NFS_OK;
        }
        denied(f.0, flock(lock_type(typ), start, n), w);
        ERR_DENIED
    }

    pub(crate) fn locku(&self, cs: &mut Cstate, r: &mut Reader, w: &mut Writer) -> u32 {
        r.u32();
        r.u32();
        let id = read_stateid(r);
        let (offset, length) = (r.u64(), r.u64());
        if r.failed {
            return ERR_BAD_XDR;
        }
        let (start, n) = match lock_range(offset, length) {
            Ok(x) => x,
            Err(st) => return st,
        };
        let (other, file) = {
            let inner = self.inner.lock().unwrap();
            match resolve_kind(&inner, cs, id, Kind::Lock) {
                Ok(k) => (k, inner.states[&k].file.clone()),
                Err(st) => return st,
            }
        };
        let mut fl = flock(libc::F_UNLCK as libc::c_short, start, n);
        if let Err(e) = fcntl_lock(file.0, OFD_SETLK, &mut fl) {
            return errno(&e);
        }
        let mut inner = self.inner.lock().unwrap();
        let Some(ls) = inner.states.get_mut(&other) else {
            return ERR_BAD_STATE_ID;
        };
        ls.seqid += 1;
        write_stateid(
            w,
            Stateid {
                seqid: ls.seqid,
                other,
            },
        );
        NFS_OK
    }

    pub(crate) fn free_stateid(&self, id: Stateid) -> u32 {
        let mut inner = self.inner.lock().unwrap();
        match inner.states.get(&id.other) {
            None => ERR_BAD_STATE_ID,
            Some(st) if st.kind == Kind::Open => ERR_LOCKS_HELD,
            Some(_) => {
                inner.states.remove(&id.other);
                NFS_OK
            }
        }
    }
}

fn verifier8(r: &mut Reader) -> [u8; 8] {
    let mut v = [0u8; 8];
    let b = r.fixed(8);
    v[..b.len()].copy_from_slice(b);
    v
}
