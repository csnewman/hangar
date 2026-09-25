package profile

import (
	"bufio"
	"context"
	"crypto/rand"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"path"
	"slices"
	"strings"
	"sync"
	"time"

	"golang.org/x/crypto/ssh"

	"github.com/csnewman/hangar/internal/audit"
	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/tunnel"
)

// Tunnels opens streams to environments through the workers connected to
// this replica.
type Tunnels interface {
	Open(workerID string, h tunnel.Header) (net.Conn, error)
	Connected() []string
}

// Sessions holds a session with every running environment whose worker's
// tunnel reaches this replica.
type Sessions struct {
	store   *Store
	tunnels Tunnels
	log     *slog.Logger

	kick chan struct{}

	mu       sync.Mutex
	sessions map[string]*session // by environment
}

func NewSessions(store *Store, tunnels Tunnels, log *slog.Logger) *Sessions {
	return &Sessions{store: store, tunnels: tunnels, log: log, kick: make(chan struct{}, 1),
		sessions: map[string]*session{}}
}

// target is an environment a session is held with.
type target struct {
	env, name, owner, ownerName, worker string
	trusted                             bool
}

// Run keeps sessions with the environments that should have them until
// ctx ends. It looks again every few seconds, and whenever Kick is called.
func (s *Sessions) Run(ctx context.Context) {
	t := time.NewTicker(3 * time.Second)
	defer t.Stop()
	for {
		s.reconcile(ctx)
		select {
		case <-ctx.Done():
			return
		case <-t.C:
		case <-s.kick:
		}
	}
}

// Kick has Run look again at once, as when an environment's phase changes.
func (s *Sessions) Kick() {
	select {
	case s.kick <- struct{}{}:
	default:
	}
}

// Changed tells the sessions of a user's environments that their profile
// changed. An empty user means any might have.
func (s *Sessions) Changed(userID string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	for _, sess := range s.sessions {
		if userID == "" || sess.owner == userID {
			sess.nudge()
		}
	}
}

func (s *Sessions) reconcile(ctx context.Context) {
	workers := s.tunnels.Connected()
	want := map[string]target{}
	if len(workers) > 0 {
		err := s.store.db.Transact(ctx, func(tx db.Tx) error {
			clear(want)
			rows, err := tx.Query(ctx, `SELECT e.id::text, e.name, e.owner_id::text, u.username, e.worker_id::text,
					coalesce((e.spec->>'untrusted')::boolean, false)
				FROM environments e JOIN users u ON u.id = e.owner_id
				WHERE e.phase = 'running' AND e.desired = 'running' AND e.worker_id = ANY($1::uuid[])`, workers)
			if err != nil {
				return err
			}
			defer rows.Close()
			for rows.Next() {
				var t target
				var untrusted bool
				if err := rows.Scan(&t.env, &t.name, &t.owner, &t.ownerName, &t.worker, &untrusted); err != nil {
					return err
				}
				t.trusted = !untrusted
				want[t.env] = t
			}
			return rows.Err()
		})
		if err != nil {
			if ctx.Err() == nil {
				s.log.Warn("profile: listing running environments", "err", err)
			}
			return
		}
	}

	s.mu.Lock()
	defer s.mu.Unlock()
	for id, sess := range s.sessions {
		if t, ok := want[id]; !ok || t != sess.target {
			sess.stop()
			delete(s.sessions, id)
		}
	}
	for id, t := range want {
		if sess, ok := s.sessions[id]; ok && !sess.ended() {
			continue
		}
		sess := &session{target: t, s: s, nudges: make(chan struct{}, 1), done: make(chan struct{}),
			own: map[int64]bool{}}
		sessCtx, cancel := context.WithCancel(ctx)
		sess.cancel = cancel
		s.sessions[id] = sess
		go sess.run(sessCtx)
	}
}

// session is one environment's.
type session struct {
	target
	s      *Sessions
	cancel context.CancelFunc
	nudges chan struct{}
	done   chan struct{}

	wmu  sync.Mutex
	conn net.Conn

	// own are the versions this session's own writes made. They are not
	// sent back: the agent has them, and a later change it has made since
	// must not be overwritten by the echo of an earlier one.
	own map[int64]bool
	// sent is the last version of the profile sent, and paths the shared
	// paths.
	sent  int64
	paths Paths
	// keys are the keys last sent.
	keys     []string
	keysSent bool
}

