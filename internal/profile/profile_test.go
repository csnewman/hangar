package profile_test

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
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
	// sets, when set, holds back the file sets the server sends until it
	// is closed.
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

// slowSets holds back the file sets written to it until sets is closed.
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
	rmu      sync.Mutex
	routes   map[string][]profile.Route
	root     string
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
	root := t.TempDir()
	store, err := profile.NewStore(d, root, sealer)
	if err != nil {
		t.Fatal(err)
	}
	p := &plane{d: d, root: root, routes: map[string][]profile.Route{}, store: store, homes: map[string]string{}, envNames: map[string]string{}}
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

// env makes a running environment with a home directory of its own, kept
// from what access says.
func (p *plane) env(t *testing.T, name string, access api.Access) (id, home string, g *profile.Guest) {
	t.Helper()
	return p.envIn(t, name, access, api.PhaseRunning)
}

// envIn makes an environment in the given phase, with a guest for it.
func (p *plane) envIn(t *testing.T, name string, access api.Access, phase api.Phase) (id, home string, g *profile.Guest) {
	t.Helper()
	spec, err := json.Marshal(api.Spec{Access: access})
	if err != nil {
		t.Fatal(err)
	}
	return p.envSpec(t, name, string(spec), phase)
}

// envSpec makes an environment with the given spec, with a guest for it.
func (p *plane) envSpec(t *testing.T, name, spec string, phase api.Phase) (id, home string, g *profile.Guest) {
	t.Helper()
	ctx := context.Background()
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

// guestFor starts a guest for an environment, as its agent does, whose
// routes are recorded rather than applied.
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
		route, slog.New(slog.NewTextHandler(io.Discard, nil)))
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

