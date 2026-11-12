package profile_test

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"log/slog"
	"net"
	"slices"
	"sync"
	"testing"
	"time"

	"golang.org/x/crypto/ssh"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/packs"
	"github.com/csnewman/hangar/internal/profile"
	"github.com/csnewman/hangar/internal/profile/profiletest"
	"github.com/csnewman/hangar/internal/tunnel"
	"github.com/csnewman/hangar/internal/users"
)

// guests stands in for workers: each environment's profile stream is a
// pipe to a stand-in for its agent.
type guests struct {
	worker string
	mu     sync.Mutex
	by     map[string]*profiletest.Guest
	served map[string]*served
	// sets, when set, holds back the copies the server sends until it is
	// closed.
	sets chan struct{}
}

func (g *guests) Connected() []string { return []string{g.worker} }

func (g *guests) Open(worker string, h tunnel.Header) (net.Conn, error) {
	g.mu.Lock()
	guest := g.by[h.Environment]
	if worker != g.worker || h.Kind != tunnel.KindProfile || guest == nil {
		g.mu.Unlock()
		return nil, errors.New("no such environment")
	}
	server, env := net.Pipe()
	s := g.served[h.Environment]
	if s == nil {
		s = &served{}
		g.served[h.Environment] = s
	}
	s.conns = append(s.conns, server)
	s.wg.Add(1)
	g.mu.Unlock()
	go func() {
		defer s.wg.Done()
		guest.Serve(env)
	}()
	if g.sets != nil {
		return &slowSets{Conn: server, sets: g.sets}, nil
	}
	return server, nil
}

// served are an environment's sessions with its guest.
type served struct {
	conns []net.Conn
	wg    sync.WaitGroup
}

// stop ends an environment's sessions and refuses it more, returning once
// its guest has finished with them.
func (g *guests) stop(id string) {
	g.mu.Lock()
	delete(g.by, id)
	s := g.served[id]
	delete(g.served, id)
	g.mu.Unlock()
	if s == nil {
		return
	}
	for _, c := range s.conns {
		c.Close()
	}
	s.wg.Wait()
}

// slowSets holds back the copies written to it until sets is closed.
type slowSets struct {
	net.Conn
	sets chan struct{}
}

func (c *slowSets) Write(b []byte) (int, error) {
	var m profile.Message
	if json.Unmarshal(b, &m) == nil && m.Type == profile.TypePaths {
		<-c.sets
	}
	return c.Conn.Write(b)
}

type plane struct {
	d      *db.DB
	store  *profile.Store
	packs  *packs.Store
	guests *guests
	owner  string
}

