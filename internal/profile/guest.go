package profile

import (
	"bufio"
	"context"
	"crypto/sha256"
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

	"github.com/fsnotify/fsnotify"
	"golang.org/x/crypto/ssh"
	"golang.org/x/crypto/ssh/agent"
)

// stagingPrefix names the files the agent writes beside a file it is about
// to replace. They are never shared.
const stagingPrefix = ".hangar-sync-"

// Guest is the agent's half: it keeps a user's home directory in step with
// their profile, answers LockFS, and serves their SSH agent.
type Guest struct {
	user *user.User
	log  *slog.Logger

	mu      sync.Mutex
	current *guestSession
	changed chan struct{} // closed and replaced when current changes
	keys    []ssh.PublicKey
	held    map[string]bool   // locks held here, renewed while they are
	backing map[string]string // LockFS's backing for each directory it serves
}

// SetBacking tells the guest where LockFS keeps each directory it serves.
// While a program takes or lets go of a lock, the directory is held by the
// kernel, so the files in it are read and written there instead.
func (g *Guest) SetBacking(backing map[string]string) {
	g.mu.Lock()
	g.backing = backing
	g.mu.Unlock()
}

// NewGuest keeps u's home directory, and serves u's SSH agent.
func NewGuest(u *user.User, log *slog.Logger) *Guest {
	g := &Guest{user: u, log: log, changed: make(chan struct{}), held: map[string]bool{}}
	go g.renew()
	return g
}

