package profile

import (
	"bufio"
	"context"
	"crypto/sha256"
	"encoding/hex"
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
	"sync/atomic"
	"time"

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

	// changes are the kernel's reports of changes to shared files
	// (ServeLocks), as absolute paths; one that did not fit is
	// changesLost, and everything is looked at again.
	changes     chan string
	changesLost atomic.Bool

	synced     chan struct{} // closed once a session has had the profile whole
	syncedOnce sync.Once

	// agreed is, for each shared file, the hash of what this home and the
	// profile last agreed it held, "" for removed. It outlasts a session,
	// so a change made here while there was none is told from one the
	// profile made, and is kept in state across the agent's restarts.
	amu         sync.Mutex
	agreed      map[string]string
	state       string
	agreedStale bool
}

// KeepStateIn keeps what the guest knows of the profile in a file, so that
// it outlasts the agent: what is changed here while the agent is not
// running, or has no session, is then still told from what the profile
// changed.
func (g *Guest) KeepStateIn(path string) error {
	g.amu.Lock()
	defer g.amu.Unlock()
	g.state = path
	b, err := os.ReadFile(path)
	if errors.Is(err, fs.ErrNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	agreed := map[string]string{}
	if err := json.Unmarshal(b, &agreed); err != nil {
		return fmt.Errorf("reading %s: %w", path, err)
	}
	for p, h := range agreed {
		g.agreed[p] = h
	}
	return nil
}

// saveState writes what the guest knows of the profile, if it changed.
func (g *Guest) saveState() {
	g.amu.Lock()
	defer g.amu.Unlock()
	if g.state == "" || !g.agreedStale {
		return
	}
	b, err := json.Marshal(g.agreed)
	if err == nil {
		tmp := g.state + ".tmp"
		if err = os.WriteFile(tmp, b, 0o600); err == nil {
			err = os.Rename(tmp, g.state)
		}
	}
	if err != nil {
		g.log.Warn("profile: saving what it knows", "err", err)
		return
	}
	g.agreedStale = false
}

// Synced is closed once the profile has first arrived whole.
func (g *Guest) Synced() <-chan struct{} { return g.synced }

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
	g := &Guest{user: u, log: log, changed: make(chan struct{}), held: map[string]bool{},
		synced: make(chan struct{}), agreed: map[string]string{},
		changes: make(chan string, 4096)}
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
	s := &guestSession{g: g, conn: conn, home: u.HomeDir,
		dirty: map[string]bool{}, waiting: map[string]Message{}, pending: map[int64]chan Message{},
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
	return g.await(wait, (*guestSession).isReady)
}

// connected returns the session as soon as there is one. Signatures and
// credentials are the server's to give and need nothing of the profile, so
// they are not held up while a large profile is still arriving.
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
	if strings.HasPrefix(dir, "/") {
		return dir
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

// guestSession is one session, and what it knows of the home directory it
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

	// Owned by run's goroutine.
	dirty map[string]bool // keys changed here, not yet looked at
	// waiting are a pack's files whose place does not exist yet: the
	// directory the pack's path is in, which a clone of the workspace
	// makes. They are written once it does.
	waiting map[string]Message

	rmu      sync.Mutex
	ready    bool // the profile has been sent whole
	haveKeys bool // the keys have been sent

	pmu     sync.Mutex
	pending map[int64]chan Message
	nextID  int64
}

func (s *guestSession) isReady() bool {
	s.rmu.Lock()
	defer s.rmu.Unlock()
	return s.ready
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

// hash is hex, being kept in the guest's state as JSON.
func hash(b []byte) string {
	sum := sha256.Sum256(b)
	return hex.EncodeToString(sum[:])
}

// A key names a shared file: relative to the home directory for the
// profile's, absolute for a pack's.

// full is where the file a key names is.
func (s *guestSession) full(key string) string {
	if strings.HasPrefix(key, "/") {
		return key
	}
	return filepath.Join(s.home, key)
}

// keysOf are the keys a path here might be shared as: itself, and,
// under the home directory, relative to it.
func (s *guestSession) keysOf(p string) []string {
	out := []string{p}
	if r, err := filepath.Rel(s.home, p); err == nil && r != "." && r != ".." && !strings.HasPrefix(r, "../") {
		out = append(out, r)
	}
	return out
}

// synced reports whether a key is shared and is not one of the agent's own
// staging files. A file the profile shares is the profile's, though a pack
// names it too.
func (s *guestSession) synced(key string) bool {
	s.kmu.Lock()
	defer s.kmu.Unlock()
	if !s.paths.Synced(key) || strings.HasPrefix(filepath.Base(key), stagingPrefix) {
		return false
	}
	if ks := s.keysOf(key); strings.HasPrefix(key, "/") && len(ks) > 1 && s.paths.Synced(ks[1]) {
		return false
	}
	return true
}

// keyOf is the key a path here is shared as, if it is.
func (s *guestSession) keyOf(p string) (string, bool) {
	for _, k := range s.keysOf(p) {
		if s.synced(k) {
			return k, true
		}
	}
	return "", false
}

// placed reports whether a pack's file has somewhere to go: the directory
// its pack's path is in exists. A profile's always has, being the home
// directory.
func (s *guestSession) placed(key string) bool {
	if !strings.HasPrefix(key, "/") {
		return true
	}
	by, ok := s.sharedPaths().Covering(key)
	if !ok {
		return false
	}
	st, err := os.Stat(path.Dir(strings.TrimSuffix(by, "/")))
	return err == nil && st.IsDir()
}

func (s *guestSession) sharedPaths() Paths {
	s.kmu.Lock()
	defer s.kmu.Unlock()
	return s.paths
}

func (s *guestSession) knownHash(rel string) (string, bool) {
	s.g.amu.Lock()
	defer s.g.amu.Unlock()
	h, ok := s.g.agreed[rel]
	return h, ok
}

// knownUnder returns the files the profile holds under dir.
func (s *guestSession) knownUnder(dir string) []string {
	s.g.amu.Lock()
	defer s.g.amu.Unlock()
	var out []string
	for p, h := range s.g.agreed {
		if h != "" && strings.HasPrefix(p, dir+"/") {
			out = append(out, p)
		}
	}
	return out
}

func (s *guestSession) setKnown(rel, h string) {
	s.g.amu.Lock()
	if cur, ok := s.g.agreed[rel]; !ok || cur != h {
		s.g.agreed[rel] = h
		s.g.agreedStale = true
	}
	s.g.amu.Unlock()
}

func (s *guestSession) run(ctx context.Context) error {
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
			if m.Type == TypeSigned || m.Type == TypeLocked || m.Type == TypeCredential {
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
	retry := time.NewTicker(2 * time.Second)
	defer retry.Stop()
	for {
		select {
		case err := <-readErr:
			return err
		case m := <-incoming:
			if err := s.handle(m); err != nil {
				return err
			}
		case p := <-s.g.changes:
			s.changedHere(p)
			debounce.Reset(100 * time.Millisecond)
		case <-retry.C:
			s.placeWaiting()
		case <-debounce.C:
			if s.g.changesLost.Swap(false) {
				s.rescan()
			}
			if err := s.flush(); err != nil {
				return err
			}
		}
	}
}

func (s *guestSession) handle(m Message) error {
	if s.isReady() {
		defer s.g.saveState()
	}
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
		defer s.g.saveState()
		if err := s.seedClaudeState(); err != nil {
			s.g.log.Warn("profile: writing Claude's state", "err", err)
		}
		s.g.syncedOnce.Do(func() { close(s.g.synced) })
		return s.flush()
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
	return nil
}

// setPaths takes what the profile shares. A path the profile stops sharing is
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
		if strings.HasPrefix(d, "/") {
			// A pack's are made as its files are written, once the
			// directory its path is in exists: one made earlier would be
			// in the way of the clone that makes it.
			continue
		}
		if err := s.mkdirAll(filepath.Join(s.home, d)); err != nil {
			s.g.log.Warn("profile: making a directory", "dir", d, "err", err)
		}
	}
	s.rescan()
	if s.isReady() {
		s.flush()
	}
}

// changedHere marks what the kernel reported changed, at the absolute path
// p, to be looked at: the path, what the profile and packs have under it,
// which a directory removed or renamed took with it, and what is under it
// here, which one made or renamed brought.
func (s *guestSession) changedHere(p string) {
	if p == "" {
		s.rescan()
		return
	}
	paths := s.sharedPaths()
	tree := false
	for _, key := range s.keysOf(p) {
		s.dirty[key] = true
		for _, k := range s.knownUnder(key) {
			s.dirty[k] = true
		}
		tree = tree || paths.InTree(key)
	}
	if tree {
		if st, err := os.Lstat(p); err == nil && st.IsDir() {
			s.markTree(p)
		}
	}
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
		s.markTree(s.full(d))
	}
}

