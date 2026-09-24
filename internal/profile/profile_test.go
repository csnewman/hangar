package profile_test

import (
	"bytes"
	"context"
	"errors"
	"io"
	"log/slog"
	"net"
	"os"
	"os/user"
	"path/filepath"
	"sync"
	"testing"
	"time"

	"golang.org/x/crypto/ssh"
	"golang.org/x/crypto/ssh/agent"

	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/profile"
	"github.com/csnewman/hangar/internal/tunnel"
)

// guests stands in for workers: each environment's profile stream is a
// pipe to a Guest keeping a home directory of its own.
type guests struct {
	worker string
	mu     sync.Mutex
	by     map[string]*profile.Guest
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
	return server, nil
}

type plane struct {
	d        *db.DB
	store    *profile.Store
	guests   *guests
	owner    string
	homes    map[string]string
	envNames map[string]string
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
	p := &plane{d: d, store: profile.NewStore(d, sealer), homes: map[string]string{}, envNames: map[string]string{}}
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
	sessions := profile.NewSessions(p.store, p.guests, log)
	go d.Listen(ctx, func(_, payload string) { sessions.Changed(payload) }, profile.Channel)
	go sessions.Run(ctx)
	return p
}

// env makes a running environment with a home directory of its own.
func (p *plane) env(t *testing.T, name string, untrusted bool) (id, home string, g *profile.Guest) {
	t.Helper()
	ctx := context.Background()
	spec := `{}`
	if untrusted {
		spec = `{"untrusted": true}`
	}
	err := p.d.Transact(ctx, func(tx db.Tx) error {
		return tx.QueryRow(ctx, `INSERT INTO environments (owner_id, name, template_name, spec, image, cpus,
				memory_mib, desired, worker_id, phase)
			VALUES ($1, $2, 't', $3, 'img', 1, 512, 'running', $4, 'running') RETURNING id`,
			p.owner, name, spec, p.guests.worker).Scan(&id)
	})
	if err != nil {
		t.Fatal(err)
	}
	home = t.TempDir()
	me, err := user.Current()
	if err != nil {
		t.Fatal(err)
	}
	g = profile.NewGuest(&user.User{Uid: me.Uid, Gid: me.Gid, Username: me.Username, HomeDir: home},
		slog.New(slog.NewTextHandler(io.Discard, nil)))
	p.guests.mu.Lock()
	p.guests.by[id] = g
	p.guests.mu.Unlock()
	return id, home, g
}

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

func read(home, rel string) string {
	b, err := os.ReadFile(filepath.Join(home, rel))
	if err != nil {
		return "<" + err.Error() + ">"
	}
	return string(b)
}

func exists(home, rel string) bool {
	_, err := os.Lstat(filepath.Join(home, rel))
	return err == nil
}