// Serve holds one session with the server. A new session replaces the one
// before it, which ended with its connection, whether or not this side has
// noticed yet.
func (g *Guest) Serve(conn io.ReadWriteCloser) {
	defer conn.Close()
	u := g.user
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	s := &guestSession{g: g, conn: conn, home: u.HomeDir, known: map[string]string{},
		dirty: map[string]bool{}, watched: map[string]bool{}, pending: map[int64]chan Message{},
	}
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

// session returns the session once it has been sent the profile whole,
// waiting up to wait for one.
func (g *Guest) session(wait time.Duration) *guestSession {
	deadline := time.After(wait)
	for {
		g.mu.Lock()
		s, changed := g.current, g.changed
		g.mu.Unlock()
		if s != nil && s.isReady() {
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

// Lock asks the server for one of the Locks. It is refused while there is
// no server to ask: the server is what knows whether another environment
// holds it. Granted, it comes with the files in the lock's directory as the
// profile has them, written here before the program that asked goes on.
func (g *Guest) Lock(p string) (bool, error) {
	s := g.session(10 * time.Second)
	if s == nil {
		return false, errors.New("not connected to Hangar")
	}
	r, err := s.ask(Message{Type: TypeLock, Path: p, Held: true})
	if err != nil || !r.Held {
		return false, err
	}
	g.mu.Lock()
	g.held[p] = true
	g.mu.Unlock()
	dir := path.Dir(p)
	for _, f := range r.Files {
		if err := s.applyUnder(g.backingFor(dir), dir, f); err != nil {
			g.log.Warn("profile: writing a file under a lock", "path", f.Path, "err", err)
		}
	}
	return true, nil
}

// Unlock lets go of a lock, having sent what changed in its directory
// first, so the next holder finds it.
func (g *Guest) Unlock(p string) error {
	g.mu.Lock()
	delete(g.held, p)
	g.mu.Unlock()
	s := g.session(0)
	if s == nil {
		// The server lets go of a lock whose session ended.
		return nil
	}
	dir := path.Dir(p)
	if err := s.sendUnder(g.backingFor(dir), dir); err != nil {
		return err
	}
	_, err := s.ask(Message{Type: TypeLock, Path: p, Held: false})
	return err
}

// backingFor is where a directory's files are read and written while a lock
// in it is taken or let go: LockFS's backing if it serves it.
func (g *Guest) backingFor(dir string) string {
	g.mu.Lock()
	defer g.mu.Unlock()
	if b, ok := g.backing[dir]; ok {
		return b
	}
	return filepath.Join(g.user.HomeDir, dir)
}

// renew keeps the server's lease on the locks held here while they are.
func (g *Guest) renew() {
	for range time.Tick(5 * time.Second) {
		g.mu.Lock()
		held := make([]string, 0, len(g.held))
		for p := range g.held {
			held = append(held, p)
		}
		s := g.current
		g.mu.Unlock()
		if s == nil {
			continue
		}
		for _, p := range held {
			s.send(Message{Type: TypeLock, Path: p, Held: true})
		}
	}
}

// guestSession is one session, and the watch on the home directory it
// keeps while it lasts.
type guestSession struct {
	g        *Guest
	conn     io.ReadWriteCloser
	home     string
	uid, gid int

	wmu sync.Mutex

	// Shared with the lock handlers.
	kmu   sync.Mutex
	paths Paths
	known map[string]string // path to the hash of what the profile holds; "" for removed

	// Owned by run's goroutine.
	dirty   map[string]bool // paths changed here, not yet looked at
	watched map[string]bool // directories watched
	watcher *fsnotify.Watcher

	rmu   sync.Mutex
	ready bool // the profile has been sent whole

	pmu     sync.Mutex
	pending map[int64]chan Message
	nextID  int64
}

func (s *guestSession) isReady() bool {
	s.rmu.Lock()
	defer s.rmu.Unlock()
	return s.ready
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

func hash(b []byte) string {
	sum := sha256.Sum256(b)
	return string(sum[:])
}

// synced reports whether a path is shared and is not one of the agent's
// own staging files.
func (s *guestSession) synced(rel string) bool {
	s.kmu.Lock()
	defer s.kmu.Unlock()
	return s.paths.Synced(rel) && !strings.HasPrefix(filepath.Base(rel), stagingPrefix)
}

func (s *guestSession) sharedPaths() Paths {
	s.kmu.Lock()
	defer s.kmu.Unlock()
	return s.paths
}

func (s *guestSession) knownHash(rel string) (string, bool) {
	s.kmu.Lock()
	defer s.kmu.Unlock()
	h, ok := s.known[rel]
	return h, ok
}

func (s *guestSession) setKnown(rel, h string) {
	s.kmu.Lock()
	s.known[rel] = h
	s.kmu.Unlock()
}

func (s *guestSession) run(ctx context.Context) error {
	w, err := fsnotify.NewWatcher()
	if err != nil {
		return err
	}
	s.watcher = w
	defer w.Close()

	// Buffered well past anything a session sends at once: the reader
	// must never wait on run, which can itself be waiting on a directory
	// a lock handler holds, while the lock handler waits on the reader.
	incoming := make(chan Message, 4096)
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
			// Answers go straight to what asked, which may be a lock
			// handler run cannot help.
			if m.Type == TypeSigned || m.Type == TypeLocked {
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

	debounce := time.NewTimer(time.Hour)
	debounce.Stop()
	for {
		select {
		case err := <-readErr:
			return err
		case m := <-incoming:
			if err := s.handle(m); err != nil {
				return err
			}
		case ev, ok := <-w.Events:
			if !ok {
				return errors.New("the file watch ended")
			}
			rel, err := filepath.Rel(s.home, ev.Name)
			if err != nil {
				continue
			}
			if ev.Has(fsnotify.Create) && s.sharedPaths().InTree(rel) {
				if st, err := os.Lstat(ev.Name); err == nil && st.IsDir() {
					s.watchTree(ev.Name)
				}
			}
			s.dirty[rel] = true
			debounce.Reset(100 * time.Millisecond)
		case err := <-w.Errors:
			s.g.log.Warn("profile: watching files", "err", err)
		case <-debounce.C:
			if err := s.flush(); err != nil {
				return err
			}
		}
	}
}

func (s *guestSession) handle(m Message) error {
	switch m.Type {
	case TypePaths:
		s.setPaths(Paths(m.Paths))
	case TypeFile:
		if !s.synced(m.Path) {
			return nil
		}
		if err := s.apply(m); err != nil {
			s.g.log.Warn("profile: writing a file", "path", m.Path, "err", err)
		}
	case TypeSynced:
		// What is here and the profile has never had is this
		// environment's to add, as when the first environment brings
		// the settings someone already had.
		s.rescan()
		s.rmu.Lock()
		s.ready = true
		s.rmu.Unlock()
		return s.flush()
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
	}
	return nil
}

// setPaths watches what the profile shares. A path the profile stops sharing is
// left as it is, a file of this environment's own.
func (s *guestSession) setPaths(paths Paths) {
	if slices.Equal(paths, s.sharedPaths()) {
		return
	}
	s.kmu.Lock()
	s.paths = paths
	s.kmu.Unlock()
	parents, trees := paths.Dirs()
	for _, d := range append(parents, trees...) {
		if err := s.mkdirAll(filepath.Join(s.home, d)); err != nil {
			s.g.log.Warn("profile: making a directory", "dir", d, "err", err)
		}
	}
	for _, d := range parents {
		s.watch(filepath.Join(s.home, d))
	}
	for _, d := range trees {
		s.watchTree(filepath.Join(s.home, d))
	}
	s.rescan()
	if s.isReady() {
		s.flush()
	}
}

// rescan marks every shared file here to be looked at.
func (s *guestSession) rescan() {
	paths := s.sharedPaths()
	_, trees := paths.Dirs()
	for _, p := range paths {
		if !strings.HasSuffix(p, "/") {
			s.dirty[p] = true
		}
	}
	for _, d := range trees {
		filepath.WalkDir(filepath.Join(s.home, d), func(p string, e fs.DirEntry, err error) error {
			if err == nil && !e.IsDir() {
				if rel, err := filepath.Rel(s.home, p); err == nil {
					s.dirty[rel] = true
				}
			}
			return nil
		})
	}
}

// flush sends the changes made here since the last flush.
func (s *guestSession) flush() error {
	if !s.isReady() {
		return nil
	}
	for rel := range s.dirty {
		delete(s.dirty, rel)
		if !s.synced(rel) {
			continue
		}
		full := filepath.Join(s.home, rel)
		st, err := os.Lstat(full)
		if errors.Is(err, fs.ErrNotExist) {
			if h, ok := s.knownHash(rel); ok && h != "" {
				s.setKnown(rel, "")
				if err := s.send(Message{Type: TypeDelete, Path: rel}); err != nil {
					return err
				}
			}
			continue
		}
		if err != nil || !st.Mode().IsRegular() || st.Size() > MaxFileSize {
			continue
		}
		data, err := os.ReadFile(full)
		if err != nil {
			continue
		}
		h := hash(data)
		if k, _ := s.knownHash(rel); k == h {
			continue
		}
		s.setKnown(rel, h)
		if err := s.send(Message{Type: TypePut, Path: rel, Data: data, Mode: uint32(st.Mode().Perm())}); err != nil {
			return err
		}
	}
	return nil
}

// apply writes a file the profile sent. It is written beside the file and
// renamed into place, so a program reading it never sees it half written,
// and one watching it sees it replaced.
func (s *guestSession) apply(m Message) error {
	return s.applyAt(filepath.Join(s.home, m.Path), m)
}

// applyUnder writes a file sent with a lock into the directory's backing.
func (s *guestSession) applyUnder(backing, dir string, m Message) error {
	rel, err := filepath.Rel(dir, m.Path)
	if err != nil || strings.HasPrefix(rel, "..") || !s.synced(m.Path) {
		return nil
	}
	return s.applyAt(filepath.Join(backing, rel), m)
}

func (s *guestSession) applyAt(full string, m Message) error {
	if m.Deleted {
		s.setKnown(m.Path, "")
		err := os.Remove(full)
		if errors.Is(err, fs.ErrNotExist) {
			return nil
		}
		return err
	}
	h := hash(m.Data)
	s.setKnown(m.Path, h)
	if cur, err := os.ReadFile(full); err == nil && hash(cur) == h {
		return nil
	}
	dir := filepath.Dir(full)
	if err := s.mkdirAll(dir); err != nil {
		return err
	}
	f, err := os.CreateTemp(dir, stagingPrefix)
	if err != nil {
		return err
	}
	tmp := f.Name()
	_, err = f.Write(m.Data)
	if cerr := f.Close(); err == nil {
		err = cerr
	}
	mode := fs.FileMode(m.Mode & 0o777)
	if mode == 0 {
		mode = 0o644
	}
	if err == nil {
		err = os.Chmod(tmp, mode)
	}
	if err == nil {
		err = os.Chown(tmp, s.uid, s.gid)
	}
	if err == nil {
		err = os.Rename(tmp, full)
	}
	if err != nil {
		os.Remove(tmp)
	}
	return err
}

// sendUnder sends the shared files in a directory, read from its backing,
// that differ from what the profile holds.
func (s *guestSession) sendUnder(backing, dir string) error {
	paths := s.sharedPaths()
	seen := map[string]bool{}
	err := filepath.WalkDir(backing, func(p string, e fs.DirEntry, err error) error {
		if err != nil || e.IsDir() {
			return nil
		}
		rel, err := filepath.Rel(backing, p)
		if err != nil {
			return nil
		}
		home := filepath.ToSlash(filepath.Join(dir, rel))
		if !paths.Synced(home) || strings.HasPrefix(e.Name(), stagingPrefix) {
			return nil
		}
		seen[home] = true
		info, err := e.Info()
		if err != nil || !info.Mode().IsRegular() || info.Size() > MaxFileSize {
			return nil
		}
		data, err := os.ReadFile(p)
		if err != nil {
			return nil
		}
		h := hash(data)
		if k, _ := s.knownHash(home); k == h {
			return nil
		}
		s.setKnown(home, h)
		return s.send(Message{Type: TypePut, Path: home, Data: data, Mode: uint32(info.Mode().Perm())})
	})
	if err != nil {
		return err
	}
	// Removed under the lock.
	s.kmu.Lock()
	var gone []string
	for p, h := range s.known {
		if h != "" && !seen[p] && strings.HasPrefix(p, dir+"/") {
			gone = append(gone, p)
			s.known[p] = ""
		}
	}
	s.kmu.Unlock()
	for _, p := range gone {
		if err := s.send(Message{Type: TypeDelete, Path: p}); err != nil {
			return err
		}
	}
	return nil
}

// mkdirAll makes a directory and any parents missing, owned by the user.
func (s *guestSession) mkdirAll(dir string) error {
	if _, err := os.Stat(dir); err == nil {
		return nil
	}
	if parent := filepath.Dir(dir); parent != dir && strings.HasPrefix(dir, s.home) {
		if err := s.mkdirAll(parent); err != nil {
			return err
		}
	}
	if err := os.Mkdir(dir, 0o755); err != nil && !errors.Is(err, fs.ErrExist) {
		return err
	}
	return os.Lchown(dir, s.uid, s.gid)
}

func (s *guestSession) watch(dir string) {
	if s.watched[dir] {
		return
	}
	if err := s.watcher.Add(dir); err != nil {
		s.g.log.Warn("profile: watching a directory", "dir", dir, "err", err)
		return
	}
	s.watched[dir] = true
}

// watchTree watches a directory and every directory under it, marking what
// is in them to be looked at.
func (s *guestSession) watchTree(dir string) {
	filepath.WalkDir(dir, func(p string, e fs.DirEntry, err error) error {
		if err != nil {
			return nil
		}
		if e.IsDir() {
			s.watch(p)
			return nil
		}
		if rel, err := filepath.Rel(s.home, p); err == nil {
			s.dirty[rel] = true
		}
		return nil
	})
}

// ask sends a request and waits for its answer. It does not wait on run:
// a lock handler asks while the kernel holds a directory run may be
// waiting on.
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

// sshAgent is the SSH agent's keyring: read-only, and backed by the server.
type sshAgent struct{ g *Guest }

var errReadOnly = errors.New("keys are managed in Hangar, on the Profile page")

func (a sshAgent) List() ([]*agent.Key, error) {
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
	s := a.g.session(5 * time.Second)
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