// flush sends the changes made here since the last flush.
func (s *guestSession) flush() error {
	if !s.isReady() {
		return nil
	}
	defer s.g.saveState()
	for rel := range s.dirty {
		delete(s.dirty, rel)
		if !s.synced(rel) {
			continue
		}
		full := s.full(rel)
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
//
// A file changed or removed here since this home and the profile last
// agreed on it -- while there was no session -- is left as it is, and the
// change is sent instead, unless the profile changed it too: then the
// profile's is taken.
func (s *guestSession) apply(m Message) error {
	full := s.full(m.Path)
	if !s.placed(m.Path) {
		s.waiting[m.Path] = m
		return nil
	}
	delete(s.waiting, m.Path)
	if base, ok := s.knownHash(m.Path); ok && base != "" {
		unchanged := !m.Deleted && hash(m.Data) == base
		cur, err := os.ReadFile(full)
		switch {
		case errors.Is(err, fs.ErrNotExist) && unchanged,
			err == nil && hash(cur) != base && (m.Deleted || unchanged):
			s.dirty[m.Path] = true
			return nil
		}
	}
	return s.applyAt(full, m)
}

// placeWaiting writes the pack's files that have somewhere to go.
func (s *guestSession) placeWaiting() {
	for key, m := range s.waiting {
		if !s.placed(key) {
			continue
		}
		if err := s.apply(m); err != nil {
			s.g.log.Warn("profile: writing a file", "path", key, "err", err)
		}
	}
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
	mode := fs.FileMode(m.Mode & 0o777)
	if mode == 0 {
		mode = 0o644
	}
	if cur, err := os.ReadFile(full); err == nil && hash(cur) == h {
		// The same contents: its mode is all that may have changed.
		if st, err := os.Stat(full); err == nil && st.Mode().Perm() != mode {
			return os.Chmod(full, mode)
		}
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
	s.g.amu.Lock()
	var gone []string
	for p, h := range s.g.agreed {
		if h != "" && !seen[p] && strings.HasPrefix(p, dir+"/") {
			gone = append(gone, p)
			s.g.agreed[p] = ""
			s.g.agreedStale = true
		}
	}
	s.g.amu.Unlock()
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
	if parent := filepath.Dir(dir); parent != dir {
		if err := s.mkdirAll(parent); err != nil {
			return err
		}
	}
	if err := os.Mkdir(dir, 0o755); err != nil && !errors.Is(err, fs.ErrExist) {
		return err
	}
	return os.Lchown(dir, s.uid, s.gid)
}

// markTree marks what is in a directory and every directory under it to be
// looked at.
func (s *guestSession) markTree(dir string) {
	filepath.WalkDir(dir, func(p string, e fs.DirEntry, err error) error {
		if err != nil || e.IsDir() {
			return nil
		}
		for _, key := range s.keysOf(p) {
			s.dirty[key] = true
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

// keysWait is how long the SSH agent waits for a session before listing the
// keys it has, which with no session is none.
const keysWait = 30 * time.Second

// sshAgent is the SSH agent's keyring: read-only, and backed by the server.
type sshAgent struct{ g *Guest }

var errReadOnly = errors.New("keys are managed in Hangar, on the Profile page")

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
