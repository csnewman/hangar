package profile

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"log/slog"
	"net"
	"os"
	"os/user"
	"path"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"

	"golang.org/x/crypto/ssh"
	"golang.org/x/crypto/ssh/agent"
)

// Guest is the agent's half. The files its owner's profile and the
// environment's packs share are served by the worker, over NFS: the guest
// routes their paths there (Route), as each session says what they are.
// It also serves the owner's SSH agent and registry credentials.
type Guest struct {
	user  *user.User
	log   *slog.Logger
	route func([]Route) error

	mu      sync.Mutex
	current *guestSession
	changed chan struct{} // closed and replaced when current changes
	keys    []ssh.PublicKey
	routes  []Route // as last applied

	synced     chan struct{} // closed once a session's routes are applied
	syncedOnce sync.Once
}

// Route is a path in the guest served from the shared files: Target, a
// file set's ID and the path in it. A directory's is everything under it.
// One that Excludes, inside a routed directory, is the guest's own again:
// served from its disk, with no target.
type Route struct {
	Path    string
	Target  string
	Dir     bool
	Exclude bool
}

// SetPaths are a file set's ID and the paths it shares, as a session sends
// them: relative to the home directory for the owner's profile; absolute,
// or in the home directory (Home), for a pack.
type SetPaths struct {
	ID    string   `json:"id"`
	Paths []string `json:"paths"`
}

// RoutesFor are the routes for the sets an environment is given, in the
// order given: a path two of them name is the first's. The owner's
// profile's paths, which are relative, and a pack's that start with Home
// are routed under home.
func RoutesFor(home string, sets []SetPaths) []Route {
	var out []Route
	seen := map[string]bool{}
	add := func(r Route) {
		if !seen[r.Path] {
			seen[r.Path] = true
			out = append(out, r)
		}
	}
	for _, s := range sets {
		for _, p := range s.Paths {
			x, exclude := strings.CutPrefix(p, Exclude)
			clean := strings.TrimSuffix(x, "/")
			if !ValidKey(clean) {
				continue
			}
			guestPath, target := clean, s.ID+clean
			if rel, ok := strings.CutPrefix(clean, Home); ok {
				guestPath, target = path.Join(home, rel), s.ID+"/"+clean
			} else if !strings.HasPrefix(clean, "/") {
				guestPath, target = path.Join(home, clean), s.ID+"/"+clean
			}
			if exclude {
				add(Route{Path: guestPath, Exclude: true})
				continue
			}
			add(Route{Path: guestPath, Target: target, Dir: strings.HasSuffix(x, "/")})
		}
	}
	return out
}

// Synced is closed once a session's routes have first been applied.
func (g *Guest) Synced() <-chan struct{} { return g.synced }

// NewGuest serves u's shared files through route, which applies a session's
// routes, and serves u's SSH agent.
func NewGuest(u *user.User, route func([]Route) error, log *slog.Logger) *Guest {
	return &Guest{user: u, log: log, route: route, changed: make(chan struct{}),
		synced: make(chan struct{})}
}

// Serve holds one session with the server. A new session replaces the one
// before it, which ended with its connection, whether or not this side has
// noticed yet.
func (g *Guest) Serve(conn io.ReadWriteCloser) {
	defer conn.Close()
	u := g.user
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	s := &guestSession{g: g, conn: conn, home: u.HomeDir, pending: map[int64]chan Message{}}
	s.uid, _ = strconv.Atoi(u.Uid)
	s.gid, _ = strconv.Atoi(u.Gid)

	g.setCurrent(s, nil)
	defer g.setCurrent(nil, s)

	if err := s.run(ctx); err != nil && !errors.Is(err, io.EOF) && !errors.Is(err, net.ErrClosed) && !errors.Is(err, os.ErrClosed) {
		g.log.Warn("profile: session ended", "err", err)
	}
}

// setCurrent makes s the session. Given was, it only replaces that one.
func (g *Guest) setCurrent(s, was *guestSession) {
	g.mu.Lock()
	defer g.mu.Unlock()
	if was != nil && g.current != was {
		return
	}
	if s != nil && g.current != nil {
		g.current.conn.Close()
	}
	g.current = s
	close(g.changed)
	g.changed = make(chan struct{})
}

// connected returns the session as soon as there is one, waiting up to
// wait.
func (g *Guest) connected(wait time.Duration) *guestSession {
	return g.await(wait, func(*guestSession) bool { return true })
}

