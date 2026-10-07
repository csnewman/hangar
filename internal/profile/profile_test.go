package profile_test

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"maps"
	"net"
	"os"
	"os/user"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"golang.org/x/crypto/ssh"
	"golang.org/x/crypto/ssh/agent"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/blob"
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
	// files, when set, holds back the files the server sends until it is
	// closed, as a large profile takes a while to arrive.
	files chan struct{}
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
	if g.files != nil {
		return &slowFiles{Conn: server, files: g.files}, nil
	}
	return server, nil
}

// slowFiles holds back each file message written to it until files is
// closed.
type slowFiles struct {
	net.Conn
	files chan struct{}
}

func (c *slowFiles) Write(b []byte) (int, error) {
	var m profile.Message
	if json.Unmarshal(b, &m) == nil && m.Type == profile.TypeFile {
		<-c.files
	}
	return c.Conn.Write(b)
}

type plane struct {
	d *db.DB
	// kernels are each guest's hangar-sync.
	kmu      sync.Mutex
	kernels  map[*profile.Guest]*fakeKernel
	blobs    *blob.Memory
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
	blobs := blob.NewMemory()
	p := &plane{d: d, blobs: blobs, kernels: map[*profile.Guest]*fakeKernel{}, store: profile.NewStore(d, blobs, sealer), homes: map[string]string{}, envNames: map[string]string{}}
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
	return p.envIn(t, name, untrusted, api.PhaseRunning)
}

// envIn makes an environment in the given phase, with a guest for it.
func (p *plane) envIn(t *testing.T, name string, untrusted bool, phase api.Phase) (id, home string, g *profile.Guest) {
	t.Helper()
	ctx := context.Background()
	spec := `{}`
	if untrusted {
		spec = `{"untrusted": true}`
	}
	err := p.d.Transact(ctx, func(tx db.Tx) error {
		return tx.QueryRow(ctx, `INSERT INTO environments (owner_id, name, template_name, spec, image, cpus,
				memory_mib, desired, worker_id, phase)
			VALUES ($1, $2, 't', $3, 'img', 1, 512, 'running', $4, $5) RETURNING id`,
			p.owner, name, spec, p.guests.worker, phase).Scan(&id)
	})
	if err != nil {
		t.Fatal(err)
	}
	home = t.TempDir()
	return id, home, p.guestFor(t, id, home)
}

// guestFor starts a guest for an environment, as its agent does, keeping
// its state beside the home directory. One started again for the same
// environment is the agent restarted.
func (p *plane) guestFor(t *testing.T, id, home string) *profile.Guest {
	t.Helper()
	me, err := user.Current()
	if err != nil {
		t.Fatal(err)
	}
	g := profile.NewGuest(&user.User{Uid: me.Uid, Gid: me.Gid, Username: me.Username, HomeDir: home},
		slog.New(slog.NewTextHandler(io.Discard, nil)))
	if err := g.KeepStateIn(home + ".state.json"); err != nil {
		t.Fatal(err)
	}
	k := serveFakeKernel(t, g, home)
	p.kmu.Lock()
	p.kernels[g] = k
	p.kmu.Unlock()
	p.guests.mu.Lock()
	p.guests.by[id] = g
	p.guests.mu.Unlock()
	return g
}