// A user shares paths of their own, files and directories; what they stop
// sharing stays in each environment as that environment's own.
func TestUserPaths(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	for _, bad := range []string{"", "/etc/passwd", "../x", ".cache/", ".claude/projects/x", ".vscode-server-oss/data/",
		"!.config/x/", "!.claude/projects/", "!../x"} {
		if err := p.store.AddPath(ctx, p.owner, bad); !errors.Is(err, profile.ErrInvalid) {
			t.Errorf("sharing %q: %v", bad, err)
		}
	}
	if err := p.store.AddPath(ctx, p.owner, ".config/nvim/"); err != nil {
		t.Fatal(err)
	}
	if err := p.store.AddPath(ctx, p.owner, "~/.bash_aliases"); err != nil {
		t.Fatal(err)
	}
	if _, err := p.store.Put(ctx, p.owner, ".bash_aliases", []byte("alias ll='ls -l'\n"), 0); err != nil {
		t.Fatal(err)
	}
	if err := p.store.RemovePath(ctx, p.owner, ".bash_aliases"); err != nil {
		t.Fatal(err)
	}
	files, _ := p.store.List(ctx, p.owner, true)
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
	_, _, g := p.envIn(t, "a", api.Access{}, api.PhaseStarting)
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

// A path is left out of a shared directory with "!", ahead of the
// directory and of another shared path in it; Claude's machine state is
// left out of .claude/ already.
func TestExcludedPaths(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	if err := p.store.AddPath(ctx, p.owner, "!.claude/agents/"); err != nil {
		t.Fatal(err)
	}
	paths, err := p.store.Paths(ctx, p.owner)
	if err != nil {
		t.Fatal(err)
	}
	for path, want := range map[string]bool{
		".claude/settings.json":     true,
		".claude/agents/a.md":       false,
		".claude/agents":            false,
		".claude/projects/x/s.json": false,
		".claude/history.jsonl":     false,
	} {
		if got := paths.Synced(path); got != want {
			t.Errorf("%s shared: %v, want %v", path, got, want)
		}
	}
	if err := p.store.AddPath(ctx, p.owner, "!.claude/agents/"); err != nil {
		t.Errorf("leaving a path out again: %v", err)
	}
	if err := p.store.RemovePath(ctx, p.owner, "!.claude/agents/"); err != nil {
		t.Fatal(err)
	}
	if paths, _ = p.store.Paths(ctx, p.owner); !paths.Synced(".claude/agents/a.md") {
		t.Error("a path is still left out once its exclusion is removed")
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
		t.Fatal("synced while the file sets are held back")
	default:
	}

	close(sets)
	select {
	case <-g.Synced():
	case <-time.After(10 * time.Second):
		t.Fatal("not synced once the file sets arrived")
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

// A file's mode and whether it is sensitive are its settings; known
// credentials start sensitive.
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
	if !f.Sensitive || f.Mode != 0o600 {
		t.Errorf("a known credential starts as %#o, sensitive %t", f.Mode, f.Sensitive)
	}
	g, err := p.store.SetSettings(ctx, p.owner, ".gitconfig", 0o640, true)
	if err != nil {
		t.Fatal(err)
	}
	if g.Mode != 0o640 || !g.Sensitive {
		t.Errorf("settings stored as %#o, sensitive %t", g.Mode, g.Sensitive)
	}
	if _, err := p.store.File(ctx, p.owner, ".gitconfig", false); !errors.Is(err, profile.ErrNotFound) {
		t.Errorf("a sensitive file read without trust: %v", err)
	}

	if _, err := p.store.SetSettings(ctx, p.owner, ".missing", 0o600, false); !errors.Is(err, profile.ErrNotFound) {
		t.Errorf("settings of a file not there: %v", err)
	}
}

// onRoot is a file of the owner's profile as the files root has it, or
// "<missing>".
func (p *plane) onRoot(key string) string {
	b, err := os.ReadFile(filepath.Join(p.root, p.owner, key))
	if err != nil {
		return "<missing>"
	}
	return string(b)
}

func TestFilesAreKeptOnTheRoot(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	const creds = ".claude/.credentials.json"
	if _, err := p.store.Put(ctx, p.owner, creds, []byte("token-123"), 0); err != nil {
		t.Fatal(err)
	}
	if got := p.onRoot(creds); got != "token-123" {
		t.Errorf("the root holds %q", got)
	}
	if st, err := os.Stat(filepath.Join(p.root, p.owner, creds)); err != nil || st.Mode().Perm() != 0o600 {
		t.Errorf("a credential is kept as %v (%v), not 0600", st.Mode().Perm(), err)
	}

	// Written again, it is replaced whole, with nothing left beside it.
	if _, err := p.store.Put(ctx, p.owner, creds, []byte("token-456"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := p.store.Put(ctx, p.owner, ".claude/settings.json", []byte("{}"), 0o644); err != nil {
		t.Fatal(err)
	}
	entries, _ := os.ReadDir(filepath.Join(p.root, p.owner, ".claude"))
	if len(entries) != 2 {
		t.Errorf("the profile's .claude holds %d entries, not its 2 files", len(entries))
	}
	f, err := p.store.File(ctx, p.owner, creds, true)
	if err != nil || string(f.Data) != "token-456" || f.Size != 9 || !f.Sensitive {
		t.Errorf("read back %q (%d bytes, sensitive %t), %v", f.Data, f.Size, f.Sensitive, err)
	}
	if _, err := p.store.File(ctx, p.owner, creds, false); !errors.Is(err, profile.ErrNotFound) {
		t.Errorf("a credential read without trust: %v", err)
	}

	// Removed, it is gone.
	if err := p.store.Delete(ctx, p.owner, ".claude/settings.json"); err != nil {
		t.Fatal(err)
	}
	if got := p.onRoot(".claude/settings.json"); got != "<missing>" {
		t.Errorf("a removed file is still kept: %q", got)
	}
	list, err := p.store.List(ctx, p.owner, true)
	if err != nil || len(list) != 1 || list[0].Path != creds || list[0].Data != nil || list[0].Size != 9 {
		t.Errorf("listed %+v, %v", list, err)
	}

	// A write past the limit is refused, and leaves nothing.
	big := make([]byte, profile.MaxFileSize)
	for i := range profile.MaxProfileSize/profile.MaxFileSize - 1 {
		if _, err := p.store.Put(ctx, p.owner, fmt.Sprintf(".claude/agents/%d.md", i), big, 0o644); err != nil {
			t.Fatal(err)
		}
	}
	if _, err := p.store.Put(ctx, p.owner, ".claude/agents/over.md", big, 0o644); !errors.Is(err, profile.ErrInvalid) {
		t.Errorf("a write over the limit: %v", err)
	}
	if got := p.onRoot(".claude/agents/over.md"); got != "<missing>" {
		t.Error("a refused write was kept")
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
	if got := p.onRoot(".config/tool/rc"); got != "<missing>" {
		t.Error("an unshared file is still kept")
	}

	// A link an environment made does not lead a write out of the root.
	outside := t.TempDir()
	os.MkdirAll(filepath.Join(p.root, p.owner, ".config"), 0o755)
	os.Symlink(outside, filepath.Join(p.root, p.owner, ".config", "gh"))
	if _, err := p.store.Put(ctx, p.owner, ".config/gh/config.yml", []byte("x"), 0o644); err == nil {
		t.Error("wrote through a link out of the root")
	}
	if _, err := os.Stat(filepath.Join(outside, "config.yml")); err == nil {
		t.Error("a file was written outside the root")
	}

	// And a user gone takes everything with them.
	sets, err := p.store.SetsOf(ctx, p.owner)
	if err != nil {
		t.Fatal(err)
	}
	if err := p.store.DeleteSets(ctx, sets...); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(filepath.Join(p.root, p.owner)); !os.IsNotExist(err) {
		t.Errorf("the user's set is still kept: %v", err)
	}

	if _, err := p.store.Put(ctx, p.owner, "../etc/passwd", []byte("x"), 0o644); !errors.Is(err, profile.ErrInvalid) {
		t.Errorf("a path outside the profile: %v", err)
	}
}

// Files kept in the blob store, as they were before the files root, are
// copied there.
func TestMigrateFromTheBlobStore(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	blobs := blob.NewMemory()
	version, err := blobs.PutVersion(ctx, p.owner+"/.gitconfig", strings.NewReader("[user]\n"), 7)
	if err != nil {
		t.Fatal(err)
	}
	err = p.d.Transact(ctx, func(tx db.Tx) error {
		if _, err := tx.Exec(ctx, `INSERT INTO file_sets (id, user_id) VALUES ($1, $1) ON CONFLICT DO NOTHING`, p.owner); err != nil {
			return err
		}
		_, err := tx.Exec(ctx, `INSERT INTO set_files (set_id, path, version_id, size, mode, deleted, version)
			VALUES ($1, '.gitconfig', $2, 7, 384, false, 1)`, p.owner, version)
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	for _, want := range []int{1, 0} {
		n, err := p.store.MigrateFrom(ctx, blobs)
		if err != nil || n != want {
			t.Fatalf("copied %d files (%v), want %d", n, err, want)
		}
	}
	if got := p.onRoot(".gitconfig"); got != "[user]\n" {
		t.Errorf("copied %q", got)
	}
	if st, _ := os.Stat(filepath.Join(p.root, p.owner, ".gitconfig")); st.Mode().Perm() != 0o600 {
		t.Errorf("copied with mode %v", st.Mode().Perm())
	}
}