// await returns the session once there is one and ok says it will do,
// waiting up to wait.
func (g *Guest) await(wait time.Duration, ok func(*guestSession) bool) *guestSession {
	deadline := time.After(wait)
	for {
		g.mu.Lock()
		s, changed := g.current, g.changed
		g.mu.Unlock()
		if s != nil && ok(s) {
			return s
		}
		select {
		case <-changed:
		case <-deadline:
			return nil
		case <-time.After(50 * time.Millisecond):
		}
	}
}

// guestSession is one session with the server.
type guestSession struct {
	g        *Guest
	conn     io.ReadWriteCloser
	home     string
	uid, gid int

	wmu sync.Mutex

	rmu      sync.Mutex
	haveKeys bool // the keys have been sent

	// unrouted are routes not yet applied, tried again until they are:
	// the worker may not be serving the shared files yet. Owned by run's
	// goroutine.
	unrouted []Route

	pmu     sync.Mutex
	pending map[int64]chan Message
	nextID  int64
}

func (s *guestSession) keysKnown() bool {
	s.rmu.Lock()
	defer s.rmu.Unlock()
	return s.haveKeys
}

func (s *guestSession) send(m Message) error {
	b, err := json.Marshal(m)
	if err != nil {
		return err
	}
	s.wmu.Lock()
	defer s.wmu.Unlock()
	_, err = s.conn.Write(append(b, '\n'))
	return err
}

func (s *guestSession) run(ctx context.Context) error {
	incoming := make(chan Message, 64)
	readErr := make(chan error, 1)
	go func() {
		r := bufio.NewReaderSize(s.conn, 64<<10)
		for {
			line, err := r.ReadBytes('\n')
			if err != nil {
				readErr <- err
				return
			}
			var m Message
			if err := json.Unmarshal(line, &m); err != nil {
				readErr <- err
				return
			}
			// Answers go straight to what asked.
			if m.Type == TypeSigned || m.Type == TypeCredential {
				s.answer(m)
				continue
			}
			select {
			case incoming <- m:
			case <-ctx.Done():
				return
			}
		}
	}()
	defer func() {
		s.pmu.Lock()
		for id, ch := range s.pending {
			ch <- Message{ID: id, Error: "the profile session ended"}
			delete(s.pending, id)
		}
		s.pmu.Unlock()
	}()

	retry := time.NewTicker(routeRetry)
	defer retry.Stop()
	for {
		select {
		case err := <-readErr:
			return err
		case m := <-incoming:
			s.handle(m)
		case <-retry.C:
			if s.unrouted != nil {
				s.applyRoutes(s.unrouted)
			}
		}
	}
}

// routeRetry is how often routes that could not be applied are tried again.
const routeRetry = 2 * time.Second

func (s *guestSession) handle(m Message) {
	switch m.Type {
	case TypePaths:
		s.applyRoutes(RoutesFor(s.home, m.Sets))
	case TypeRegistry:
		s.g.setRegistry(m.Host)
	case TypeKeys:
		var keys []ssh.PublicKey
		for _, k := range m.Keys {
			if pub, _, _, _, err := ssh.ParseAuthorizedKey([]byte(k)); err == nil {
				keys = append(keys, pub)
			}
		}
		s.g.mu.Lock()
		s.g.keys = keys
		s.g.mu.Unlock()
		s.rmu.Lock()
		s.haveKeys = true
		s.rmu.Unlock()
	}
}

// applyRoutes has the routes applied, when they are not those already,
// and Claude's record of this machine written if it has none: the setup's
// choices are in the profile, so it is not taken through it again.
func (s *guestSession) applyRoutes(routes []Route) {
	g := s.g
	g.mu.Lock()
	same := g.routes != nil && slices.Equal(routes, g.routes)
	g.mu.Unlock()
	if !same {
		if err := g.route(routes); err != nil {
			if s.unrouted == nil {
				g.log.Warn("profile: routing the shared files; trying again", "err", err)
			}
			s.unrouted = routes
			return
		}
		g.mu.Lock()
		g.routes = routes
		g.mu.Unlock()
	}
	s.unrouted = nil
	if err := s.seedClaudeState(); err != nil {
		g.log.Warn("profile: writing Claude's state", "err", err)
	}
	g.syncedOnce.Do(func() { close(g.synced) })
}

// claudeState is Claude's own record of this machine: the projects it has
// been used in, its caches, and that its setup is done. Claude rewrites it
// all the time, so it is not shared; an environment that has none is given
// one saying the setup is done, since the setup's choices -- the theme in
// settings.json and the sign-in -- come with the profile.
const claudeState = ".claude.json"