func (x *session) nudge() {
	select {
	case x.nudges <- struct{}{}:
	default:
	}
}

func (x *session) stop() { x.cancel() }

func (x *session) ended() bool {
	select {
	case <-x.done:
		return true
	default:
		return false
	}
}

func (x *session) send(m Message) error {
	b, err := json.Marshal(m)
	if err != nil {
		return err
	}
	x.wmu.Lock()
	defer x.wmu.Unlock()
	x.conn.SetWriteDeadline(time.Now().Add(30 * time.Second))
	_, err = x.conn.Write(append(b, '\n'))
	return err
}

// run holds the session until it fails or is stopped; the next reconcile
// starts another if the environment still wants one.
func (x *session) run(ctx context.Context) {
	defer close(x.done)
	log := x.s.log.With("environment", x.env)
	err := x.serve(ctx)
	if err != nil && ctx.Err() == nil && !errors.Is(err, io.EOF) {
		log.Warn("profile session ended", "err", err)
	}
	// A lock the environment held goes with the session.
	unlockCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	for _, l := range Locks {
		x.s.store.Unlock(unlockCtx, x.owner, l.Path, x.env)
	}
}

func (x *session) serve(ctx context.Context) error {
	// What arrives from the environment is its owner's doing, through it.
	ctx = audit.WithActor(ctx, audit.Actor{UserID: x.owner, Name: x.ownerName, Via: "environment:" + x.env + " (" + x.name + ")"})
	conn, err := x.s.tunnels.Open(x.worker, tunnel.Header{Kind: tunnel.KindProfile, Environment: x.env})
	if err != nil {
		return err
	}
	x.conn = conn
	defer conn.Close()
	go func() {
		<-ctx.Done()
		conn.Close()
	}()

	incoming := make(chan Message)
	readErr := make(chan error, 1)
	go func() {
		r := bufio.NewReaderSize(conn, 64<<10)
		for {
			line, err := r.ReadBytes('\n')
			if err != nil {
				readErr <- err
				return
			}
			var m Message
			if err := json.Unmarshal(line, &m); err != nil {
				readErr <- fmt.Errorf("a malformed message from the agent: %w", err)
				return
			}
			select {
			case incoming <- m:
			case <-ctx.Done():
				return
			}
		}
	}()

	// What is shared, everything in it, then that it is everything: the
	// agent sends what it has that the profile does not only once it knows
	// what the profile has.
	if err := x.sendPaths(ctx); err != nil {
		return err
	}
	if err := x.sendFiles(ctx); err != nil {
		return err
	}
	if err := x.sendKeys(ctx); err != nil {
		return err
	}
	if err := x.send(Message{Type: TypeSynced}); err != nil {
		return err
	}

	tick := time.NewTicker(5 * time.Second)
	defer tick.Stop()
	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case err := <-readErr:
			return err
		case m := <-incoming:
			if err := x.handle(ctx, m); err != nil {
				return err
			}
		case <-x.nudges:
		case <-tick.C:
		}
		if err := x.sendPaths(ctx); err != nil {
			return err
		}
		if err := x.sendFiles(ctx); err != nil {
			return err
		}
		if err := x.sendKeys(ctx); err != nil {
			return err
		}
	}
}

// sendPaths sends the shared paths when they are not what was last sent.
func (x *session) sendPaths(ctx context.Context) error {
	paths, err := x.s.store.Paths(ctx, x.owner)
	if err != nil {
		return err
	}
	if slices.Equal(paths, x.paths) {
		return nil
	}
	x.paths = paths
	return x.send(Message{Type: TypePaths, Paths: paths})
}

// sendFiles sends what changed since the last it sent.
func (x *session) sendFiles(ctx context.Context) error {
	files, err := x.s.store.Since(ctx, x.owner, x.trusted, x.sent)
	if err != nil {
		return err
	}
	for _, f := range files {
		x.sent = max(x.sent, f.Version)
		if x.own[f.Version] {
			delete(x.own, f.Version)
			continue
		}
		if err := x.send(Message{Type: TypeFile, Path: f.Path, Data: f.Data, Mode: f.Mode,
			Version: f.Version, Deleted: f.Deleted}); err != nil {
			return err
		}
	}
	return nil
}