func newPlane(t *testing.T) *plane {
	t.Helper()
	ctx, cancel := context.WithCancel(context.Background())
	t.Cleanup(cancel)
	d := dbtest.Open(t)
	sealer, err := profile.NewSealer(make([]byte, 32))
	if err != nil {
		t.Fatal(err)
	}
	ps, err := packs.NewStore(d, t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	p := &plane{d: d, store: profile.NewStore(d, sealer), packs: ps}
	err = d.Transact(ctx, func(tx db.Tx) error {
		if err := tx.QueryRow(ctx, `INSERT INTO users (username) VALUES ('alice') RETURNING id`).Scan(&p.owner); err != nil {
			return err
		}
		var worker string
		if err := tx.QueryRow(ctx, `INSERT INTO workers (name, credential_hash) VALUES ('w', '\x00') RETURNING id`).Scan(&worker); err != nil {
			return err
		}
		p.guests = &guests{worker: worker, by: map[string]*profiletest.Guest{}, served: map[string]*served{}}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	log := slog.New(slog.NewTextHandler(io.Discard, nil))
	sessions := profile.NewSessions(p.store, ps, p.guests, log)
	sessions.UseAgents(environments.NewManager(d))
	go d.Listen(ctx, func(_, payload string) { sessions.Changed(payload) }, profile.Channel, packs.Channel,
		environments.Channel)
	go sessions.Run(ctx)
	return p
}

// env makes a running environment, kept from what access says.
func (p *plane) env(t *testing.T, name string, access api.Access) (id string, g *profiletest.Guest) {
	t.Helper()
	return p.envIn(t, name, api.Spec{Access: access}, api.PhaseRunning)
}

// envIn makes an environment with spec in the given phase, with a guest
// for it.
func (p *plane) envIn(t *testing.T, name string, spec api.Spec, phase api.Phase) (id string, g *profiletest.Guest) {
	t.Helper()
	ctx := context.Background()
	raw, err := json.Marshal(spec)
	if err != nil {
		t.Fatal(err)
	}
	err = p.d.Transact(ctx, func(tx db.Tx) error {
		return tx.QueryRow(ctx, `INSERT INTO environments (owner_id, name, template_name, spec, image, cpus,
				memory_mib, desired, worker_id, phase)
			VALUES ($1, $2, 't', $3, 'img', 1, 512, 'running', $4, $5) RETURNING id`,
			p.owner, name, raw, p.guests.worker, phase).Scan(&id)
	})
	if err != nil {
		t.Fatal(err)
	}
	g = profiletest.New()
	p.guests.mu.Lock()
	p.guests.by[id] = g
	p.guests.mu.Unlock()
	t.Cleanup(func() { p.guests.stop(id) })
	return id, g
}

func as(id string) users.Principal { return users.Principal{UserID: id} }

func eventually(t *testing.T, what string, ok func() bool) {
	t.Helper()
	deadline := time.Now().Add(15 * time.Second)
	for !ok() {
		if time.Now().After(deadline) {
			t.Fatalf("timed out waiting for %s", what)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// The owner's keys reach the environment's agent, and the server signs
// with them.
func TestKeysAndSignatures(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	key, err := p.store.GenerateKey(ctx, p.owner, "laptop")
	if err != nil {
		t.Fatal(err)
	}
	_, g := p.env(t, "a", api.Access{})
	var keys []string
	eventually(t, "the keys", func() bool {
		var ok bool
		keys, ok = g.Keys()
		return ok
	})
	if !slices.Equal(keys, []string{key.PublicKey}) {
		t.Fatalf("keys %q, want %q", keys, key.PublicKey)
	}
	pub, _, _, _, err := ssh.ParseAuthorizedKey([]byte(key.PublicKey))
	if err != nil {
		t.Fatal(err)
	}
	data := []byte("session to sign")
	b, err := g.Sign(pub.Marshal(), data)
	if err != nil {
		t.Fatal(err)
	}
	var sig ssh.Signature
	if err := ssh.Unmarshal(b, &sig); err != nil {
		t.Fatal(err)
	}
	if err := pub.Verify(data, &sig); err != nil {
		t.Fatalf("the signature does not verify: %v", err)
	}
	other, err := p.store.GenerateKey(ctx, p.owner, "other")
	if err != nil {
		t.Fatal(err)
	}
	if err := p.store.DeleteKey(ctx, p.owner, other.ID); err != nil {
		t.Fatal(err)
	}
	stranger, _, _, _, _ := ssh.ParseAuthorizedKey([]byte(other.PublicKey))
	if _, err := g.Sign(stranger.Marshal(), data); err == nil {
		t.Error("signed with a key the owner does not have")
	}
}

// A clone while an environment is still starting signs in with its owner's
// keys: the session opens then.
func TestKeysReachAStartingEnvironment(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	if _, err := p.store.GenerateKey(ctx, p.owner, "laptop"); err != nil {
		t.Fatal(err)
	}
	_, g := p.envIn(t, "a", api.Spec{}, api.PhaseStarting)
	eventually(t, "the keys of a starting environment", func() bool {
		keys, _ := g.Keys()
		return len(keys) == 1
	})
}

// The keys come before the shared files, so a large profile does not hold
// them up.
func TestKeysComeBeforeTheFiles(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	sets := make(chan struct{})
	p.guests.sets = sets
	if _, err := p.store.GenerateKey(ctx, p.owner, "laptop"); err != nil {
		t.Fatal(err)
	}
	_, g := p.env(t, "a", api.Access{})
	eventually(t, "the keys", func() bool {
		keys, _ := g.Keys()
		return len(keys) == 1
	})
	if _, ok := g.Sets(); ok {
		t.Fatal("the copies arrived while held back")
	}
	close(sets)
	eventually(t, "the copies", func() bool {
		_, ok := g.Sets()
		return ok
	})
}

// An environment not trusted with its owner's credentials gets no keys.
func TestUntrustedEnvironmentsGetNoKeys(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	if _, err := p.store.GenerateKey(ctx, p.owner, "k"); err != nil {
		t.Fatal(err)
	}
	_, g := p.env(t, "review", api.Access{NoSSHKeys: true})
	eventually(t, "the copies", func() bool {
		_, ok := g.Sets()
		return ok
	})
	if keys, ok := g.Keys(); !ok || len(keys) != 0 {
		t.Errorf("an environment kept from its owner's keys was sent %q (sent: %v)", keys, ok)
	}
}