// kernel is a guest's hangar-sync.
func (p *plane) kernel(g *profile.Guest) *fakeKernel {
	p.kmu.Lock()
	defer p.kmu.Unlock()
	return p.kernels[g]
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
	// Claude's record of an environment is its own, but one without it is
	// not taken through Claude's setup again: its choices are in the
	// profile.
	if err := os.WriteFile(filepath.Join(b, ".claude.json"), []byte(`{"numStartups":3}`), 0o600); err != nil {
		t.Fatal(err)
	}
	for _, home := range []string{a, b} {
		eventually(t, "the profile to reach an environment", func() bool {
			return read(home, ".gitconfig") == "[user]\n\tname = Alice\n"
		})
	}
	eventually(t, "Claude's setup to be marked done", func() bool {
		return strings.Contains(read(a, ".claude.json"), `"hasCompletedOnboarding": true`)
	})
	if got := read(b, ".claude.json"); got != `{"numStartups":3}` {
		t.Errorf("an environment's own record of Claude became %q", got)
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
	// An empty file travels as one.
	os.WriteFile(filepath.Join(b, ".claude", "skills", "review", "EMPTY"), nil, 0o644)
	eventually(t, "a new file in a new directory to reach the other", func() bool {
		return read(a, ".claude/skills/review/SKILL.md") == "# Review\n" && exists(a, ".claude/skills/review/EMPTY")
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

// Claude refreshes its sign-in under a lock directory, which LockFS puts
// to the server before it is made. Held in one environment, it cannot be
// taken in another; taken, it comes with the profile's latest, and let go,
// it sends what changed under it first.
func TestLocksAreTheServers(t *testing.T) {
	p := newPlane(t)
	_, a, ga := p.env(t, "a", false)
	_, b, gb := p.env(t, "b", false)
	const creds = ".claude/.credentials.json"
	const lock = ".claude/.oauth_refresh.lock"
	os.MkdirAll(filepath.Join(a, ".claude"), 0o755)
	if err := os.WriteFile(filepath.Join(a, creds), []byte("old"), 0o600); err != nil {
		t.Fatal(err)
	}
	eventually(t, "the credentials to reach the other", func() bool { return read(b, creds) == "old" })

	held, err := ga.Lock(lock)
	if err != nil || !held {
		t.Fatalf("taking the lock in a: %v, %v", held, err)
	}
	if held, err := gb.Lock(lock); err != nil || held {
		t.Fatalf("b took a lock a holds: %v, %v", held, err)
	}

	// Refreshed under the lock, and let go at once: faster than the file
	// would travel on its own.
	if err := os.WriteFile(filepath.Join(a, creds), []byte("new"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := ga.Unlock(lock); err != nil {
		t.Fatal(err)
	}
	held, err = gb.Lock(lock)
	if err != nil || !held {
		t.Fatalf("taking the lock in b once a let go: %v, %v", held, err)
	}
	if got := read(b, creds); got != "new" {
		t.Fatalf("b took the lock with the credentials still %q", got)
	}
	if err := gb.Unlock(lock); err != nil {
		t.Fatal(err)
	}
}

// A user shares paths of their own, files and directories; what they stop
// sharing stays in each environment as that environment's own.
func TestUserPaths(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	_, a, _ := p.env(t, "a", false)
	_, b, _ := p.env(t, "b", false)
	for _, bad := range []string{"", "/etc/passwd", "../x", ".cache/", ".claude/", ".claude/projects/x", ".vscode-server-oss/data/"} {
		if err := p.store.AddPath(ctx, p.owner, bad); !errors.Is(err, profile.ErrInvalid) {
			t.Errorf("sharing %q: %v", bad, err)
		}
	}
	if err := p.store.AddPath(ctx, p.owner, ".config/nvim/"); err != nil {
		t.Fatal(err)
	}
	if err := p.store.AddPath(ctx, p.owner, ".bash_aliases"); err != nil {
		t.Fatal(err)
	}
	os.MkdirAll(filepath.Join(a, ".config", "nvim", "lua"), 0o755)
	os.WriteFile(filepath.Join(a, ".config", "nvim", "lua", "init.lua"), []byte("-- mine\n"), 0o644)
	os.WriteFile(filepath.Join(a, ".bash_aliases"), []byte("alias ll='ls -l'\n"), 0o644)
	eventually(t, "a shared directory to reach the other", func() bool {
		return read(b, ".config/nvim/lua/init.lua") == "-- mine\n"
	})
	eventually(t, "a shared file to reach the other", func() bool { return read(b, ".bash_aliases") == "alias ll='ls -l'\n" })

	if err := p.store.RemovePath(ctx, p.owner, ".bash_aliases"); err != nil {
		t.Fatal(err)
	}
	time.Sleep(300 * time.Millisecond)
	os.WriteFile(filepath.Join(a, ".bash_aliases"), []byte("changed\n"), 0o644)
	time.Sleep(500 * time.Millisecond)
	if got := read(b, ".bash_aliases"); got != "alias ll='ls -l'\n" {
		t.Errorf("a file the profile stopped sharing still travels: %q", got)
	}
	files, _ := p.store.Files(ctx, p.owner, true)
	for _, f := range files {
		if f.Path == ".bash_aliases" {
			t.Error("the profile kept a file it stopped sharing")
		}
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

// A clone while an environment is still starting signs in with its owner's
// keys: the session opens then, and the agent waits for it rather than
// answering that there are no keys.
func TestSSHAgentServesAStartingEnvironment(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	if _, err := p.store.GenerateKey(ctx, p.owner, "laptop"); err != nil {
		t.Fatal(err)
	}
	_, _, g := p.envIn(t, "a", false, api.PhaseStarting)
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

// What goes from a shared directory in one environment goes from the others:
// a file, a directory and everything in it, and a directory moved aside, as
// Claude moves a skill to .trash.
func TestRemovalsInSharedDirectories(t *testing.T) {
	p := newPlane(t)
	_, a, _ := p.env(t, "a", false)
	_, b, _ := p.env(t, "b", false)
	skills := filepath.Join(a, ".claude", "skills")
	for _, f := range []string{"one/SKILL.md", "one/notes.md", "two/SKILL.md", "three/SKILL.md", "three/ref/deep.md"} {
		os.MkdirAll(filepath.Dir(filepath.Join(skills, f)), 0o755)
		os.WriteFile(filepath.Join(skills, f), []byte(f), 0o644)
	}
	eventually(t, "the skills to reach the other", func() bool {
		return read(b, ".claude/skills/three/ref/deep.md") == "three/ref/deep.md" && read(b, ".claude/skills/one/notes.md") == "one/notes.md"
	})

	os.Remove(filepath.Join(skills, "one", "notes.md"))
	eventually(t, "a file removed from a shared directory to go", func() bool {
		return !exists(b, ".claude/skills/one/notes.md")
	})
	os.RemoveAll(filepath.Join(skills, "two"))
	eventually(t, "a directory removed to go", func() bool { return !exists(b, ".claude/skills/two/SKILL.md") })
	os.MkdirAll(filepath.Join(skills, ".trash"), 0o755)
	if err := os.Rename(filepath.Join(skills, "three"), filepath.Join(skills, ".trash", "three")); err != nil {
		t.Fatal(err)
	}
	eventually(t, "a directory moved aside to go from where it was", func() bool {
		return !exists(b, ".claude/skills/three/SKILL.md") && !exists(b, ".claude/skills/three/ref/deep.md") &&
			read(b, ".claude/skills/.trash/three/ref/deep.md") == "three/ref/deep.md"
	})
	if !exists(b, ".claude/skills/one/SKILL.md") {
		t.Error("a file still there went")
	}
}

// What an environment changes while it has no session -- the server or
// its worker restarting, or the environment rebooting -- is not undone when
// the session comes back: the profile takes it, as it would have.
func TestChangesWhileDisconnected(t *testing.T) {
	t.Run("session", func(t *testing.T) { changesWhileDisconnected(t, false) })
	t.Run("agent restarted", func(t *testing.T) { changesWhileDisconnected(t, true) })
}

func changesWhileDisconnected(t *testing.T, restart bool) {
	ctx := context.Background()
	p := newPlane(t)
	id, a, _ := p.env(t, "a", false)
	_, b, _ := p.env(t, "b", false)
	skills := filepath.Join(a, ".claude", "skills", "x")
	os.MkdirAll(skills, 0o755)
	os.WriteFile(filepath.Join(skills, "gone.md"), []byte("gone"), 0o644)
	os.WriteFile(filepath.Join(skills, "edited.md"), []byte("before"), 0o644)
	eventually(t, "the files to reach the other", func() bool {
		return read(b, ".claude/skills/x/gone.md") == "gone" && read(b, ".claude/skills/x/edited.md") == "before"
	})

	setPhase := func(phase string) {
		err := p.d.Transact(ctx, func(tx db.Tx) error {
			_, err := tx.Exec(ctx, `UPDATE environments SET phase = $2 WHERE id = $1`, id, phase)
			return err
		})
		if err != nil {
			t.Fatal(err)
		}
	}
	setPhase("stopped")
	time.Sleep(4 * time.Second)
	if restart {
		p.guestFor(t, id, a)
	}
	os.Remove(filepath.Join(skills, "gone.md"))
	os.WriteFile(filepath.Join(skills, "edited.md"), []byte("after"), 0o644)
	time.Sleep(500 * time.Millisecond)
	setPhase("running")

	eventually(t, "the changes made while disconnected to reach the other", func() bool {
		return !exists(b, ".claude/skills/x/gone.md") && read(b, ".claude/skills/x/edited.md") == "after"
	})
	if exists(a, ".claude/skills/x/gone.md") || read(a, ".claude/skills/x/edited.md") != "after" {
		t.Errorf("the session undid what was changed while it was away: gone.md there %v, edited.md %q",
			exists(a, ".claude/skills/x/gone.md"), read(a, ".claude/skills/x/edited.md"))
	}
}

// Shared paths may overlap: a directory over a shared file, one inside a
// shared directory is refused as already shared, and what one of two
// overlapping paths shares goes on being shared once the other goes.
func TestOverlappingPaths(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	for path, why := range map[string]string{
		".claude/skills/review/": "inside a default directory",
		".gitconfig":             "a default file",
	} {
		if err := p.store.AddPath(ctx, p.owner, path); !errors.Is(err, profile.ErrInvalid) {
			t.Errorf("sharing %s, %s: %v", path, why, err)
		}
	}
	// Over the default .config/gh/config.yml.
	if err := p.store.AddPath(ctx, p.owner, ".config/"); err != nil {
		t.Fatal(err)
	}
	if err := p.store.AddPath(ctx, p.owner, ".config/"); err != nil {
		t.Errorf("sharing a path again: %v", err)
	}
	if err := p.store.AddPath(ctx, p.owner, ".config/nvim/"); !errors.Is(err, profile.ErrInvalid) {
		t.Errorf("sharing a directory inside one shared: %v", err)
	}
	if err := p.store.AddPath(ctx, p.owner, ".config/nvim/init.lua"); !errors.Is(err, profile.ErrInvalid) {
		t.Errorf("sharing a file inside a shared directory: %v", err)
	}

	_, a, _ := p.env(t, "a", false)
	_, b, _ := p.env(t, "b", false)
	os.MkdirAll(filepath.Join(a, ".config", "gh"), 0o755)
	os.MkdirAll(filepath.Join(a, ".config", "nvim"), 0o755)
	os.WriteFile(filepath.Join(a, ".config", "gh", "config.yml"), []byte("git_protocol: ssh\n"), 0o644)
	os.WriteFile(filepath.Join(a, ".config", "nvim", "init.lua"), []byte("-- one\n"), 0o644)
	eventually(t, "files under overlapping paths to reach the other", func() bool {
		return read(b, ".config/gh/config.yml") == "git_protocol: ssh\n" && read(b, ".config/nvim/init.lua") == "-- one\n"
	})
	files, err := p.store.Files(ctx, p.owner, true)
	if err != nil {
		t.Fatal(err)
	}
	n := 0
	for _, f := range files {
		if f.Path == ".config/gh/config.yml" {
			n++
		}
	}
	if n != 1 {
		t.Errorf("a file under two shared paths is kept %d times", n)
	}

	// The directory goes; the default file it covered stays shared.
	if err := p.store.RemovePath(ctx, p.owner, ".config/"); err != nil {
		t.Fatal(err)
	}
	time.Sleep(300 * time.Millisecond)
	os.WriteFile(filepath.Join(a, ".config", "gh", "config.yml"), []byte("git_protocol: https\n"), 0o644)
	os.WriteFile(filepath.Join(a, ".config", "nvim", "init.lua"), []byte("-- two\n"), 0o644)
	eventually(t, "the default file to go on being shared", func() bool {
		return read(b, ".config/gh/config.yml") == "git_protocol: https\n"
	})
	time.Sleep(500 * time.Millisecond)
	if got := read(b, ".config/nvim/init.lua"); got != "-- one\n" {
		t.Errorf("a file no longer shared still travels: %q", got)
	}
}

// A server shares paths for everyone beside the defaults, as it is set up
// to; a user neither adds nor removes them.
func TestPathsForEveryone(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	if err := p.store.ShareForEveryone([]string{".claude/projects/"}); err == nil {
		t.Error("the server shared a path no one may")
	}
	if err := p.store.ShareForEveryone([]string{".config/nvim/", ".gitconfig"}); err != nil {
		t.Fatal(err)
	}
	paths, err := p.store.Paths(ctx, p.owner)
	if err != nil {
		t.Fatal(err)
	}
	if !paths.Synced(".config/nvim/init.lua") || !paths.Synced(".gitconfig") {
		t.Errorf("the server's paths are not shared: %v", paths)
	}
	if err := p.store.AddPath(ctx, p.owner, ".config/nvim/"); !errors.Is(err, profile.ErrInvalid) {
		t.Errorf("a user shared a path the server shares: %v", err)
	}
	if err := p.store.RemovePath(ctx, p.owner, ".config/nvim/"); !errors.Is(err, profile.ErrNotFound) {
		t.Errorf("a user removed a path the server shares: %v", err)
	}
	_, a, _ := p.env(t, "a", false)
	_, b, _ := p.env(t, "b", false)
	os.MkdirAll(filepath.Join(a, ".config", "nvim"), 0o755)
	os.WriteFile(filepath.Join(a, ".config", "nvim", "init.lua"), []byte("-- everyone\n"), 0o644)
	eventually(t, "a file under the server's path to reach the other environment", func() bool {
		return read(b, ".config/nvim/init.lua") == "-- everyone\n"
	})
}

// The agent offers the owner's keys while the profile's files are still
// arriving, and the guest says when they have all arrived.
func TestSSHAgentServesBeforeTheProfileArrives(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	files := make(chan struct{})
	p.guests.files = files
	key, err := p.store.GenerateKey(ctx, p.owner, "laptop")
	if err != nil {
		t.Fatal(err)
	}
	if _, err := p.store.Put(ctx, p.owner, ".gitconfig", []byte("[user]\n\tname = Alice\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	_, home, g := p.env(t, "a", false)
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
		t.Fatal("synced while the profile's files are held back")
	default:
	}
	if exists(home, ".gitconfig") {
		t.Fatal("the profile's files arrived: the test held none back")
	}

	close(files)
	select {
	case <-g.Synced():
	case <-time.After(10 * time.Second):
		t.Fatal("not synced once the files arrived")
	}
	if !exists(home, ".gitconfig") {
		t.Error("synced without the profile's files")
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

// versions are how many versions the blob store keeps of each of the
// owner's files.
func (p *plane) versions(t *testing.T) map[string]int {
	t.Helper()
	out := map[string]int{}
	for k, n := range p.blobs.Versions() {
		if path, ok := strings.CutPrefix(k, p.owner+"/"); ok {
			out[path] = n
		}
	}
	return out
}

func TestContentsAreKeptInTheBlobStore(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	const creds = ".claude/.credentials.json"
	if _, err := p.store.Put(ctx, p.owner, creds, []byte("token-123"), 0o600); err != nil {
		t.Fatal(err)
	}
	if got := p.versions(t); !maps.Equal(got, map[string]int{creds: 1}) {
		t.Errorf("the blob store holds %v", got)
	}

	// A change keeps the new version and lets go of the one it replaced.
	if _, err := p.store.Put(ctx, p.owner, creds, []byte("token-456"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := p.store.Put(ctx, p.owner, ".claude/settings.json", []byte("{}"), 0o644); err != nil {
		t.Fatal(err)
	}
	if got := p.versions(t); !maps.Equal(got, map[string]int{creds: 1, ".claude/settings.json": 1}) {
		t.Errorf("after a change, the blob store holds %v", got)
	}
	f, err := p.store.File(ctx, p.owner, creds, true)
	if err != nil || string(f.Data) != "token-456" || f.Size != 9 {
		t.Errorf("read back %q (%d bytes), %v", f.Data, f.Size, err)
	}

	// A removed file keeps nothing.
	if _, err := p.store.Delete(ctx, p.owner, ".claude/settings.json"); err != nil {
		t.Fatal(err)
	}
	if got := p.versions(t); !maps.Equal(got, map[string]int{creds: 1}) {
		t.Errorf("after a removal, the blob store holds %v", got)
	}
	if _, err := p.store.File(ctx, p.owner, ".claude/settings.json", true); !errors.Is(err, profile.ErrNotFound) {
		t.Errorf("a removed file: %v", err)
	}
	if _, err := p.store.File(ctx, p.owner, creds, false); !errors.Is(err, profile.ErrNotFound) {
		t.Errorf("a credential read without secrets: %v", err)
	}
	list, err := p.store.List(ctx, p.owner, true)
	if err != nil || len(list) != 2 || list[0].Data != nil || list[0].Size != 9 || !list[1].Deleted {
		t.Errorf("listed %+v, %v", list, err)
	}

	// A write refused leaves nothing behind.
	big := make([]byte, profile.MaxFileSize)
	for i := range profile.MaxProfileSize/profile.MaxFileSize - 1 {
		if _, err := p.store.Put(ctx, p.owner, fmt.Sprintf(".claude/agents/%d.md", i), big, 0o644); err != nil {
			t.Fatal(err)
		}
	}
	before := p.versions(t)
	if _, err := p.store.Put(ctx, p.owner, ".claude/agents/over.md", big, 0o644); !errors.Is(err, profile.ErrInvalid) {
		t.Errorf("a write over the limit: %v", err)
	}
	if got := p.versions(t); !maps.Equal(got, before) {
		t.Errorf("a refused write left %v, before %v", got, before)
	}

	// Unsharing a path lets go of its files.
	if err := p.store.AddPath(ctx, p.owner, ".config/tool/"); err != nil {
		t.Fatal(err)
	}
	if _, err := p.store.Put(ctx, p.owner, ".config/tool/rc", []byte("x"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := p.store.RemovePath(ctx, p.owner, ".config/tool/"); err != nil {
		t.Fatal(err)
	}
	if _, ok := p.versions(t)[".config/tool/rc"]; ok {
		t.Error("an unshared file is still kept")
	}

	// And a user gone takes everything with them.
	sets, err := p.store.SetsOf(ctx, p.owner)
	if err != nil {
		t.Fatal(err)
	}
	if err := p.store.DeleteSets(ctx, sets...); err != nil {
		t.Fatal(err)
	}
	if got := p.versions(t); len(got) != 0 {
		t.Errorf("after the user went, the blob store holds %v", got)
	}

	if _, err := p.store.Put(ctx, p.owner, "../etc/passwd", []byte("x"), 0o644); !errors.Is(err, profile.ErrInvalid) {
		t.Errorf("a path outside the profile: %v", err)
	}
}

func TestFilesUnderADirectory(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	for _, f := range []string{".claude/settings.json", ".claude/agents/a.md", ".claude/.credentials.json", ".gitconfig", ".config/gh/config.yml"} {
		if _, err := p.store.Put(ctx, p.owner, f, []byte("data of "+f), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	// The paths under dir, a removed one marked so.
	paths := func(trusted bool, dir string) []string {
		t.Helper()
		files, err := p.store.FilesUnder(ctx, p.owner, dir, trusted)
		if err != nil {
			t.Fatal(err)
		}
		var out []string
		for _, f := range files {
			if f.Deleted {
				out = append(out, f.Path+"(removed)")
				continue
			}
			if string(f.Data) != "data of "+f.Path {
				t.Errorf("%s holds %q", f.Path, f.Data)
			}
			out = append(out, f.Path)
		}
		return out
	}
	if got := strings.Join(paths(true, ".claude/"), " "); got != ".claude/settings.json .claude/agents/a.md .claude/.credentials.json" {
		t.Errorf("under .claude/: %s", got)
	}
	if got := strings.Join(paths(false, ".claude/"), " "); got != ".claude/settings.json .claude/agents/a.md .claude/.credentials.json(removed)" {
		t.Errorf("under .claude/ for an untrusted environment: %s", got)
	}
	if got := strings.Join(paths(true, ".config/"), " "); got != ".config/gh/config.yml" {
		t.Errorf("under .config/: %s", got)
	}
	if got := strings.Join(paths(true, ".c%/"), " "); got != "" {
		t.Errorf("a pattern character matched: %s", got)
	}
}

// A file's mode and whether it is trusted-only are its settings: changing
// them reaches environments as a change to it does, and an untrusted
// environment loses a file made trusted-only, and gets one that stops being.
func TestFileSettings(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	if _, err := p.store.Put(ctx, p.owner, ".gitconfig", []byte("x"), 0o644); err != nil {
		t.Fatal(err)
	}
	f, err := p.store.Put(ctx, p.owner, ".netrc", []byte("machine m"), 0)
	if err != nil {
		t.Fatal(err)
	}
	if !f.TrustedOnly || f.Mode != 0o600 {
		t.Errorf("a known credential starts as %#o, trusted-only %t", f.Mode, f.TrustedOnly)
	}
	_, trustedHome, _ := p.env(t, "dev", false)
	_, untrustedHome, _ := p.env(t, "review", true)
	eventually(t, "the profile to arrive", func() bool {
		return read(trustedHome, ".netrc") == "machine m" && read(untrustedHome, ".gitconfig") == "x"
	})
	if exists(untrustedHome, ".netrc") {
		t.Error("an untrusted environment was given a trusted-only file")
	}
	mode := func(home, rel string) os.FileMode {
		st, err := os.Stat(filepath.Join(home, rel))
		if err != nil {
			return 0
		}
		return st.Mode().Perm()
	}

	// A mode reaches the file as it is.
	if _, err := p.store.SetSettings(ctx, p.owner, ".gitconfig", 0o640, false); err != nil {
		t.Fatal(err)
	}
	eventually(t, "the mode to reach the environments", func() bool {
		return mode(trustedHome, ".gitconfig") == 0o640 && mode(untrustedHome, ".gitconfig") == 0o640
	})

	// Trusted-only, it leaves the untrusted environment; not, it arrives.
	if _, err := p.store.SetSettings(ctx, p.owner, ".gitconfig", 0o640, true); err != nil {
		t.Fatal(err)
	}
	eventually(t, "a trusted-only file to leave the untrusted environment", func() bool {
		return !exists(untrustedHome, ".gitconfig")
	})
	if read(trustedHome, ".gitconfig") != "x" {
		t.Error("a trusted environment lost a trusted-only file")
	}
	if _, err := p.store.SetSettings(ctx, p.owner, ".netrc", 0o600, false); err != nil {
		t.Fatal(err)
	}
	eventually(t, "a file no longer trusted-only to reach the untrusted environment", func() bool {
		return read(untrustedHome, ".netrc") == "machine m"
	})

	// The untrusted environment writing a trusted-only file changes nothing.
	os.WriteFile(filepath.Join(untrustedHome, ".gitconfig"), []byte("from review"), 0o644)
	time.Sleep(time.Second)
	if g, err := p.store.File(ctx, p.owner, ".gitconfig", true); err != nil || string(g.Data) != "x" {
		t.Errorf("an untrusted environment changed a trusted-only file: %q, %v", g.Data, err)
	}

	if _, err := p.store.SetSettings(ctx, p.owner, ".missing", 0o600, false); !errors.Is(err, profile.ErrNotFound) {
		t.Errorf("settings of a file not there: %v", err)
	}
}
