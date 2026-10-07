//go:build linux

package nfs

import (
	"bytes"
	"path"
	"slices"
	"sort"
	"time"

	"golang.org/x/sys/unix"
)

// op runs one operation, reading its arguments from r and writing its
// result to w. A SEQUENCE retransmitted gives the reply it gave before.
func (s *Server) op(cs *cstate, op uint32, r *reader, w *writer) (uint32, []byte) {
	switch op {
	case opExchangeID:
		return s.exchangeID(r, w), nil
	case opCreateSession:
		return s.createSession(r, w), nil
	case opDestroySession:
		return s.destroySession(r), nil
	case opDestroyClientID:
		return s.destroyClientID(r), nil
	case opBindConnToSession:
		return s.bindConn(r, w), nil
	case opSequence:
		return s.sequence(cs, r, w)
	case opReclaimComplete:
		r.bool()
		return nfsOK, nil
	case opSecInfoNoName:
		r.uint32()
		cs.hasCur = false
		secinfo(w)
		return nfsOK, nil
	case opSecInfo:
		r.string(maxName)
		if !cs.hasCur {
			return errNoFileHandle, nil
		}
		cs.hasCur = false
		secinfo(w)
		return nfsOK, nil
	case opPutRootFH, opPutPubFH:
		cs.cur, cs.hasCur = "", true
		return nfsOK, nil
	case opPutFH:
		p, st := s.h.path(r.opaque(128))
		if st == nfsOK {
			st = s.visible(p)
		}
		if st != nfsOK {
			return st, nil
		}
		cs.cur, cs.hasCur = p, true
		return nfsOK, nil
	case opGetFH:
		if !cs.hasCur {
			return errNoFileHandle, nil
		}
		w.opaque(s.h.handle(cs.cur))
		return nfsOK, nil
	case opSaveFH:
		if !cs.hasCur {
			return errNoFileHandle, nil
		}
		cs.save, cs.hasSave = cs.cur, true
		return nfsOK, nil
	case opRestoreFH:
		if !cs.hasSave {
			return errRestoreFH, nil
		}
		cs.cur, cs.hasCur = cs.save, true
		return nfsOK, nil
	case opLookup:
		return s.lookup(cs, r.string(maxName+1)), nil
	case opLookupP:
		if !cs.hasCur {
			return errNoFileHandle, nil
		}
		if cs.cur == "" {
			return errNoEnt, nil
		}
		dir := path.Dir(cs.cur)
		if dir == "." {
			dir = ""
		}
		cs.cur = dir
		return nfsOK, nil
	case opGetattr:
		return s.getattr(cs, r.bitmap(), w), nil
	case opSetattr:
		return s.setattr(cs, r, w), nil
	case opAccess:
		return s.access(cs, r.uint32(), w), nil
	case opReadLink:
		return s.readlink(cs, w), nil
	case opReadDir:
		return s.readdir(cs, r, w), nil
	case opCreate:
		return s.create(cs, r, w), nil
	case opRemove:
		return s.remove(cs, r.string(maxName+1), w), nil
	case opRename:
		return s.rename(cs, r.string(maxName+1), r.string(maxName+1), w), nil
	case opLink:
		return s.link(cs, r.string(maxName+1), w), nil
	case opOpen:
		return s.open(cs, r, w), nil
	case opClose:
		return s.close(cs, r, w), nil
	case opOpenDowngrade:
		return s.openDowngrade(cs, r, w), nil
	case opRead:
		return s.read(cs, r, w), nil
	case opWrite:
		return s.write(cs, r, w), nil
	case opCommit:
		return s.commit(cs, r, w), nil
	case opLock:
		return s.lock(cs, r, w), nil
	case opLockT:
		return s.lockt(cs, r, w), nil
	case opLockU:
		return s.locku(cs, r, w), nil
	case opFreeStateID:
		return s.freeStateid(readStateid(r)), nil
	case opTestStateID:
		n := r.count(1024)
		ids := make([]stateid, n)
		for i := range ids {
			ids[i] = readStateid(r)
		}
		w.uint32(uint32(n))
		s.mu.Lock()
		for _, id := range ids {
			if _, ok := s.states[id.other]; ok {
				w.uint32(nfsOK)
			} else {
				w.uint32(errBadStateID)
			}
		}
		s.mu.Unlock()
		return nfsOK, nil
	case opDelegReturn:
		readStateid(r)
		return nfsOK, nil
	case opDelegPurge:
		r.uint64()
		return nfsOK, nil
	case opBackchannelCtl:
		return errNotSupp, nil
	}
	if known(op) {
		return errNotSupp, nil
	}
	return errOpIllegal, nil
}