func (s *guestSession) seedClaudeState() error {
	f, err := os.OpenFile(filepath.Join(s.home, claudeState), os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
	if errors.Is(err, fs.ErrExist) {
		return nil
	}
	if err != nil {
		return err
	}
	_, err = f.Write([]byte(`{"hasCompletedOnboarding": true}` + "\n"))
	if cerr := f.Close(); err == nil {
		err = cerr
	}
	if err == nil {
		err = os.Chown(f.Name(), s.uid, s.gid)
	}
	return err
}

// ask sends a request and waits for its answer.
func (s *guestSession) ask(m Message) (Message, error) {
	ch := make(chan Message, 1)
	s.pmu.Lock()
	s.nextID++
	m.ID = s.nextID
	s.pending[m.ID] = ch
	s.pmu.Unlock()
	forget := func() {
		s.pmu.Lock()
		delete(s.pending, m.ID)
		s.pmu.Unlock()
	}
	if err := s.send(m); err != nil {
		forget()
		return Message{}, err
	}
	select {
	case r := <-ch:
		if r.Error != "" {
			return r, errors.New(r.Error)
		}
		return r, nil
	case <-time.After(30 * time.Second):
		forget()
		return Message{}, errors.New("the server did not answer")
	}
}

// answer hands an answer to the request waiting for it.
func (s *guestSession) answer(m Message) {
	s.pmu.Lock()
	ch := s.pending[m.ID]
	delete(s.pending, m.ID)
	s.pmu.Unlock()
	if ch != nil {
		ch <- m
	}
}

// ServeSSHAgent serves the SSH agent on socket, for the user, until it
// cannot. Each key it lists is one of the profile's, and each signature is
// the server's.
func (g *Guest) ServeSSHAgent(socket string) error {
	uid, _ := strconv.Atoi(g.user.Uid)
	gid, _ := strconv.Atoi(g.user.Gid)
	if err := os.MkdirAll(filepath.Dir(socket), 0o755); err != nil {
		return err
	}
	os.Remove(socket)
	ln, err := net.Listen("unix", socket)
	if err != nil {
		return err
	}
	defer ln.Close()
	if err := os.Chown(socket, uid, gid); err != nil {
		return err
	}
	if err := os.Chmod(socket, 0o600); err != nil {
		return err
	}
	for {
		conn, err := ln.Accept()
		if err != nil {
			return err
		}
		go func() {
			defer conn.Close()
			agent.ServeAgent(sshAgent{g}, conn)
		}()
	}
}

// keysWait is how long the SSH agent waits for a session before listing the
// keys it has, which with no session is none.
const keysWait = 30 * time.Second

// sshAgent is the SSH agent's keyring: read-only, and backed by the server.
type sshAgent struct{ g *Guest }

var errReadOnly = errors.New("keys are managed in Hangar, on the Account page")

// List gives the user's keys, waiting a while for the server to send them:
// a clone made while the environment is still starting asks before the
// profile session has connected. The keys come before the profile's files,
// so a large profile does not hold them up.
func (a sshAgent) List() ([]*agent.Key, error) {
	a.g.await(keysWait, (*guestSession).keysKnown)
	a.g.mu.Lock()
	defer a.g.mu.Unlock()
	out := []*agent.Key{}
	for _, k := range a.g.keys {
		out = append(out, &agent.Key{Format: k.Type(), Blob: k.Marshal(), Comment: "hangar"})
	}
	return out, nil
}

func (a sshAgent) Sign(key ssh.PublicKey, data []byte) (*ssh.Signature, error) {
	return a.SignWithFlags(key, data, 0)
}

func (a sshAgent) SignWithFlags(key ssh.PublicKey, data []byte, flags agent.SignatureFlags) (*ssh.Signature, error) {
	s := a.g.connected(keysWait)
	if s == nil {
		return nil, errors.New("not connected to Hangar")
	}
	r, err := s.ask(Message{Type: TypeSign, Key: key.Marshal(), Data: data, Flags: uint32(flags)})
	if err != nil {
		return nil, err
	}
	var sig ssh.Signature
	if err := ssh.Unmarshal(r.Signature, &sig); err != nil {
		return nil, fmt.Errorf("a malformed signature: %w", err)
	}
	return &sig, nil
}

func (a sshAgent) Add(agent.AddedKey) error       { return errReadOnly }
func (a sshAgent) Remove(ssh.PublicKey) error     { return errReadOnly }
func (a sshAgent) RemoveAll() error               { return errReadOnly }
func (a sshAgent) Lock([]byte) error              { return errReadOnly }
func (a sshAgent) Unlock([]byte) error            { return errReadOnly }
func (a sshAgent) Signers() ([]ssh.Signer, error) { return nil, errReadOnly }
func (a sshAgent) Extension(string, []byte) ([]byte, error) {
	return nil, agent.ErrExtensionUnsupported
}

// User is whom the guest keeps the profile of.
func (g *Guest) User() *user.User { return g.user }
