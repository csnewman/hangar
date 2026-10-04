package profile

import (
	"io"
	"path"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/csnewman/hangar/internal/hangarsync"
)

// ServeLocks serves the kernel's requests for locks on shared files, from
// dev (hangarsync.Device), until it fails. A process here taking a lock on
// one has the lock taken for the environment from the server first; when no
// process here holds one any more, the environment lets it go.
//
// The shared paths are registered with the kernel and kept up to date, but
// for the directories LockFS serves, whose locks it decides itself.
func (g *Guest) ServeLocks(dev io.ReadWriter) error {
	sl := &lockServer{g: g, dev: dev, queues: map[string]chan hangarsync.Msg{},
		cancels: map[uint64]chan struct{}{}}
	done := make(chan struct{})
	defer close(done)
	go sl.registerPaths(done)
	rd := hangarsync.NewReader(dev)
	for {
		m, err := rd.Next()
		if err != nil {
			return err
		}
		switch m.Op {
		case hangarsync.OpCancel:
			sl.mu.Lock()
			if c, ok := sl.cancels[m.ID]; ok {
				close(c)
				delete(sl.cancels, m.ID)
			}
			sl.mu.Unlock()
		case hangarsync.OpLock, hangarsync.OpIdle:
			if m.Op == hangarsync.OpLock {
				sl.mu.Lock()
				sl.cancels[m.ID] = make(chan struct{})
				sl.mu.Unlock()
			}
			sl.queue(m.Path) <- m
		}
	}
}

type lockServer struct {
	g   *Guest
	dev io.ReadWriter

	wmu sync.Mutex // one message written at a time

	mu      sync.Mutex
	queues  map[string]chan hangarsync.Msg // per path, in the kernel's order
	cancels map[uint64]chan struct{}       // per Lock not yet answered
}

func (sl *lockServer) write(m hangarsync.Msg) error {
	sl.wmu.Lock()
	defer sl.wmu.Unlock()
	_, err := sl.dev.Write(m.Marshal())
	return err
}

// queue is the path's queue: its requests are answered in order, so that a
// lock let go and taken again is let go first.
func (sl *lockServer) queue(p string) chan hangarsync.Msg {
	sl.mu.Lock()
	defer sl.mu.Unlock()
	q, ok := sl.queues[p]
	if !ok {
		q = make(chan hangarsync.Msg, 64)
		sl.queues[p] = q
		go sl.serve(q)
	}
	return q
}

func (sl *lockServer) serve(q chan hangarsync.Msg) {
	for m := range q {
		rel, ok := sl.rel(m.Path)
		if !ok {
			if m.Op == hangarsync.OpLock {
				sl.answer(m.ID, 0)
			}
			continue
		}
		sl.g.log.Debug("profile: the kernel about a lock", "op", m.Op, "id", m.ID, "path", rel, "flags", m.Flags)
		switch m.Op {
		case hangarsync.OpIdle:
			if sl.g.Holds(rel) {
				if err := sl.g.Unlock(rel); err != nil {
					sl.g.log.Warn("profile: letting go of a lock", "path", rel, "err", err)
				}
			}
		case hangarsync.OpLock:
			sl.lock(m, rel)
		}
	}
}

// lock answers a Lock: at once while the environment holds the file's
// lock, or when the server gives it; a caller that will not wait is told
// another environment has it.
func (sl *lockServer) lock(m hangarsync.Msg, rel string) {
	sl.mu.Lock()
	cancel := sl.cancels[m.ID]
	sl.mu.Unlock()
	wait := 50 * time.Millisecond
	for {
		if sl.g.Holds(rel) {
			sl.answer(m.ID, 0)
			return
		}
		ok, err := sl.g.Lock(rel)
		if ok {
			sl.answer(m.ID, 0)
			return
		}
		if m.Flags&hangarsync.FlagWait == 0 {
			if err != nil {
				// No server to ask: nobody can say it is free.
				sl.answer(m.ID, -int32(syscall.ENOLCK))
			} else {
				sl.answer(m.ID, -int32(syscall.EAGAIN))
			}
			return
		}
		select {
		case <-cancel:
			return
		case <-time.After(wait):
		}
		wait = min(2*wait, time.Second)
	}
}

func (sl *lockServer) answer(id uint64, result int32) {
	sl.mu.Lock()
	delete(sl.cancels, id)
	sl.mu.Unlock()
	if err := sl.write(hangarsync.Msg{Op: hangarsync.OpReply, ID: id, Result: result}); err != nil {
		sl.g.log.Warn("profile: answering the kernel about a lock", "err", err)
	}
}

// rel is a path under the home directory, relative to it.
func (sl *lockServer) rel(p string) (string, bool) {
	r, err := filepath.Rel(sl.g.user.HomeDir, p)
	if err != nil || r == "." || strings.HasPrefix(r, "../") || r == ".." {
		return "", false
	}
	return r, true
}

// registerPaths keeps the kernel's shared paths those of the current
// session, until done.
func (sl *lockServer) registerPaths(done <-chan struct{}) {
	var have []string
	for {
		want := sl.kernelPaths()
		if !slices.Equal(want, have) {
			msgs := []hangarsync.Msg{{Op: hangarsync.OpClearPaths}}
			for _, p := range want {
				msgs = append(msgs, hangarsync.Msg{Op: hangarsync.OpAddPath, Path: p})
			}
			ok := true
			for _, m := range msgs {
				if err := sl.write(m); err != nil {
					sl.g.log.Warn("profile: registering shared paths with the kernel", "err", err)
					ok = false
					break
				}
			}
			if ok {
				have = want
			}
		}
		select {
		case <-done:
			return
		case <-time.After(2 * time.Second):
		}
	}
}

// kernelPaths are the shared paths as the kernel matches them: absolute,
// a directory's with its trailing slash, without LockFS's directories.
func (sl *lockServer) kernelPaths() []string {
	sl.g.mu.Lock()
	s := sl.g.current
	sl.g.mu.Unlock()
	if s == nil {
		return nil
	}
	lockDirs := LockDirs()
	var out []string
	for _, p := range s.sharedPaths() {
		clean := strings.TrimSuffix(p, "/")
		if slices.ContainsFunc(lockDirs, func(d string) bool {
			return clean == d || strings.HasPrefix(clean, d+"/") || strings.HasPrefix(d, clean+"/")
		}) {
			continue
		}
		abs := path.Join(sl.g.user.HomeDir, clean)
		if strings.HasSuffix(p, "/") {
			abs += "/"
		}
		out = append(out, abs)
	}
	slices.Sort(out)
	return out
}

// Holds reports whether the environment holds a lock on a path, relative to
// the home directory.
func (g *Guest) Holds(p string) bool {
	g.mu.Lock()
	defer g.mu.Unlock()
	return g.held[p]
}