// secinfo is the one flavour this server takes: AUTH_SYS.
func secinfo(w *writer) {
	w.uint32(1)
	w.uint32(authSys)
}

func (s *Server) exchangeID(r *reader, w *writer) uint32 {
	var verifier [8]byte
	copy(verifier[:], r.fixed(8))
	owner := string(r.opaque(1024))
	r.uint32() // flags
	switch r.uint32() {
	case sp4MachCred:
		r.bitmap()
		r.bitmap()
	case sp4SSV:
		return errNotSupp
	}
	if r.count(1) == 1 {
		r.string(1024)
		r.string(1024)
		r.int64()
		r.uint32()
	}
	if r.err != nil {
		return errBadXDR
	}
	s.mu.Lock()
	c := s.byOwner[owner]
	if c != nil && c.verifier != verifier {
		// The client restarted: what it held before is gone.
		s.dropClient(c)
		c = nil
	}
	if c == nil {
		c = &client{id: s.newID(), owner: owner, verifier: verifier, renewed: time.Now()}
		s.clients[c.id] = c
		s.byOwner[owner] = c
	}
	flags := uint32(exchgidUseNonPNFS)
	if c.confirmed {
		flags |= exchgidConfirmedR
	}
	w.uint64(c.id)
	w.uint32(c.seq + 1)
	s.mu.Unlock()
	w.uint32(flags)
	w.uint32(sp4None)
	w.uint64(0)                // server_owner.so_minor_id
	w.opaque([]byte("hangar")) // so_major_id
	w.opaque([]byte("hangar")) // server scope
	w.uint32(0)                // no implementation ID
	return nfsOK
}

type channelAttrs struct {
	headerPad, maxReq, maxResp, maxRespCached, maxOps, maxReqs uint32
}

func readChannel(r *reader) channelAttrs {
	a := channelAttrs{r.uint32(), r.uint32(), r.uint32(), r.uint32(), r.uint32(), r.uint32()}
	if r.count(1) == 1 {
		r.uint32()
	}
	return a
}

func writeChannel(w *writer, a channelAttrs) {
	for _, v := range []uint32{a.headerPad, a.maxReq, a.maxResp, a.maxRespCached, a.maxOps, a.maxReqs} {
		w.uint32(v)
	}
	w.uint32(0) // no RDMA
}