func TestFilesFollowTheOwner(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)

	// Written from the web UI before any environment runs.
	if _, err := p.store.Put(ctx, p.owner, ".gitconfig", []byte("[user]\n\tname = Alice\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	_, a, _ := p.env(t, "a", false)
	_, b, _ := p.env(t, "b", false)
	for _, home := range []string{a, b} {
		eventually(t, "the profile to reach an environment", func() bool {
			return read(home, ".gitconfig") == "[user]\n\tname = Alice\n"
		})
	}

	// Changed in one environment, it reaches the other and the profile.
	settings := filepath.Join(a, ".claude", "settings.json")
	if err := os.WriteFile(settings, []byte(`{"model":"opus"}`), 0o644); err != nil {
		t.Fatal(err)
	}
	eventually(t, "a change in one environment to reach the other", func() bool {
		return read(b, ".claude/settings.json") == `{"model":"opus"}`
	})
	skill := filepath.Join(b, ".claude", "skills", "review", "SKILL.md")
	os.MkdirAll(filepath.Dir(skill), 0o755)
	if err := os.WriteFile(skill, []byte("# Review\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	eventually(t, "a new file in a new directory to reach the other", func() bool {
		return read(a, ".claude/skills/review/SKILL.md") == "# Review\n"
	})

	// Removed in one, it goes from the other.
	if err := os.Remove(filepath.Join(b, ".claude", "settings.json")); err != nil {
		t.Fatal(err)
	}
	eventually(t, "a removal to reach the other", func() bool { return !exists(a, ".claude/settings.json") })

	// Files the profile does not hold stay where they are.
	os.MkdirAll(filepath.Join(a, ".claude", "projects"), 0o755)
	os.WriteFile(filepath.Join(a, ".claude", "projects", "log.jsonl"), []byte("x"), 0o644)
	time.Sleep(500 * time.Millisecond)
	if exists(b, ".claude/projects/log.jsonl") {
		t.Error("a file outside the profile was copied")
	}
	files, _ := p.store.Files(ctx, p.owner, true)
	for _, f := range files {
		if f.Path == ".claude/projects/log.jsonl" {
			t.Error("a file outside the profile was stored")
		}
	}
}

// Claude refreshes its sign-in under a lock directory. Taken in one
// environment it is held in the others, and the new credentials reach them
// before they let it go.
func TestLockCarriesAcrossEnvironments(t *testing.T) {
	p := newPlane(t)
	_, a, _ := p.env(t, "a", false)
	_, b, _ := p.env(t, "b", false)
	const creds = ".claude/.credentials.json"
	const lock = ".claude/.oauth_refresh.lock"
	os.MkdirAll(filepath.Join(a, ".claude"), 0o755)
	if err := os.WriteFile(filepath.Join(a, creds), []byte("old"), 0o600); err != nil {
		t.Fatal(err)
	}
	eventually(t, "the credentials to reach the other", func() bool { return read(b, creds) == "old" })
	if st, err := os.Stat(filepath.Join(b, creds)); err != nil || st.Mode().Perm() != 0o600 {
		t.Errorf("credentials arrived as %v, %v", st.Mode(), err)
	}

	if err := os.Mkdir(filepath.Join(a, lock), 0o755); err != nil {
		t.Fatal(err)
	}
	eventually(t, "the lock to be held in the other", func() bool { return exists(b, lock) })
	// A program there waiting on the lock sees it held.
	if err := os.Mkdir(filepath.Join(b, lock), 0o755); !errors.Is(err, os.ErrExist) {
		t.Fatalf("taking the held lock: %v", err)
	}

	if err := os.WriteFile(filepath.Join(a, creds), []byte("new"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.Remove(filepath.Join(a, lock)); err != nil {
		t.Fatal(err)
	}
	eventually(t, "the lock to be let go in the other", func() bool { return !exists(b, lock) })
	if got := read(b, creds); got != "new" {
		t.Fatalf("the lock was let go with the credentials still %q", got)
	}
}

func TestSSHAgentSignsWithTheOwnersKey(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	key, err := p.store.GenerateKey(ctx, p.owner, "laptop")
	if err != nil {
		t.Fatal(err)
	}
	_, _, g := p.env(t, "a", false)
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

// An environment not trusted with its owner's credentials is sent none,
// and its agent signs nothing.
func TestUntrustedEnvironmentsGetNoCredentials(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	if _, err := p.store.Put(ctx, p.owner, ".claude/.credentials.json", []byte("secret"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := p.store.Put(ctx, p.owner, ".gitconfig", []byte("x"), 0o644); err != nil {
		t.Fatal(err)
	}
	if _, err := p.store.GenerateKey(ctx, p.owner, "k"); err != nil {
		t.Fatal(err)
	}
	_, home, g := p.env(t, "review", true)
	eventually(t, "the profile to arrive", func() bool { return read(home, ".gitconfig") == "x" })
	sock := socketPath(t)
	go g.ServeSSHAgent(sock)
	time.Sleep(300 * time.Millisecond)
	if exists(home, ".claude/.credentials.json") {
		t.Error("an untrusted environment was sent credentials")
	}
	conn, err := net.Dial("unix", sock)
	if err != nil {
		t.Fatal(err)
	}
	if keys, err := agent.NewClient(conn).List(); err != nil || len(keys) != 0 {
		t.Errorf("an untrusted environment's agent offers %d keys (%v)", len(keys), err)
	}
	// Written there, a credential is not taken into the profile either.
	os.WriteFile(filepath.Join(home, ".claude", ".credentials.json"), []byte("stolen"), 0o600)
	time.Sleep(300 * time.Millisecond)
	files, _ := p.store.Files(ctx, p.owner, true)
	for _, f := range files {
		if f.Path == ".claude/.credentials.json" && string(f.Data) != "secret" {
			t.Error("an untrusted environment replaced the credentials")
		}
	}
}

func TestSecretsAreSealed(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	if _, err := p.store.Put(ctx, p.owner, ".claude/.credentials.json", []byte("token-123"), 0o600); err != nil {
		t.Fatal(err)
	}
	var raw []byte
	p.d.Transact(ctx, func(tx db.Tx) error {
		return tx.QueryRow(ctx, `SELECT data FROM profile_files WHERE path = '.claude/.credentials.json'`).Scan(&raw)
	})
	if bytes.Contains(raw, []byte("token-123")) {
		t.Fatal("a credential is stored in the clear")
	}
	withoutKey := profile.NewStore(p.d, nil)
	if _, err := withoutKey.Put(ctx, p.owner, ".claude/.credentials.json", []byte("x"), 0o600); !errors.Is(err, profile.ErrNoKey) {
		t.Errorf("a store without a key kept a secret: %v", err)
	}
	if _, err := p.store.Put(ctx, p.owner, "../etc/passwd", []byte("x"), 0o644); !errors.Is(err, profile.ErrInvalid) {
		t.Errorf("a path outside the profile: %v", err)
	}
}
