//go:build linux

package nfs

import (
	"io"
	"math"
	"os"
	"path"

	"golang.org/x/sys/unix"
)

func (s *Server) openDirRead(p string) (int, error) {
	return unix.Openat2(s.rootFD, p, &unix.OpenHow{
		Flags:   unix.O_RDONLY | unix.O_DIRECTORY | unix.O_CLOEXEC,
		Resolve: unix.RESOLVE_BENEATH | unix.RESOLVE_NO_SYMLINKS | unix.RESOLVE_NO_MAGICLINKS,
	})
}

func fileFromFD(fd int, name string) *os.File { return os.NewFile(uintptr(fd), name) }

// openFlags are the open(2) flags for a share access.
func openFlags(access uint32) int {
	switch access & shareAccessMask {
	case shareAccessWrite:
		return unix.O_WRONLY
	case shareAccessBoth:
		return unix.O_RDWR
	}
	return unix.O_RDONLY
}

func (s *Server) newState(kind int, client uint64, owner, p string, f *os.File) *state {
	st := &state{seqid: 1, kind: kind, client: client, owner: owner, path: p, file: f}
	copy(st.other[:], randomBytes(12))
	s.states[st.other] = st
	return st
}

func (st *state) id() stateid { return stateid{seqid: st.seqid, other: st.other} }

// replace gives the state another file, keeping the one before.
func (st *state) replace(f *os.File) {
	st.retired = append(st.retired, st.file)
	st.file = f
}

func (st *state) closeFiles() {
	st.file.Close()
	for _, f := range st.retired {
		f.Close()
	}
}

var (
	anonymousStateid stateid
	bypassStateid    = stateid{seqid: math.MaxUint32, other: [12]byte{
		0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff}}
	currentStateid = stateid{seqid: 1}
)