func (s *Server) createSession(r *reader, w *writer) uint32 {
	clientID := r.uint64()
	seq := r.uint32()
	r.uint32() // flags: no persistence, no back channel, are given
	fore, back := readChannel(r), readChannel(r)
	r.uint32() // callback program
	n := r.count(16)
	for i := 0; i < n; i++ {
		switch r.uint32() {
		case authSys:
			r.uint32()
			r.string(255)
			r.uint32()
			r.uint32()
			for g := r.count(16); g > 0; g-- {
				r.uint32()
			}
		case 6: // RPCSEC_GSS
			r.uint32()
			r.opaque(1024)
			r.opaque(1024)
		}
	}
	if r.err != nil {
		return errBadXDR
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	c := s.clients[clientID]
	if c == nil {
		return errStaleClientID
	}
	switch {
	case seq == c.seq && c.reply != nil:
		w.b = append(w.b, c.reply...)
		return nfsOK
	case seq != c.seq+1:
		return errSeqMisordered
	}
	se := &session{client: c}
	copy(se.id[:], randomBytes(16))
	fore.headerPad = 0
	fore.maxReq = min(fore.maxReq, maxRecord)
	fore.maxResp = min(fore.maxResp, maxRecord)
	fore.maxRespCached = min(fore.maxRespCached, maxCached)
	fore.maxOps = min(fore.maxOps, maxOps)
	fore.maxReqs = max(1, min(fore.maxReqs, maxSlots))
	back.headerPad = 0
	back.maxReqs = 1
	se.maxOps = fore.maxOps
	se.slots = make([]slot, fore.maxReqs)
	s.sessions[se.id] = se
	c.confirmed = true
	c.renewed = time.Now()
	c.seq = seq

	var res writer
	res.fixed(se.id[:])
	res.uint32(seq)
	res.uint32(0) // flags: no back channel
	writeChannel(&res, fore)
	writeChannel(&res, back)
	c.reply = res.b
	w.b = append(w.b, res.b...)
	return nfsOK
}

func (s *Server) destroySession(r *reader) uint32 {
	var id [16]byte
	copy(id[:], r.fixed(16))
	s.mu.Lock()
	defer s.mu.Unlock()
	if _, ok := s.sessions[id]; !ok {
		return errBadSession
	}
	delete(s.sessions, id)
	return nfsOK
}

func (s *Server) destroyClientID(r *reader) uint32 {
	id := r.uint64()
	s.mu.Lock()
	defer s.mu.Unlock()
	c := s.clients[id]
	if c == nil {
		return errStaleClientID
	}
	for _, se := range s.sessions {
		if se.client == c {
			return errClientIDBusy
		}
	}
	s.dropClient(c)
	return nfsOK
}

func (s *Server) bindConn(r *reader, w *writer) uint32 {
	var id [16]byte
	copy(id[:], r.fixed(16))
	r.uint32() // direction
	r.bool()
	s.mu.Lock()
	_, ok := s.sessions[id]
	s.mu.Unlock()
	if !ok {
		return errBadSession
	}
	w.fixed(id[:])
	w.uint32(1) // the fore channel: there is no back channel
	w.bool(false)
	return nfsOK
}

func (s *Server) sequence(cs *cstate, r *reader, w *writer) (uint32, []byte) {
	var id [16]byte
	copy(id[:], r.fixed(16))
	seq, slotID := r.uint32(), r.uint32()
	r.uint32() // highest slot the client uses
	cache := r.bool()
	if r.err != nil {
		return errBadXDR, nil
	}
	s.mu.Lock()
	se := s.sessions[id]
	if se != nil {
		se.client.renewed = time.Now()
	}
	s.mu.Unlock()
	if se == nil {
		return errBadSession, nil
	}
	if int(slotID) >= len(se.slots) {
		return errBadSlot, nil
	}
	sl := &se.slots[slotID]
	sl.mu.Lock()
	switch {
	case seq == sl.seq && sl.seq != 0:
		replay := sl.reply
		sl.mu.Unlock()
		if replay == nil {
			return errRetryUncached, nil
		}
		return nfsOK, replay
	case seq != sl.seq+1:
		sl.mu.Unlock()
		return errSeqMisordered, nil
	}
	sl.seq = seq
	cs.session, cs.slot, cs.cacheThis = se, sl, cache
	w.fixed(id[:])
	w.uint32(seq)
	w.uint32(slotID)
	w.uint32(uint32(len(se.slots) - 1))
	w.uint32(uint32(len(se.slots) - 1))
	w.uint32(0) // status flags
	return nfsOK, nil
}

// node is the current file's attributes.
func (s *Server) node(p string) (*node, uint32) {
	if p == "" {
		return &node{root: true, fh: s.h.handle("")}, nfsOK
	}
	st, err := s.lstat(p)
	if err != nil {
		return nil, errno(err)
	}
	return &node{st: st, fh: s.h.handle(p)}, nfsOK
}

func (s *Server) lookup(cs *cstate, name string) uint32 {
	if !cs.hasCur {
		return errNoFileHandle
	}
	if st := validName(name); st != nfsOK {
		return st
	}
	p := path.Join(cs.cur, name)
	if cs.cur == "" {
		if !slices.Contains(s.view.Sets(), name) {
			return errNoEnt
		}
		if err := s.ensureSet(name); err != nil {
			return errno(err)
		}
	} else {
		n, st := s.node(cs.cur)
		if st != nfsOK {
			return st
		}
		switch ftype(n.st.Mode) {
		case nf4Dir:
		case nf4Lnk:
			return errSymlink
		default:
			return errNotDir
		}
		if st := s.visible(p); st != nfsOK {
			return st
		}
	}
	if _, err := s.lstat(p); err != nil {
		return errno(err)
	}
	cs.cur = p
	return nfsOK
}

func (s *Server) getattr(cs *cstate, req bitmap, w *writer) uint32 {
	if !cs.hasCur {
		return errNoFileHandle
	}
	n, st := s.node(cs.cur)
	if st != nfsOK {
		return st
	}
	mask, vals := s.encodeAttrs(req, n)
	w.bitmap(mask)
	w.opaque(vals)
	return nfsOK
}

func (s *Server) setattr(cs *cstate, r *reader, w *writer) uint32 {
	readStateid(r)
	a, st := decodeAttrs(r)
	var set bitmap
	if st == nfsOK && !cs.hasCur {
		st = errNoFileHandle
	}
	if st == nfsOK && cs.cur == "" {
		st = errAccess
	}
	if st == nfsOK {
		set, st = s.apply(cs.cur, a)
	}
	// The attributes set are given whether or not it succeeded.
	w.bitmap(set)
	return st
}

// access is which of the asked-for accesses the caller has, by the file's
// mode bits, as a server checking permissions would.
func (s *Server) access(cs *cstate, want uint32, w *writer) uint32 {
	if !cs.hasCur {
		return errNoFileHandle
	}
	n, st := s.node(cs.cur)
	if st != nfsOK {
		return st
	}
	var mode uint32
	switch {
	case n.root:
		mode = 0o5
	case cs.cred.uid == 0:
		mode = 0o7
		if ftype(n.st.Mode) == nf4Reg && n.st.Mode&0o111 == 0 {
			mode = 0o6
		}
	case cs.cred.uid == s.uid:
		mode = (n.st.Mode >> 6) & 7
	case cs.cred.gid == s.gid || slices.Contains(cs.cred.gids, s.gid):
		mode = (n.st.Mode >> 3) & 7
	default:
		mode = n.st.Mode & 7
	}
	var got uint32
	dir := !n.root && ftype(n.st.Mode) == nf4Dir || n.root
	if mode&4 != 0 {
		got |= accessRead
	}
	if mode&2 != 0 {
		got |= accessModify | accessExtend
		if dir {
			got |= accessDelete
		}
	}
	if mode&1 != 0 {
		if dir {
			got |= accessLookup
		} else {
			got |= accessExecute
		}
	}
	w.uint32(want & accessAll)
	w.uint32(want & got)
	return nfsOK
}

func (s *Server) readlink(cs *cstate, w *writer) uint32 {
	if !cs.hasCur {
		return errNoFileHandle
	}
	if cs.cur == "" {
		return errInval
	}
	fd, name, err := s.at(cs.cur)
	if err != nil {
		return errno(err)
	}
	defer unix.Close(fd)
	buf := make([]byte, 4096)
	n, err := unix.Readlinkat(fd, name, buf)
	if err != nil {
		return errno(err)
	}
	w.opaque(buf[:n])
	return nfsOK
}

// names are a directory's entries, by name, but those hidden.
func (s *Server) names(dir string) ([]string, uint32) {
	if dir == "" {
		sets := slices.Clone(s.view.Sets())
		sort.Strings(sets)
		return sets, nfsOK
	}
	fd, err := s.openDirRead(dir)
	if err != nil {
		return nil, errno(err)
	}
	f := fileFromFD(fd, dir)
	defer f.Close()
	names, err := f.Readdirnames(-1)
	if err != nil {
		return nil, errIO
	}
	sort.Strings(names)
	set, rest := split(dir)
	out := names[:0]
	for _, n := range names {
		if !s.view.Hidden(set, path.Join(rest, n)) {
			out = append(out, n)
		}
	}
	return out, nfsOK
}

func (s *Server) readdir(cs *cstate, r *reader, w *writer) uint32 {
	cookie := r.uint64()
	r.fixed(8) // the cookie verifier: a cookie is a position in the sorted names
	dircount := r.uint32()
	maxcount := r.uint32()
	req := r.bitmap()
	if r.err != nil {
		return errBadXDR
	}
	if !cs.hasCur {
		return errNoFileHandle
	}
	if cs.cur != "" {
		n, st := s.node(cs.cur)
		if st != nfsOK {
			return st
		}
		if ftype(n.st.Mode) != nf4Dir {
			return errNotDir
		}
	}
	names, st := s.names(cs.cur)
	if st != nfsOK {
		return st
	}
	if cookie != 0 && cookie < 3 {
		return errBadCookie
	}
	start := 0
	if cookie >= 3 {
		start = int(cookie - 2)
	}
	if start > len(names) {
		return errBadCookie
	}
	if maxcount > maxIO {
		maxcount = maxIO
	}
	var body writer
	body.fixed(make([]byte, 8)) // the cookie verifier
	used := 0
	eof := true
	entries := 0
	for i := start; i < len(names); i++ {
		p := path.Join(cs.cur, names[i])
		if cs.cur == "" {
			if err := s.ensureSet(names[i]); err != nil {
				continue
			}
		}
		n, st := s.node(p)
		if st != nfsOK {
			continue // gone since it was listed
		}
		var e writer
		e.bool(true)
		e.uint64(uint64(i + 3))
		e.string(names[i])
		mask, vals := s.encodeAttrs(req, n)
		e.bitmap(mask)
		e.opaque(vals)
		used += len(names[i]) + 24
		// The reply's header and the end of the list take the rest.
		if len(body.b)+len(e.b)+32 > int(maxcount) || (dircount > 0 && used > int(dircount) && entries > 0) {
			eof = false
			break
		}
		body.b = append(body.b, e.b...)
		entries++
	}
	if entries == 0 && !eof {
		return errTooSmall
	}
	body.bool(false)
	body.bool(eof)
	w.b = append(w.b, body.b...)
	return nfsOK
}

// changeOf is a directory's change attribute, for change_info.
func (s *Server) changeOf(dir string) uint64 {
	if dir == "" {
		return uint64(s.started.Sec)
	}
	st, err := s.lstat(dir)
	if err != nil {
		return 0
	}
	return change(&st)
}

func writeChangeInfo(w *writer, before, after uint64) {
	w.bool(false)
	w.uint64(before)
	w.uint64(after)
}

// child checks a name to make in the current directory, a set's.
func (s *Server) child(cs *cstate, name string) (string, uint32) {
	if !cs.hasCur {
		return "", errNoFileHandle
	}
	if cs.cur == "" {
		return "", errAccess // the root is the sets', which are given, not made
	}
	if st := validName(name); st != nfsOK {
		return "", st
	}
	p := path.Join(cs.cur, name)
	if st := s.visible(p); st != nfsOK {
		return "", errAccess
	}
	return p, nfsOK
}

func (s *Server) create(cs *cstate, r *reader, w *writer) uint32 {
	typ := r.uint32()
	var target string
	var major, minor uint32
	switch typ {
	case nf4Lnk:
		target = r.string(4096)
	case nf4Blk, nf4Chr:
		major, minor = r.uint32(), r.uint32()
	}
	name := r.string(maxName + 1)
	a, st := decodeAttrs(r)
	if st != nfsOK {
		return st
	}
	if r.err != nil {
		return errBadXDR
	}
	p, st := s.child(cs, name)
	if st != nfsOK {
		return st
	}
	fd, err := s.openDir(cs.cur)
	if err != nil {
		return errno(err)
	}
	defer unix.Close(fd)
	before := s.changeOf(cs.cur)
	mode := uint32(0o755)
	if a.mode != nil {
		mode = *a.mode
	}
	switch typ {
	case nf4Dir:
		err = unix.Mkdirat(fd, name, mode)
	case nf4Lnk:
		err = unix.Symlinkat(target, fd, name)
	case nf4FIFO:
		err = unix.Mknodat(fd, name, unix.S_IFIFO|mode, 0)
	case nf4Sock:
		err = unix.Mknodat(fd, name, unix.S_IFSOCK|mode, 0)
	case nf4Blk, nf4Chr:
		_, _ = major, minor
		return errPerm
	default:
		return errBadType
	}
	if err != nil {
		return errno(err)
	}
	// The mode is set as given, not as the server's umask leaves it.
	set, st := s.apply(p, a)
	if st != nfsOK {
		return st
	}
	writeChangeInfo(w, before, s.changeOf(cs.cur))
	w.bitmap(set)
	cs.cur = p
	return nfsOK
}

func (s *Server) remove(cs *cstate, name string, w *writer) uint32 {
	p, st := s.child(cs, name)
	if st != nfsOK {
		return st
	}
	fd, err := s.openDir(cs.cur)
	if err != nil {
		return errno(err)
	}
	defer unix.Close(fd)
	var lst unix.Stat_t
	if err := unix.Fstatat(fd, name, &lst, unix.AT_SYMLINK_NOFOLLOW); err != nil {
		return errno(err)
	}
	before := s.changeOf(cs.cur)
	flags := 0
	if lst.Mode&unix.S_IFMT == unix.S_IFDIR {
		flags = unix.AT_REMOVEDIR
	}
	if err := unix.Unlinkat(fd, name, flags); err != nil {
		return errno(err)
	}
	s.h.removed(p)
	writeChangeInfo(w, before, s.changeOf(cs.cur))
	return nfsOK
}

func (s *Server) rename(cs *cstate, oldName, newName string, w *writer) uint32 {
	if !cs.hasSave {
		return errNoFileHandle
	}
	src := &cstate{cur: cs.save, hasCur: true}
	from, st := s.child(src, oldName)
	if st != nfsOK {
		return st
	}
	to, st := s.child(cs, newName)
	if st != nfsOK {
		return st
	}
	if set, _ := split(from); set != func() string { s, _ := split(to); return s }() {
		return errXDev // a set's files stay in the set
	}
	ofd, err := s.openDir(cs.save)
	if err != nil {
		return errno(err)
	}
	defer unix.Close(ofd)
	nfd, err := s.openDir(cs.cur)
	if err != nil {
		return errno(err)
	}
	defer unix.Close(nfd)
	srcBefore, dstBefore := s.changeOf(cs.save), s.changeOf(cs.cur)
	if err := unix.Renameat(ofd, oldName, nfd, newName); err != nil {
		return errno(err)
	}
	s.h.moved(from, to)
	writeChangeInfo(w, srcBefore, s.changeOf(cs.save))
	writeChangeInfo(w, dstBefore, s.changeOf(cs.cur))
	return nfsOK
}

func (s *Server) link(cs *cstate, name string, w *writer) uint32 {
	if !cs.hasSave {
		return errNoFileHandle
	}
	to, st := s.child(cs, name)
	if st != nfsOK {
		return st
	}
	if cs.save == "" {
		return errIsDir
	}
	if a, _ := split(cs.save); a != func() string { s, _ := split(to); return s }() {
		return errXDev
	}
	ofd, oname, err := s.at(cs.save)
	if err != nil {
		return errno(err)
	}
	defer unix.Close(ofd)
	nfd, err := s.openDir(cs.cur)
	if err != nil {
		return errno(err)
	}
	defer unix.Close(nfd)
	before := s.changeOf(cs.cur)
	if err := unix.Linkat(ofd, oname, nfd, name, 0); err != nil {
		return errno(err)
	}
	writeChangeInfo(w, before, s.changeOf(cs.cur))
	return nfsOK
}

// sameOwner reports whether two owners are the same bytes.
func sameOwner(a, b string) bool { return bytes.Equal([]byte(a), []byte(b)) }
