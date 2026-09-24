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
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/fsnotify/fsnotify"
	"golang.org/x/crypto/ssh"
	"golang.org/x/crypto/ssh/agent"
)

// Guest is the agent's half: it keeps a user's home directory in step with
// their profile, and serves their SSH agent.
type Guest struct {
	user *user.User
	log  *slog.Logger

	mu      sync.Mutex
	current *guestSession
	keys    []ssh.PublicKey
}

// NewGuest keeps u's home directory, and serves u's SSH agent.
func NewGuest(u *user.User, log *slog.Logger) *Guest {
	return &Guest{user: u, log: log}
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
		mirrors: map[string]bool{}, taken: map[string]bool{}, dirty: map[string]bool{},
		pending: map[int64]chan Message{}}
	s.uid, _ = strconv.Atoi(u.Uid)
	s.gid, _ = strconv.Atoi(u.Gid)

	g.mu.Lock()
	if g.current != nil {
		g.current.conn.Close()
	}
	g.current = s
	g.mu.Unlock()
	defer func() {
		g.mu.Lock()
		if g.current == s {
			g.current = nil
		}
		g.mu.Unlock()
	}()

	if err := s.run(ctx); err != nil && !errors.Is(err, io.EOF) && !errors.Is(err, net.ErrClosed) && !errors.Is(err, os.ErrClosed) {
		g.log.Warn("profile: session ended", "err", err)
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

	// Owned by run's goroutine.
	known   map[string]string // path to the hash of what the profile holds; "" for removed
	mirrors map[string]bool   // locks held here for another environment
	taken   map[string]bool   // locks a program here holds
	dirty   map[string]bool   // paths changed here, not yet looked at
	ready   bool              // the profile has been sent whole
	watcher *fsnotify.Watcher

	pmu     sync.Mutex
	pending map[int64]chan Message
	nextID  int64
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

func (s *guestSession) run(ctx context.Context) error {
	w, err := fsnotify.NewWatcher()
	if err != nil {
		return err
	}
	s.watcher = w
	defer w.Close()
	parents, trees := Dirs()
	for _, d := range append(parents, trees...) {
		if err := s.mkdirAll(filepath.Join(s.home, d)); err != nil {
			return err
		}
	}
	for _, d := range parents {
		full := filepath.Join(s.home, d)
		if err := w.Add(full); err != nil {
			return err
		}
		for _, p := range synced {
			if path.Dir(p) == d && !strings.HasSuffix(p, "/") {
				s.dirty[p] = true
			}
		}
	}
	for _, d := range trees {
		s.watchTree(filepath.Join(s.home, d))
	}
	// A lock held here for another environment must not outlive the
	// session: nothing would let it go.
	defer func() {
		for p := range s.mirrors {
			os.RemoveAll(filepath.Join(s.home, p))
		}
	}()

	incoming := make(chan Message)
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
			if m.Type == TypeSigned {
				s.pmu.Lock()
				ch := s.pending[m.ID]
				delete(s.pending, m.ID)
				s.pmu.Unlock()
				if ch != nil {
					ch <- m
				}
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
			ch <- Message{Type: TypeSigned, ID: id, Error: "the profile session ended"}
			delete(s.pending, id)
		}
		s.pmu.Unlock()
	}()

	debounce := time.NewTimer(time.Hour)
	debounce.Stop()
	renew := time.NewTicker(5 * time.Second)
	defer renew.Stop()
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
			if _, isLock := LockAt(rel); isLock {
				if err := s.lockChanged(rel); err != nil {
					return err
				}
				continue
			}
			if ev.Has(fsnotify.Create) && InTree(rel) {
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
		case <-renew.C:
			now := time.Now()
			for p := range s.mirrors {
				os.Chtimes(filepath.Join(s.home, p), now, now)
			}
			for p := range s.taken {
				if err := s.send(Message{Type: TypeLocked, Path: p, Held: true}); err != nil {
					return err
				}
			}
		}
	}
}

func (s *guestSession) handle(m Message) error {
	switch m.Type {
	case TypeFile:
		if !Synced(m.Path) {
			return nil
		}
		if err := s.apply(m); err != nil {
			s.g.log.Warn("profile: writing a file", "path", m.Path, "err", err)
		}
	case TypeSynced:
		s.ready = true
		// What is here and the profile has never had is this
		// environment's to add, as when the first environment brings
		// the settings someone already had.
		for _, d := range synced {
			full := filepath.Join(s.home, d)
			filepath.WalkDir(full, func(p string, e fs.DirEntry, err error) error {
				if err != nil || e.IsDir() {
					return nil
				}
				if rel, err := filepath.Rel(s.home, p); err == nil {
					if _, ok := s.known[rel]; !ok {
						s.dirty[rel] = true
					}
				}
				return nil
			})
		}
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
	case TypeLock:
		lock, ok := LockAt(m.Path)
		if !ok {
			return nil
		}
		full := filepath.Join(s.home, lock.Path)
		if m.Held {
			if s.mirrors[lock.Path] || s.taken[lock.Path] {
				return nil
			}
			s.mirrors[lock.Path] = true
			var err error
			if lock.Dir {
				err = os.Mkdir(full, 0o755)
			} else {
				var f *os.File
				if f, err = os.OpenFile(full, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0o644); err == nil {
					f.Close()
				}
			}
			if err != nil {
				// A program here holds it too: both go on.
				delete(s.mirrors, lock.Path)
				return nil
			}
			os.Lchown(full, s.uid, s.gid)
		} else if s.mirrors[lock.Path] {
			delete(s.mirrors, lock.Path)
			os.RemoveAll(full)
		}
	}
	return nil
}