// sendKeys sends the public keys the environment's SSH agent offers, when
// they are not what was last sent. Keys change without a version, so they
// are read each time.
func (x *session) sendKeys(ctx context.Context) error {
	keys := []string{}
	if x.trusted {
		ks, err := x.s.store.Keys(ctx, x.owner)
		if err != nil {
			return err
		}
		for _, k := range ks {
			keys = append(keys, k.PublicKey)
		}
	}
	if x.keysSent && slices.Equal(keys, x.keys) {
		return nil
	}
	x.keys, x.keysSent = keys, true
	return x.send(Message{Type: TypeKeys, Keys: keys})
}

func (x *session) handle(ctx context.Context, m Message) error {
	log := x.s.log.With("environment", x.env)
	switch m.Type {
	case TypePut, TypeDelete:
		if !x.paths.Synced(m.Path) || (Secret(m.Path) && !x.trusted) {
			return nil
		}
		var f File
		var err error
		if m.Type == TypePut {
			f, err = x.s.store.Put(ctx, x.owner, m.Path, m.Data, m.Mode)
		} else {
			f, err = x.s.store.Delete(ctx, x.owner, m.Path)
		}
		if err == nil {
			x.own[f.Version] = true
		}
		if errors.Is(err, ErrInvalid) || errors.Is(err, ErrNoKey) {
			log.Warn("profile: refused a file from the environment", "path", m.Path, "err", err)
			return nil
		}
		return err
	case TypeLock:
		if _, ok := LockAt(m.Path); !ok {
			return nil
		}
		if m.ID == 0 {
			// A renewal from the holder.
			if m.Held {
				_, err := x.s.store.Lock(ctx, x.owner, m.Path, x.env)
				return err
			}
			return nil
		}
		reply := Message{Type: TypeLocked, ID: m.ID, Path: m.Path}
		if !m.Held {
			// The agent sent what changed under the lock before asking
			// to let it go, and those have been written above.
			if err := x.s.store.Unlock(ctx, x.owner, m.Path, x.env); err != nil {
				return err
			}
			return x.send(reply)
		}
		got, err := x.s.store.Lock(ctx, x.owner, m.Path, x.env)
		if err != nil {
			return err
		}
		if got {
			// The holder starts from the profile as it stands: what it
			// is about to read, another environment may have just
			// written.
			files, err := x.s.store.Files(ctx, x.owner, x.trusted)
			if err != nil {
				return err
			}
			dir := path.Dir(m.Path) + "/"
			for _, f := range files {
				if strings.HasPrefix(f.Path, dir) {
					reply.Files = append(reply.Files, Message{Type: TypeFile, Path: f.Path, Data: f.Data,
						Mode: f.Mode, Version: f.Version, Deleted: f.Deleted})
				}
			}
		}
		reply.Held = got
		return x.send(reply)
	case TypeSign:
		reply := Message{Type: TypeSigned, ID: m.ID}
		sig, err := x.sign(ctx, m)
		fingerprint := ""
		if pub, perr := ssh.ParsePublicKey(m.Key); perr == nil {
			fingerprint = ssh.FingerprintSHA256(pub)
		}
		x.s.store.RecordSignature(ctx, x.owner, fingerprint, err)
		if err != nil {
			reply.Error = err.Error()
		} else {
			reply.Signature = ssh.Marshal(sig)
		}
		return x.send(reply)
	}
	return nil
}

// sign signs with the owner's key that matches the one asked for, as an SSH
// agent would with the flags it was given.
func (x *session) sign(ctx context.Context, m Message) (*ssh.Signature, error) {
	if !x.trusted {
		return nil, errors.New("this environment is not trusted with its owner's keys")
	}
	signers, err := x.s.store.Signers(ctx, x.owner)
	if err != nil {
		return nil, err
	}
	for _, signer := range signers {
		if string(signer.PublicKey().Marshal()) != string(m.Key) {
			continue
		}
		algo := ""
		if signer.PublicKey().Type() == ssh.KeyAlgoRSA {
			switch {
			case m.Flags&4 != 0: // SSH_AGENT_RSA_SHA2_512
				algo = ssh.KeyAlgoRSASHA512
			case m.Flags&2 != 0: // SSH_AGENT_RSA_SHA2_256
				algo = ssh.KeyAlgoRSASHA256
			}
		}
		if as, ok := signer.(ssh.AlgorithmSigner); ok && algo != "" {
			return as.SignWithAlgorithm(rand.Reader, m.Data, algo)
		}
		return signer.Sign(rand.Reader, m.Data)
	}
	return nil, errors.New("no such key")
}
