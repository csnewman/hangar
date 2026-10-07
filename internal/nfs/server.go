//go:build linux

// Package nfs is an NFSv4.1 and 4.2 server for one environment's share of
// files that environments share with others: their file sets, each a
// directory of a root (View). It serves the operations Linux's client uses,
// each a system call on the root, as the client's user says it is (AUTH_SYS):
// within the environment's sets that is the environment's own business.
//
// Locks are open file description locks on the root's files, one
// description for each lock owner's locks on a file: they hold between this
// server's clients and anything else locking the same files, every
// environment on the worker, and through an NFS mount of the root, on every
// worker. A client that stops renewing its lease loses its state, its opens
// and so its locks, as a crashed environment should. There are no
// delegations, and no state outlives the server: an environment's server
// lives as long as the environment does on its worker.
package nfs

import (
	"crypto/rand"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"os"
	"sync"
	"time"

	"golang.org/x/sys/unix"
)

// Config is a server's.
type Config struct {
	// Root holds the file sets, each a directory named by its ID.
	Root string
	View View
	// UID and GID own every file, as the environment sees them: its user,
	// whoever made the file on the root -- the control plane, or another
	// worker. Changing a file's owner is accepted and changes nothing.
	UID, GID uint32
	Log      *slog.Logger
}

// Server serves one environment's view of the root.
type Server struct {
	view     View
	uid, gid uint32
	log      *slog.Logger
	rootFD   int
	h        *handles
	started  unix.Timespec
	verf     [8]byte

	mu       sync.Mutex
	nextID   uint64
	clients  map[uint64]*client
	byOwner  map[string]*client
	sessions map[[16]byte]*session
	states   map[[12]byte]*state
	excl     map[string][8]byte // verifiers of exclusive creates, by path
	closed   bool
	done     chan struct{}
}

type client struct {
	id       uint64
	owner    string
	verifier [8]byte
	// seq is the last CREATE_SESSION's sequence, and reply its reply.
	seq       uint32
	reply     []byte
	confirmed bool
	renewed   time.Time
}

type session struct {
	id     [16]byte
	client *client
	slots  []slot
	maxOps uint32
}

type slot struct {
	mu    sync.Mutex
	seq   uint32
	reply []byte // the last compound's whole reply, when it was cached
}

const (
	stateOpen = iota
	stateLock
)

// state is an open of a file, or a lock owner's locks on one.
type state struct {
	other  [12]byte
	seqid  uint32
	kind   int
	client uint64
	owner  string // the open or lock owner
	path   string
	access uint32 // an open's share access
	// file is the open's, or the lock owner's description. One an open
	// gives up for another, with more or less access, is kept in retired
	// until the open is closed: a READ or WRITE through it may still be
	// on its way.
	file    *os.File
	retired []*os.File
	parent  *state // a lock's open
}

// New starts a server for cfg.View, rooted at cfg.Root.
func New(cfg Config) (*Server, error) {
	fd, err := unix.Open(cfg.Root, unix.O_PATH|unix.O_DIRECTORY|unix.O_CLOEXEC, 0)
	if err != nil {
		return nil, fmt.Errorf("opening %s: %w", cfg.Root, err)
	}
	log := cfg.Log
	if log == nil {
		log = slog.New(slog.DiscardHandler)
	}
	now := time.Now()
	s := &Server{view: cfg.View, uid: cfg.UID, gid: cfg.GID, log: log, rootFD: fd, h: newHandles(),
		started: unix.NsecToTimespec(now.UnixNano()), nextID: uint64(now.Unix()) << 32,
		clients: map[uint64]*client{}, byOwner: map[string]*client{},
		sessions: map[[16]byte]*session{}, states: map[[12]byte]*state{},
		excl: map[string][8]byte{}, done: make(chan struct{})}
	copy(s.verf[:], s.h.boot[:])
	go s.expire()
	return s, nil
}

// Close ends every client's state, closing its files and so letting go of
// its locks. Connections still served are the caller's to close.
func (s *Server) Close() {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return
	}
	s.closed = true
	close(s.done)
	for _, c := range s.clients {
		s.dropClient(c)
	}
	unix.Close(s.rootFD)
}

// expire ends the state of clients that stopped renewing it.
func (s *Server) expire() {
	t := time.NewTicker(leaseTime / 3)
	defer t.Stop()
	for {
		select {
		case <-s.done:
			return
		case <-t.C:
		}
		s.mu.Lock()
		for _, c := range s.clients {
			if time.Since(c.renewed) > leaseTime*3/2 {
				s.log.Info("nfs: a client's lease ran out", "client", c.id)
				s.dropClient(c)
			}
		}
		s.mu.Unlock()
	}
}

// dropClient ends a client: its sessions, opens and locks. s.mu is held.
func (s *Server) dropClient(c *client) {
	for id, st := range s.states {
		if st.client == c.id {
			st.closeFiles()
			delete(s.states, id)
		}
	}
	for id, se := range s.sessions {
		if se.client == c {
			delete(s.sessions, id)
		}
	}
	delete(s.clients, c.id)
	if s.byOwner[c.owner] == c {
		delete(s.byOwner, c.owner)
	}
}