// resolveStateid is the state a stateid names, the current one standing in
// for the special "current stateid". nil, with nfsOK, is a special stateid
// that names none: I/O without an open.
func (s *Server) resolveStateid(cs *cstate, id stateid) (*state, uint32) {
	if id == currentStateid {
		id = cs.curState
	}
	if id == anonymousStateid || id == bypassStateid {
		return nil, nfsOK
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	st := s.states[id.other]
	if st == nil {
		return nil, errBadStateID
	}
	// Zero is the latest. An older one is the client's view from before a
	// change it has yet to hear of -- an OPEN of the same file as it
	// closes it -- which it learns of from OLD_STATEID.
	switch {
	case id.seqid == 0 || id.seqid == st.seqid:
	case id.seqid < st.seqid:
		return nil, errOldStateID
	default:
		return nil, errBadStateID
	}
	return st, nfsOK
}

// ioFile is the file to read or write the current file through: the
// state's, or for none, one opened for the call, which the caller closes.
func (s *Server) ioFile(cs *cstate, id stateid, write bool) (f *os.File, temp bool, st uint32) {
	if !cs.hasCur || cs.cur == "" {
		return nil, false, errNoFileHandle
	}
	state, st := s.resolveStateid(cs, id)
	if st != nfsOK {
		return nil, false, st
	}
	if state != nil {
		if state.path != cs.cur {
			return nil, false, errBadStateID
		}
		s.mu.Lock()
		defer s.mu.Unlock()
		if state.kind == stateLock {
			state = state.parent
		}
		need := uint32(shareAccessRead)
		if write {
			need = shareAccessWrite
		}
		if state.access&need == 0 {
			return nil, false, errOpenMode
		}
		return state.file, false, nfsOK
	}
	flags := unix.O_RDONLY
	if write {
		flags = unix.O_WRONLY
	}
	fd, err := s.openFile(cs.cur, flags, 0)
	if err != nil {
		return nil, false, errno(err)
	}
	return fileFromFD(fd, cs.cur), true, nfsOK
}

// fcntlLock takes, lets go of or tests a lock on f.
func fcntlLock(f *os.File, cmd int, fl *unix.Flock_t) error {
	rc, err := f.SyscallConn()
	if err != nil {
		return err
	}
	var lerr error
	if err := rc.Control(func(fd uintptr) { lerr = unix.FcntlFlock(fd, cmd, fl) }); err != nil {
		return err
	}
	return lerr
}

func (s *Server) open(cs *cstate, r *reader, w *writer) uint32 {
	r.uint32() // seqid: sessions order calls instead
	access := r.uint32() & shareAccessMask
	r.uint32() // share deny: not enforced
	clientID := r.uint64()
	owner := string(r.opaque(1024))
	how := r.uint32()
	var createMode uint32
	var attrs setAttrs
	var verifier [8]byte
	if how == openCreate {
		createMode = r.uint32()
		switch createMode {
		case createUnchecked, createGuarded:
			var st uint32
			if attrs, st = decodeAttrs(r); st != nfsOK {
				return st
			}
		case createExclusive:
			copy(verifier[:], r.fixed(8))
		case createExclusive1:
			copy(verifier[:], r.fixed(8))
			var st uint32
			if attrs, st = decodeAttrs(r); st != nfsOK {
				return st
			}
		default:
			return errInval
		}
	}
	claim := r.uint32()
	var name string
	switch claim {
	case claimNull:
		name = r.string(maxName + 1)
	case claimFH:
	case claimPrevious:
		r.uint32()
		return errNoGrace
	default:
		return errNotSupp
	}
	if r.err != nil {
		return errBadXDR
	}
	if access == 0 {
		return errInval
	}

	var p, dir string
	if claim == claimNull {
		var st uint32
		if p, st = s.child(cs, name); st != nfsOK {
			return st
		}
		dir = cs.cur
	} else {
		if !cs.hasCur || cs.cur == "" {
			return errNoFileHandle
		}
		p, dir = cs.cur, path.Dir(cs.cur)
		if how == openCreate {
			return errInval
		}
	}

	before := s.changeOf(dir)
	flags := openFlags(access)
	var set bitmap
	created := false
	if how == openCreate {
		flags |= unix.O_CREAT
		switch createMode {
		case createGuarded, createExclusive, createExclusive1:
			flags |= unix.O_EXCL
		}
	}
	mode := uint32(0o644)
	if attrs.mode != nil {
		mode = *attrs.mode
	}
	fd, err := s.openFile(p, flags, mode)
	exclusive := createMode == createExclusive || createMode == createExclusive1
	if err == unix.EEXIST && how == openCreate && exclusive {
		// The same create retransmitted, made already.
		s.mu.Lock()
		v, ok := s.excl[p]
		s.mu.Unlock()
		if ok && v == verifier {
			fd, err = s.openFile(p, openFlags(access), 0)
		}
	} else if err == nil && how == openCreate {
		created = flags&unix.O_EXCL != 0
	}
	if err != nil {
		return errno(err)
	}
	var fst unix.Stat_t
	if err := unix.Fstat(fd, &fst); err != nil {
		unix.Close(fd)
		return errno(err)
	}
	if fst.Mode&unix.S_IFMT != unix.S_IFREG {
		unix.Close(fd)
		if fst.Mode&unix.S_IFMT == unix.S_IFDIR {
			return errIsDir
		}
		return errSymlink
	}
	if how == openCreate {
		if exclusive {
			s.mu.Lock()
			s.excl[p] = verifier
			s.mu.Unlock()
		}
		if !created {
			attrs.mode = nil // an existing file keeps its mode
		}
		var st uint32
		if set, st = s.apply(p, attrs); st != nfsOK {
			unix.Close(fd)
			return st
		}
		if created {
			set.set(attrMode)
		}
	}

	s.mu.Lock()
	var state *state
	for _, o := range s.states {
		if o.kind == stateOpen && o.client == clientID && sameOwner(o.owner, owner) && o.path == p {
			state = o
			break
		}
	}
	if state != nil {
		// The same owner opening the file again: one open, with every
		// access it asked for.
		if state.access|access != state.access {
			nfd, err := s.openFile(p, openFlags(state.access|access), 0)
			if err != nil {
				s.mu.Unlock()
				unix.Close(fd)
				return errno(err)
			}
			state.replace(fileFromFD(nfd, p))
			state.access |= access
		}
		unix.Close(fd)
		state.seqid++
	} else {
		state = s.newState(stateOpen, clientID, owner, p, fileFromFD(fd, p))
		state.access = access
	}
	id := state.id()
	s.mu.Unlock()

	cs.cur, cs.curState = p, id
	writeStateid(w, id)
	writeChangeInfo(w, before, s.changeOf(dir))
	w.uint32(openResultLocktypePosix)
	w.bitmap(set.trim())
	w.uint32(delegateNone)
	return nfsOK
}

func (s *Server) close(cs *cstate, r *reader, w *writer) uint32 {
	r.uint32()
	state, st := s.resolveStateid(cs, readStateid(r))
	if st != nfsOK {
		return st
	}
	if state == nil || state.kind != stateOpen {
		return errBadStateID
	}
	s.mu.Lock()
	// Its lock owners' locks go with it.
	for id, l := range s.states {
		if l.parent == state {
			l.closeFiles()
			delete(s.states, id)
		}
	}
	state.closeFiles()
	delete(s.states, state.other)
	s.mu.Unlock()
	writeStateid(w, stateid{seqid: math.MaxUint32})
	return nfsOK
}

func (s *Server) openDowngrade(cs *cstate, r *reader, w *writer) uint32 {
	id := readStateid(r)
	r.uint32()
	access := r.uint32() & shareAccessMask
	r.uint32()
	state, st := s.resolveStateid(cs, id)
	if st != nfsOK {
		return st
	}
	if state == nil || state.kind != stateOpen {
		return errBadStateID
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if access == 0 || access&^state.access != 0 {
		return errInval
	}
	if access != state.access {
		fd, err := s.openFile(state.path, openFlags(access), 0)
		if err != nil {
			return errno(err)
		}
		state.replace(fileFromFD(fd, state.path))
		state.access = access
	}
	state.seqid++
	writeStateid(w, state.id())
	return nfsOK
}

func (s *Server) read(cs *cstate, r *reader, w *writer) uint32 {
	id := readStateid(r)
	offset, count := r.uint64(), r.uint32()
	if r.err != nil {
		return errBadXDR
	}
	f, temp, st := s.ioFile(cs, id, false)
	if st != nfsOK {
		return st
	}
	if temp {
		defer f.Close()
	}
	buf := make([]byte, min(count, maxIO))
	n, err := f.ReadAt(buf, int64(offset))
	if err != nil && err != io.EOF {
		return errno(err)
	}
	fi, err := f.Stat()
	if err != nil {
		return errno(err)
	}
	w.bool(int64(offset)+int64(n) >= fi.Size())
	w.opaque(buf[:n])
	return nfsOK
}

func (s *Server) write(cs *cstate, r *reader, w *writer) uint32 {
	id := readStateid(r)
	offset := r.uint64()
	r.uint32() // stable_how: every write is answered as stable
	data := r.opaque(maxIO)
	if r.err != nil {
		return errBadXDR
	}
	f, temp, st := s.ioFile(cs, id, true)
	if st != nfsOK {
		return st
	}
	if temp {
		defer f.Close()
	}
	n, err := f.WriteAt(data, int64(offset))
	if err != nil {
		return errno(err)
	}
	// Written is kept: the server's root is a disk that outlives the
	// environment only as long as its worker does, which ends with it, or
	// an NFS mount that writes through. So a write asked to be stable is,
	// without a sync per write -- as an export with "async" answers.
	w.uint32(uint32(n))
	w.uint32(fileSync)
	w.fixed(s.verf[:])
	return nfsOK
}

func (s *Server) commit(cs *cstate, r *reader, w *writer) uint32 {
	r.uint64()
	r.uint32()
	if !cs.hasCur || cs.cur == "" {
		return errNoFileHandle
	}
	// Every WRITE is stable already.
	w.fixed(s.verf[:])
	return nfsOK
}

// lockRange is a LOCK's range as fcntl takes it: a length of all ones runs
// to the end of the file, which fcntl's zero does.
func lockRange(offset, length uint64) (int64, int64, uint32) {
	if length == 0 {
		return 0, 0, errInval
	}
	if length == math.MaxUint64 {
		if offset > math.MaxInt64 {
			return 0, 0, errBadRange
		}
		return int64(offset), 0, nfsOK
	}
	if offset > math.MaxInt64 || length > math.MaxInt64 || offset+length > math.MaxInt64 {
		return 0, 0, errBadRange
	}
	return int64(offset), int64(length), nfsOK
}

func lockType(t uint32) int16 {
	if t == lockRead || t == lockReadW {
		return unix.F_RDLCK
	}
	return unix.F_WRLCK
}

// denied writes LOCK4denied for the lock in the way of fl.
func denied(f *os.File, fl unix.Flock_t, w *writer) {
	fcntlLock(f, unix.F_OFD_GETLK, &fl)
	w.uint64(uint64(fl.Start))
	if fl.Len == 0 {
		w.uint64(math.MaxUint64)
	} else {
		w.uint64(uint64(fl.Len))
	}
	if fl.Type == unix.F_RDLCK {
		w.uint32(lockRead)
	} else {
		w.uint32(lockWrite)
	}
	// Whose it is: another environment's, or a program's, which NFS has
	// no name for.
	w.uint64(0)
	w.opaque(nil)
}

func (s *Server) lock(cs *cstate, r *reader, w *writer) uint32 {
	typ := r.uint32()
	r.bool() // reclaim: there is no grace period to reclaim in
	offset, length := r.uint64(), r.uint64()
	var openID, lockID stateid
	var owner string
	var clientID uint64
	isNew := r.bool()
	if isNew {
		r.uint32()
		openID = readStateid(r)
		r.uint32()
		clientID = r.uint64()
		owner = string(r.opaque(1024))
	} else {
		lockID = readStateid(r)
		r.uint32()
	}
	if r.err != nil {
		return errBadXDR
	}
	start, n, st := lockRange(offset, length)
	if st != nfsOK {
		return st
	}
	if !cs.hasCur || cs.cur == "" {
		return errNoFileHandle
	}

	var ls *state
	if isNew {
		open, st := s.resolveStateid(cs, openID)
		if st != nfsOK {
			return st
		}
		if open == nil || open.kind != stateOpen || open.path != cs.cur {
			return errBadStateID
		}
		s.mu.Lock()
		for _, l := range s.states {
			if l.kind == stateLock && l.parent == open && sameOwner(l.owner, owner) {
				ls = l
			}
		}
		if ls == nil {
			// The owner's own description: its locks are its own,
			// and conflict with every other's.
			fd, err := s.openFile(open.path, openFlags(open.access), 0)
			if err != nil {
				s.mu.Unlock()
				return errno(err)
			}
			ls = s.newState(stateLock, clientID, owner, open.path, fileFromFD(fd, open.path))
			ls.parent = open
		}
		s.mu.Unlock()
	} else {
		l, st := s.resolveStateid(cs, lockID)
		if st != nfsOK {
			return st
		}
		if l == nil || l.kind != stateLock || l.path != cs.cur {
			return errBadStateID
		}
		ls = l
	}

	fl := unix.Flock_t{Type: lockType(typ), Whence: 0, Start: start, Len: n}
	if err := fcntlLock(ls.file, unix.F_OFD_SETLK, &fl); err != nil {
		if err == unix.EAGAIN || err == unix.EACCES {
			denied(ls.file, unix.Flock_t{Type: lockType(typ), Start: start, Len: n}, w)
			return errDenied
		}
		return errno(err)
	}
	s.mu.Lock()
	ls.seqid++
	id := ls.id()
	s.mu.Unlock()
	cs.curState = id
	writeStateid(w, id)
	return nfsOK
}

func (s *Server) lockt(cs *cstate, r *reader, w *writer) uint32 {
	typ := r.uint32()
	offset, length := r.uint64(), r.uint64()
	r.uint64()
	r.opaque(1024)
	if r.err != nil {
		return errBadXDR
	}
	start, n, st := lockRange(offset, length)
	if st != nfsOK {
		return st
	}
	if !cs.hasCur || cs.cur == "" {
		return errNoFileHandle
	}
	fd, err := s.openFile(cs.cur, unix.O_RDONLY, 0)
	if err != nil {
		return errno(err)
	}
	f := fileFromFD(fd, cs.cur)
	defer f.Close()
	fl := unix.Flock_t{Type: lockType(typ), Start: start, Len: n}
	if err := fcntlLock(f, unix.F_OFD_GETLK, &fl); err != nil {
		return errno(err)
	}
	if fl.Type == unix.F_UNLCK {
		return nfsOK
	}
	denied(f, unix.Flock_t{Type: lockType(typ), Start: start, Len: n}, w)
	return errDenied
}

func (s *Server) locku(cs *cstate, r *reader, w *writer) uint32 {
	r.uint32()
	r.uint32()
	id := readStateid(r)
	offset, length := r.uint64(), r.uint64()
	if r.err != nil {
		return errBadXDR
	}
	start, n, st := lockRange(offset, length)
	if st != nfsOK {
		return st
	}
	ls, st := s.resolveStateid(cs, id)
	if st != nfsOK {
		return st
	}
	if ls == nil || ls.kind != stateLock {
		return errBadStateID
	}
	fl := unix.Flock_t{Type: unix.F_UNLCK, Start: start, Len: n}
	if err := fcntlLock(ls.file, unix.F_OFD_SETLK, &fl); err != nil {
		return errno(err)
	}
	s.mu.Lock()
	ls.seqid++
	id = ls.id()
	s.mu.Unlock()
	writeStateid(w, id)
	return nfsOK
}

func (s *Server) freeStateid(id stateid) uint32 {
	s.mu.Lock()
	defer s.mu.Unlock()
	st := s.states[id.other]
	if st == nil {
		return errBadStateID
	}
	if st.kind == stateOpen {
		return errLocksHeld
	}
	st.closeFiles()
	delete(s.states, id.other)
	return nfsOK
}
