package profile_test

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"log/slog"
	"net"
	"os"
	"os/user"
	"path/filepath"
	"slices"
	"sync"
	"testing"
	"time"

	"golang.org/x/crypto/ssh"
	"golang.org/x/crypto/ssh/agent"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/packs"
	"github.com/csnewman/hangar/internal/profile"
	"github.com/csnewman/hangar/internal/tunnel"
	"github.com/csnewman/hangar/internal/users"
)

// guests stands in for workers: each environment's profile stream is a
// pipe to a Guest keeping a home directory of its own.
type guests struct {
	worker string
	mu     sync.Mutex
	by     map[string]*profile.Guest
	// sets, when set, holds back the copies the server sends until it is
	// closed.
	sets chan struct{}
}

func (g *guests) Connected() []string { return []string{g.worker} }

func (g *guests) Open(worker string, h tunnel.Header) (net.Conn, error) {
	g.mu.Lock()
	guest := g.by[h.Environment]
	g.mu.Unlock()
	if worker != g.worker || h.Kind != tunnel.KindProfile || guest == nil {
		return nil, errors.New("no such environment")
	}
	server, env := net.Pipe()
	go guest.Serve(env)
	if g.sets != nil {
		return &slowSets{Conn: server, sets: g.sets}, nil
	}
	return server, nil
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
	d *db.DB
	// routes are each environment's, as its guest last applied them.
	rmu    sync.Mutex
	routes map[string][]profile.Route
	// root is the files root, which the guests reach as their shared
	// files.
	root   string
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
	root := t.TempDir()
	ps, err := packs.NewStore(d, root)
	if err != nil {
		t.Fatal(err)
	}
	p := &plane{d: d, root: root, routes: map[string][]profile.Route{}, store: profile.NewStore(d, sealer), packs: ps}
	err = d.Transact(ctx, func(tx db.Tx) error {
		if err := tx.QueryRow(ctx, `INSERT INTO users (username) VALUES ('alice') RETURNING id`).Scan(&p.owner); err != nil {
			return err
		}
		var worker string
		if err := tx.QueryRow(ctx, `INSERT INTO workers (name, credential_hash) VALUES ('w', '\x00') RETURNING id`).Scan(&worker); err != nil {
			return err
		}
		p.guests = &guests{worker: worker, by: map[string]*profile.Guest{}}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	log := slog.New(slog.NewTextHandler(io.Discard, nil))
	sessions := profile.NewSessions(p.store, ps, p.guests, log)
	go d.Listen(ctx, func(_, payload string) { sessions.Changed(payload) }, profile.Channel, packs.Channel)
	go sessions.Run(ctx)
	return p
}

// env makes a running environment with a home directory of its own, kept
// from what access says.
func (p *plane) env(t *testing.T, name string, access api.Access) (id, home string, g *profile.Guest) {
	t.Helper()
	return p.envIn(t, name, api.Spec{Access: access}, api.PhaseRunning, t.TempDir())
}

// envIn makes an environment with spec in the given phase, with a guest
// for it keeping home.
func (p *plane) envIn(t *testing.T, name string, spec api.Spec, phase api.Phase, home string) (id, _ string, g *profile.Guest) {
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
	return id, home, p.guestFor(t, id, home)
}

// guestFor starts a guest for an environment, as its agent does, whose
// routes are recorded rather than applied: its shared files are the files
// root itself.
func (p *plane) guestFor(t *testing.T, id, home string) *profile.Guest {
	t.Helper()
	me, err := user.Current()
	if err != nil {
		t.Fatal(err)
	}
	route := func(routes []profile.Route) error {
		p.rmu.Lock()
		p.routes[id] = routes
		p.rmu.Unlock()
		return nil
	}
	g := profile.NewGuest(&user.User{Uid: me.Uid, Gid: me.Gid, Username: me.Username, HomeDir: home},
		profile.GuestFiles{Mount: func() error { return nil }, Route: route, Shared: p.root, State: t.TempDir()},
		slog.New(slog.NewTextHandler(io.Discard, nil)))
	p.guests.mu.Lock()
	p.guests.by[id] = g
	p.guests.mu.Unlock()
	return g
}

// routesOf are an environment's routes, as its guest last applied them.
func (p *plane) routesOf(id string) []profile.Route {
	p.rmu.Lock()
	defer p.rmu.Unlock()
	return p.routes[id]
}

func (p *plane) routed(id string, r profile.Route) bool { return slices.Contains(p.routesOf(id), r) }

// socketPath is somewhere short enough for a unix socket: a test's own
// temporary directory can be longer than one allows.
func socketPath(t *testing.T) string {
	dir, err := os.MkdirTemp("/tmp", "hp")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(dir) })
	return filepath.Join(dir, "agent.sock")
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

func TestSSHAgentSignsWithTheOwnersKey(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	key, err := p.store.GenerateKey(ctx, p.owner, "laptop")
	if err != nil {
		t.Fatal(err)
	}
	_, _, g := p.env(t, "a", api.Access{})
	sock := socketPath(t)
	go g.ServeSSHAgent(sock)

	var client agent.ExtendedAgent
	var keys []*agent.Key
	eventually(t, "the agent to offer the key", func() bool {
		conn, err := net.Dial("unix", sock)
		if err != nil {
			return false
		}
		client = agent.NewClient(conn)
		keys, err = client.List()
		return err == nil && len(keys) == 1
	})
	pub, _, _, _, err := ssh.ParseAuthorizedKey([]byte(key.PublicKey))
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(keys[0].Blob, pub.Marshal()) {
		t.Fatal("the agent offers a different key")
	}
	data := []byte("session to sign")
	sig, err := client.Sign(pub, data)
	if err != nil {
		t.Fatal(err)
	}
	if err := pub.Verify(data, sig); err != nil {
		t.Fatalf("the signature does not verify: %v", err)
	}
	if err := client.Add(agent.AddedKey{}); err == nil {
		t.Error("the agent took a key")
	}
}

// A clone while an environment is still starting signs in with its owner's
// keys: the session opens then, and the agent waits for it rather than
// answering that there are no keys.
func TestSSHAgentServesAStartingEnvironment(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	if _, err := p.store.GenerateKey(ctx, p.owner, "laptop"); err != nil {
		t.Fatal(err)
	}
	_, _, g := p.envIn(t, "a", api.Spec{}, api.PhaseStarting, t.TempDir())
	sock := socketPath(t)
	go g.ServeSSHAgent(sock)
	var conn net.Conn
	eventually(t, "the agent's socket", func() bool {
		var err error
		conn, err = net.Dial("unix", sock)
		return err == nil
	})
	if keys, err := agent.NewClient(conn).List(); err != nil || len(keys) != 1 {
		t.Fatalf("the agent of a starting environment offers %d keys (%v), want 1", len(keys), err)
	}
}

// The agent offers the owner's keys before the shared files are routed,
// and the guest says when they are.
func TestSSHAgentServesBeforeTheFilesAreRouted(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	sets := make(chan struct{})
	p.guests.sets = sets
	key, err := p.store.GenerateKey(ctx, p.owner, "laptop")
	if err != nil {
		t.Fatal(err)
	}
	id, _, g := p.env(t, "a", api.Access{})
	sock := socketPath(t)
	go g.ServeSSHAgent(sock)
	var conn net.Conn
	eventually(t, "the agent's socket", func() bool {
		var err error
		conn, err = net.Dial("unix", sock)
		return err == nil
	})
	client := agent.NewClient(conn)
	start := time.Now()
	keys, err := client.List()
	if err != nil || len(keys) != 1 {
		t.Fatalf("the agent offers %d keys (%v), want 1", len(keys), err)
	}
	if d := time.Since(start); d > 10*time.Second {
		t.Errorf("listing took %v", d)
	}
	select {
	case <-g.Synced():
		t.Fatal("synced while the copies are held back")
	default:
	}

	close(sets)
	select {
	case <-g.Synced():
	case <-time.After(10 * time.Second):
		t.Fatal("not synced once the copies arrived")
	}
	if len(p.routesOf(id)) == 0 {
		t.Error("synced without routing the shared files")
	}
	pub, _, _, _, err := ssh.ParseAuthorizedKey([]byte(key.PublicKey))
	if err != nil {
		t.Fatal(err)
	}
	data := []byte("session to sign")
	sig, err := client.Sign(pub, data)
	if err != nil {
		t.Fatal(err)
	}
	if err := pub.Verify(data, sig); err != nil {
		t.Fatalf("the signature does not verify: %v", err)
	}
}

// An environment not trusted with its owner's credentials gets no keys:
// its agent signs nothing.
func TestUntrustedEnvironmentsGetNoKeys(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	if _, err := p.store.GenerateKey(ctx, p.owner, "k"); err != nil {
		t.Fatal(err)
	}
	_, _, g := p.env(t, "review", api.Access{NoSSHKeys: true})
	sock := socketPath(t)
	go g.ServeSSHAgent(sock)
	eventually(t, "the guest to be routed", func() bool {
		select {
		case <-g.Synced():
			return true
		default:
			return false
		}
	})
	conn, err := net.Dial("unix", sock)
	if err != nil {
		t.Fatal(err)
	}
	if keys, err := agent.NewClient(conn).List(); err != nil || len(keys) != 0 {
		t.Errorf("an environment kept from its owner's keys offers %d keys (%v)", len(keys), err)
	}
}