// Serve answers calls on conn until it fails or closes. Calls are answered
// as they finish, not in order, as RPC allows.
func (s *Server) Serve(conn io.ReadWriteCloser) error {
	defer conn.Close()
	var wmu sync.Mutex
	reply := func(b []byte) error {
		wmu.Lock()
		defer wmu.Unlock()
		return writeRecord(conn, b)
	}
	// As many calls at once as a session has slots.
	sem := make(chan struct{}, maxSlots)
	var wg sync.WaitGroup
	defer wg.Wait()
	for {
		rec, err := readRecord(conn)
		if err != nil {
			if errors.Is(err, io.EOF) || errors.Is(err, os.ErrClosed) {
				return nil
			}
			return err
		}
		c, err := parseCall(rec)
		if err != nil {
			s.log.Debug("nfs: a call that does not parse", "err", err, "len", len(rec))
			if len(rec) >= 4 {
				w := replyHeader(be32(rec))
				accepted(w, acceptGarbageArgs)
				if err := reply(w.b); err != nil {
					return err
				}
			}
			continue
		}
		sem <- struct{}{}
		wg.Add(1)
		go func() {
			defer wg.Done()
			defer func() { <-sem }()
			if err := reply(s.answer(c)); err != nil {
				conn.Close()
			}
		}()
	}
}

// answer is a call's whole reply.
func (s *Server) answer(c call) []byte {
	w := replyHeader(c.xid)
	switch {
	case c.reject != nil:
		c.reject(w)
	case c.proc == procNull:
		accepted(w, acceptSuccess)
	default:
		res, ok := s.compound(c)
		if !ok {
			accepted(w, acceptGarbageArgs)
			break
		}
		if res.replay != nil {
			// The reply to the same call, made before: replyHeader's xid
			// is the retransmission's, the same.
			return append(w.b, res.replay...)
		}
		accepted(w, acceptSuccess)
		w.b = append(w.b, res.body...)
	}
	return w.b
}

const (
	maxSlots = 64
	maxOps   = 32
	// maxCached bounds a reply kept for a retransmission.
	maxCached = 64 << 10
)

// cstate is a compound's: its current and saved filehandles, and stateid.
type cstate struct {
	cred      cred
	minor     uint32
	cur, save string
	hasCur    bool
	hasSave   bool
	curState  stateid
	session   *session
	slot      *slot
	cacheThis bool
}

type stateid struct {
	seqid uint32
	other [12]byte
}

func readStateid(r *reader) stateid {
	var id stateid
	id.seqid = r.uint32()
	copy(id.other[:], r.fixed(12))
	return id
}

func writeStateid(w *writer, id stateid) {
	w.uint32(id.seqid)
	w.fixed(id.other[:])
}

type compoundResult struct {
	body   []byte
	replay []byte
}

// compound runs a COMPOUND's operations in turn, until one fails.
func (s *Server) compound(c call) (compoundResult, bool) {
	r := &reader{b: c.args}
	tag := r.opaque(1024)
	minor := r.uint32()
	n := r.count(1 << 16)
	if r.err != nil {
		return compoundResult{}, false
	}
	cs := &cstate{cred: c.cred, minor: minor}
	var res writer
	var status uint32 = nfsOK
	done := 0
	if minor != 1 && minor != 2 {
		status = errMinorVersMis
		n = 0
	}
	for i := 0; i < n; i++ {
		op := r.uint32()
		if r.err != nil {
			status = errBadXDR
			break
		}
		if i == 0 && op != opSequence && !sessionless(op) {
			status = errOpNotInSession
		} else if i > 0 && sessionless(op) && op != opDestroySession {
			status = errNotOnlyOp
		} else if cs.session != nil && uint32(i) >= cs.session.maxOps {
			status = errTooManyOps
		}
		var body writer
		if status == nfsOK {
			var replay []byte
			status, replay = s.op(cs, op, r, &body)
			if replay != nil {
				return compoundResult{replay: replay}, true
			}
			if r.err != nil && status == nfsOK {
				status = errBadXDR
			}
		}
		if !known(op) {
			op = opIllegal
		}
		if status != nfsOK && status != errNoEnt {
			s.log.Debug("nfs: an operation failed", "op", op, "status", status, "path", cs.cur)
		}
		res.uint32(op)
		res.uint32(status)
		res.b = append(res.b, body.b...)
		done++
		if status != nfsOK {
			break
		}
	}
	var out writer
	out.uint32(status)
	out.opaque(tag)
	out.uint32(uint32(done))
	out.b = append(out.b, res.b...)
	if cs.slot != nil {
		// Kept for a retransmission, which gets this same reply.
		if len(out.b) <= maxCached {
			cs.slot.reply = out.b
		} else {
			cs.slot.reply = nil
		}
		cs.slot.mu.Unlock()
	}
	return compoundResult{body: out.b}, true
}

// sessionless are the operations that may come without a SEQUENCE before
// them, alone in their compound.
func sessionless(op uint32) bool {
	switch op {
	case opExchangeID, opCreateSession, opDestroySession, opBindConnToSession, opDestroyClientID:
		return true
	}
	return false
}

func known(op uint32) bool { return op >= opAccess && op <= 75 }

// newID is a fresh client or session ID's part.
func (s *Server) newID() uint64 {
	s.nextID++
	return s.nextID
}

func randomBytes(n int) []byte {
	b := make([]byte, n)
	rand.Read(b)
	return b
}

func be64(b []byte) uint64 { return binary.BigEndian.Uint64(b) }
func be32(b []byte) uint32 { return binary.BigEndian.Uint32(b) }