// lockChanged looks at a lock that was taken or let go here.
func (s *guestSession) lockChanged(rel string) error {
	if s.mirrors[rel] {
		return nil
	}
	_, err := os.Lstat(filepath.Join(s.home, rel))
	exists := err == nil
	switch {
	case exists && !s.taken[rel]:
		s.taken[rel] = true
		return s.send(Message{Type: TypeLocked, Path: rel, Held: true})
	case !exists && s.taken[rel]:
		delete(s.taken, rel)
		// What the program changed under the lock goes first, so the
		// other environments have it before they let the lock go.
		if err := s.flush(); err != nil {
			return err
		}
		return s.send(Message{Type: TypeLocked, Path: rel, Held: false})
	}
	return nil
}

// flush sends the changes made here since the last flush.
func (s *guestSession) flush() error {
	if !s.ready {
		return nil
	}
	for rel := range s.dirty {
		delete(s.dirty, rel)
		if !Synced(rel) {
			continue
		}
		full := filepath.Join(s.home, rel)
		st, err := os.Lstat(full)
		if errors.Is(err, fs.ErrNotExist) {
			if h, ok := s.known[rel]; ok && h != "" {
				s.known[rel] = ""
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
		if s.known[rel] == h {
			continue
		}
		s.known[rel] = h
		if err := s.send(Message{Type: TypePut, Path: rel, Data: data, Mode: uint32(st.Mode().Perm())}); err != nil {
			return err
		}
	}
	return nil
}

// apply writes a file the profile sent. It is written beside the home
// directory and renamed into place, so a program reading it never sees it
// half written, and one watching it sees it replaced.
func (s *guestSession) apply(m Message) error {
	full := filepath.Join(s.home, m.Path)
	if m.Deleted {
		s.known[m.Path] = ""
		err := os.Remove(full)
		if errors.Is(err, fs.ErrNotExist) {
			return nil
		}
		return err
	}
	h := hash(m.Data)
	s.known[m.Path] = h
	if cur, err := os.ReadFile(full); err == nil && hash(cur) == h {
		return nil
	}
	if err := s.mkdirAll(filepath.Dir(full)); err != nil {
		return err
	}
	staging := filepath.Join(s.home, ".cache", "hangar-profile")
	if err := s.mkdirAll(staging); err != nil {
		return err
	}
	f, err := os.CreateTemp(staging, "file-")
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

// watchTree watches a directory and every directory under it, marking what
// is in them to be looked at.
func (s *guestSession) watchTree(dir string) {
	filepath.WalkDir(dir, func(p string, e fs.DirEntry, err error) error {
		if err != nil {
			return nil
		}
		if e.IsDir() {
			if err := s.watcher.Add(p); err != nil {
				s.g.log.Warn("profile: watching a directory", "dir", p, "err", err)
			}
			return nil
		}
		if rel, err := filepath.Rel(s.home, p); err == nil {
			s.dirty[rel] = true
		}
		return nil
	})
}

// request sends a signing request and waits for its answer.
func (s *guestSession) request(m Message) (Message, error) {
	ch := make(chan Message, 1)
	s.pmu.Lock()
	s.nextID++
	m.ID = s.nextID
	s.pending[m.ID] = ch
	s.pmu.Unlock()
	if err := s.send(m); err != nil {
		s.pmu.Lock()
		delete(s.pending, m.ID)
		s.pmu.Unlock()
		return Message{}, err
	}
	select {
	case r := <-ch:
		return r, nil
	case <-time.After(30 * time.Second):
		s.pmu.Lock()
		delete(s.pending, m.ID)
		s.pmu.Unlock()
		return Message{}, errors.New("the server did not answer")
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
	a.g.mu.Lock()
	s := a.g.current
	a.g.mu.Unlock()
	if s == nil {
		return nil, errors.New("not connected to Hangar")
	}
	r, err := s.request(Message{Type: TypeSign, Key: key.Marshal(), Data: data, Flags: uint32(flags)})
	if err != nil {
		return nil, err
	}
	if r.Error != "" {
		return nil, errors.New(r.Error)
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
