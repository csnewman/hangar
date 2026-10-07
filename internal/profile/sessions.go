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
	"slices"
	"strings"
	"sync"
	"time"

	"golang.org/x/crypto/ssh"

	"github.com/csnewman/hangar/internal/api"
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
	// registry is Hangar's registry's host, when it has one.
	registry string

	kick chan struct{}

	mu       sync.Mutex
	sessions map[string]*session // by environment
	// tries are the attempts at an environment's session since it last
	// had one, for the log to say how long it took to open.
	tries map[string]*tries
}

type tries struct {
	first time.Time
	n     int
}

func NewSessions(store *Store, tunnels Tunnels, log *slog.Logger) *Sessions {
	return &Sessions{store: store, tunnels: tunnels, log: log, kick: make(chan struct{}, 1),
		sessions: map[string]*session{}, tries: map[string]*tries{}}
}

// UseRegistry has sessions offer environments credentials for Hangar's
// registry, at host.
func (s *Sessions) UseRegistry(host string) { s.registry = host }

// target is an environment a session is held with.
type target struct {
	env, name, owner, ownerName, worker string
	// access is what its spec keeps from it.
	access api.Access
	// packs are the file packs its spec lists, joined by commas.
	packs string
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

// Changed tells the sessions kept in step with a file set that it changed.
// An empty set means any might have.
func (s *Sessions) Changed(set string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	for _, sess := range s.sessions {
		if set == "" || sess.owner == set || sess.uses(set) {
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
			rows, err := tx.Query(ctx, `SELECT e.id::text, e.name, e.owner_id::text, u.username, e.worker_id::text, e.spec
				FROM environments e JOIN users u ON u.id = e.owner_id
				WHERE e.phase IN ('starting', 'running') AND e.desired = 'running' AND e.worker_id = ANY($1::uuid[])`, workers)
			if err != nil {
				return err
			}
			defer rows.Close()
			for rows.Next() {
				var t target
				var raw []byte
				if err := rows.Scan(&t.env, &t.name, &t.owner, &t.ownerName, &t.worker, &raw); err != nil {
					return err
				}
				var spec api.Spec
				if err := json.Unmarshal(raw, &spec); err != nil {
					return err
				}
				t.access, t.packs = spec.Access, strings.Join(spec.FilePacks, ",")
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
	for id := range s.tries {
		if _, ok := want[id]; !ok {
			delete(s.tries, id)
		}
	}
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
		tr := s.tries[id]
		if tr == nil {
			tr = &tries{first: time.Now()}
			s.tries[id] = tr
		}
		tr.n++
		sess := &session{target: t, s: s, nudges: make(chan struct{}, 1), done: make(chan struct{}), tries: *tr}
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

	// sets are the file sets the environment is given, the owner's
	// profile first (Store.EnvironmentSets), as last sent.
	sets     []FileSet
	setsSent bool
	// setIDs are the sets' IDs, for Changed.
	smu    sync.Mutex
	setIDs []string
	// keys are the keys last sent.
	keys     []string
	keysSent bool
	// tries are the attempts at a session with the environment, this one
	// included, since it last had one.
	tries tries
}

// uses reports whether the session gives a set other than the profile.
func (x *session) uses(set string) bool {
	x.smu.Lock()
	defer x.smu.Unlock()
	return slices.Contains(x.setIDs, set)
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
	err := x.serve(ctx)
	if err != nil && ctx.Err() == nil && !errors.Is(err, io.EOF) {
		x.s.log.Warn("profile session ended", "environment", x.env, "err", err)
	}
}

func (x *session) serve(ctx context.Context) error {
	conn, err := x.s.tunnels.Open(x.worker, tunnel.Header{Kind: tunnel.KindProfile, Environment: x.env})
	if err != nil {
		return err
	}
	x.conn = conn
	log := x.s.log.With("environment", x.env)
	x.s.mu.Lock()
	delete(x.s.tries, x.env)
	x.s.mu.Unlock()
	log.Info("profile session opened", "attempts", x.tries.n, "waited", time.Since(x.tries.first).Round(time.Millisecond))
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
			// Each is answered on its own: a clone signing in is not
			// held up behind another request.
			if m.Type == TypeSign || m.Type == TypeGetCredential {
				go func() {
					if err := x.handle(ctx, m); err != nil {
						conn.Close()
					}
				}()
				continue
			}
			select {
			case incoming <- m:
			case <-ctx.Done():
				return
			}
		}
	}()

	// The keys first, which a clone as the environment starts waits for;
	// then the shared files, which the agent routes to the worker's.
	if err := x.sendKeys(ctx); err != nil {
		return err
	}
	if x.s.registry != "" && !x.access.NoRegistry {
		if err := x.send(Message{Type: TypeRegistry, Host: x.s.registry}); err != nil {
			return err
		}
	}
	if err := x.sendSets(ctx); err != nil {
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
		case <-incoming:
		case <-x.nudges:
		case <-tick.C:
		}
		if err := x.sendSets(ctx); err != nil {
			return err
		}
		if err := x.sendKeys(ctx); err != nil {
			return err
		}
	}
}

// sendSets looks up the sets again, and sends them when they are not what
// was last sent.
func (x *session) sendSets(ctx context.Context) error {
	var packs []string
	if x.packs != "" {
		packs = strings.Split(x.packs, ",")
	}
	sets, err := x.s.store.environmentSets(ctx, x.owner, api.Spec{Access: x.access, FilePacks: packs})
	if err != nil {
		return err
	}
	ids := make([]string, 0, len(sets))
	for _, s := range sets {
		ids = append(ids, s.ID)
	}
	x.smu.Lock()
	x.setIDs = ids
	x.smu.Unlock()
	if x.setsSent && slices.EqualFunc(sets, x.sets, func(a, b FileSet) bool {
		return a.ID == b.ID && slices.Equal(a.Paths, b.Paths)
	}) {
		return nil
	}
	x.sets, x.setsSent = sets, true
	m := Message{Type: TypePaths}
	for _, s := range sets {
		m.Sets = append(m.Sets, SetPaths{ID: s.ID, Paths: s.Paths})
	}
	return x.send(m)
}

// sendKeys sends the public keys the environment's SSH agent offers, when
// they are not what was last sent. Keys change without a version, so they
// are read each time.
func (x *session) sendKeys(ctx context.Context) error {
	keys := []string{}
	if !x.access.NoSSHKeys {
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
	// What arrives from the environment is its owner's doing, through it.
	ctx = audit.WithActor(ctx, audit.Actor{UserID: x.owner, Name: x.ownerName, Via: "environment:" + x.env + " (" + x.name + ")"})
	switch m.Type {
	case TypeGetCredential:
		reply := Message{Type: TypeCredential, ID: m.ID}
		switch {
		case x.s.registry == "":
			reply.Error = "this server runs no registry"
		case x.access.NoRegistry:
			reply.Error = "this environment's template gives it no credential for the registry"
		default:
			secret, err := x.s.store.IssueRegistryCredential(x.owner, x.env)
			if err != nil {
				reply.Error = err.Error()
			} else {
				reply.Host, reply.Username, reply.Secret = x.s.registry, x.ownerName, secret
			}
		}
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
	if x.access.NoSSHKeys {
		return nil, errors.New("this environment's template gives it none of its owner's SSH keys")
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
