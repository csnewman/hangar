package profile_test

import (
	"context"
	"io/fs"
	"net"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"syscall"
	"testing"
	"time"

	"github.com/fsnotify/fsnotify"

	"github.com/csnewman/hangar/internal/hangarsync"
	"github.com/csnewman/hangar/internal/profile"
)

// fakeKernel is the kernel's end of /dev/hangar-sync for one guest. It
// reports what changes in the guest's home directory, as hangarfs does for
// the files written through it, until told not to (quiet).
type fakeKernel struct {
	conn    net.Conn
	wmu     sync.Mutex // one message written at a time
	mu      sync.Mutex
	paths   []string
	watches []string
	cleared int // ClearPaths messages read
	replies map[uint64]chan int32
	quiet   atomic.Bool
}

// serveFakeKernel is the kernel for a guest whose home directory is home.
func serveFakeKernel(t *testing.T, g *profile.Guest, home string) *fakeKernel {
	kernel, agent := net.Pipe()
	t.Cleanup(func() { kernel.Close(); agent.Close() })
	k := &fakeKernel{conn: kernel, replies: map[uint64]chan int32{}}
	go g.ServeLocks(agent)
	k.report(t, home)
	go func() {
		rd := hangarsync.NewReader(kernel)
		for {
			m, err := rd.Next()
			if err != nil {
				return
			}
			k.mu.Lock()
			switch m.Op {
			case hangarsync.OpClearPaths:
				k.paths, k.watches = nil, nil
				k.cleared++
			case hangarsync.OpAddPath:
				k.paths = append(k.paths, m.Path)
			case hangarsync.OpAddWatch:
				k.watches = append(k.watches, m.Path)
			case hangarsync.OpReply:
				if c, ok := k.replies[m.ID]; ok {
					c <- m.Result
				}
			}
			k.mu.Unlock()
		}
	}()
	return k
}

// report reports what changes under home, watching it as hangarfs would
// see it written.
func (k *fakeKernel) report(t *testing.T, home string) {
	w, err := fsnotify.NewWatcher()
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { w.Close() })
	watchTree := func(dir string) {
		filepath.WalkDir(dir, func(p string, e fs.DirEntry, err error) error {
			if err == nil && e.IsDir() {
				w.Add(p)
			}
			return nil
		})
	}
	watchTree(home)
	go func() {
		for ev := range w.Events {
			changed := []string{ev.Name}
			if ev.Has(fsnotify.Create) {
				if st, err := os.Lstat(ev.Name); err == nil && st.IsDir() {
					// Watched only now: what was made in it before is
					// reported as hangarfs would have, one by one.
					watchTree(ev.Name)
					filepath.WalkDir(ev.Name, func(p string, _ fs.DirEntry, err error) error {
						if err == nil && p != ev.Name {
							changed = append(changed, p)
						}
						return nil
					})
				}
			}
			if !k.quiet.Load() {
				for _, p := range changed {
					k.send(hangarsync.Msg{Op: hangarsync.OpChanged, Path: p})
				}
			}
		}
	}()
}

func (k *fakeKernel) send(m hangarsync.Msg) error {
	k.wmu.Lock()
	defer k.wmu.Unlock()
	_, err := k.conn.Write(m.Marshal())
	return err
}

func (k *fakeKernel) shares(p string) bool {
	k.mu.Lock()
	defer k.mu.Unlock()
	for _, s := range k.paths {
		if p == s || strings.HasPrefix(p, s) && strings.HasSuffix(s, "/") {
			return true
		}
	}
	return false
}

// lock sends a Lock, and returns where its answer arrives.
func (k *fakeKernel) lock(t *testing.T, id uint64, path string, wait bool) chan int32 {
	t.Helper()
	c := make(chan int32, 1)
	k.mu.Lock()
	k.replies[id] = c
	k.mu.Unlock()
	m := hangarsync.Msg{Op: hangarsync.OpLock, ID: id, Type: syscall.F_WRLCK, Path: path}
	if wait {
		m.Flags = hangarsync.FlagWait
	}
	if err := k.send(m); err != nil {
		t.Fatal(err)
	}
	return c
}

func (k *fakeKernel) watching(p string) bool {
	k.mu.Lock()
	defer k.mu.Unlock()
	for _, s := range k.watches {
		if p == s || strings.HasPrefix(p, s) && strings.HasSuffix(s, "/") {
			return true
		}
	}
	return false
}

func (k *fakeKernel) changed(t *testing.T, path string) {
	t.Helper()
	if err := k.send(hangarsync.Msg{Op: hangarsync.OpChanged, Path: path}); err != nil {
		t.Fatal(err)
	}
}

func (k *fakeKernel) idle(t *testing.T, path string) {
	t.Helper()
	if err := k.send(hangarsync.Msg{Op: hangarsync.OpIdle, Path: path}); err != nil {
		t.Fatal(err)
	}
}

func answer(t *testing.T, c chan int32, what string) int32 {
	t.Helper()
	select {
	case r := <-c:
		return r
	case <-time.After(15 * time.Second):
		t.Fatalf("no answer %s", what)
		return 0
	}
}

// A lock a program takes on a shared file is the environment's, from the
// server: another environment's program waits for it, and finds the file
// as the holder left it.
func TestKernelLocksAreTheServers(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	if err := p.store.AddPath(ctx, p.owner, "proj/"); err != nil {
		t.Fatal(err)
	}
	_, a, ga := p.env(t, "a", false)
	_, b, gb := p.env(t, "b", false)
	ka, kb := p.kernel(ga), p.kernel(gb)
	dbA, dbB := filepath.Join(a, "proj", "db"), filepath.Join(b, "proj", "db")
	eventually(t, "the shared directory to be registered", func() bool { return ka.shares(dbA) && kb.shares(dbB) })
	if ka.shares(filepath.Join(a, ".claude", "x")) {
		t.Errorf("LockFS's directory registered with the kernel")
	}
	os.MkdirAll(filepath.Join(a, "proj"), 0o755)
	os.WriteFile(dbA, []byte("v1"), 0o644)
	eventually(t, "the file to reach b", func() bool { return read(b, "proj/db") == "v1" })

	if r := answer(t, ka.lock(t, 1, dbA, false), "to a's lock"); r != 0 {
		t.Fatalf("a's lock: %d", r)
	}
	if r := answer(t, ka.lock(t, 2, dbA, false), "to a's second lock"); r != 0 {
		t.Fatalf("a's second lock, while a holds it: %d", r)
	}
	if r := answer(t, kb.lock(t, 3, dbB, false), "to b's lock"); r != -int32(syscall.EAGAIN) {
		t.Fatalf("b's lock without waiting, while a holds it: %d", r)
	}
	waiting := kb.lock(t, 4, dbB, true)
	select {
	case r := <-waiting:
		t.Fatalf("b's waiting lock answered (%d) while a holds it", r)
	case <-time.After(300 * time.Millisecond):
	}

	// Written under the lock, and let go at once.
	os.WriteFile(dbA, []byte("v2"), 0o644)
	ka.idle(t, dbA)
	if r := answer(t, waiting, "to b's waiting lock"); r != 0 {
		t.Fatalf("b's waiting lock: %d", r)
	}
	if got := read(b, "proj/db"); got != "v2" {
		t.Fatalf("b took the lock with the file still %q", got)
	}
	// b lets go as it gets to it: a waits for it rather than racing it.
	kb.idle(t, dbB)
	if r := answer(t, ka.lock(t, 5, dbA, true), "to a's lock again"); r != 0 {
		t.Fatalf("a's lock once b let go: %d", r)
	}
}

// The kernel reports what changes, and nothing else notices: a change
// reaches the profile when it is reported.
func TestKernelReportsChanges(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	_, a, ga := p.env(t, "a", false)
	_, b, _ := p.env(t, "b", false)
	backing := t.TempDir()
	ga.SetBacking(map[string]string{".claude": backing})
	ka := p.kernel(ga)
	ka.quiet.Store(true)
	if err := p.store.AddPath(ctx, p.owner, "proj/"); err != nil {
		t.Fatal(err)
	}
	eventually(t, "the shared directory to be registered", func() bool { return ka.shares(filepath.Join(a, "proj", "x")) })

	os.WriteFile(filepath.Join(a, "proj", "x"), []byte("v1"), 0o644)
	time.Sleep(time.Second)
	if got := read(b, "proj/x"); got == "v1" {
		t.Fatalf("a file no one reported reached b: %q", got)
	}
	ka.changed(t, filepath.Join(a, "proj", "x"))
	eventually(t, "the reported file to reach b", func() bool { return read(b, "proj/x") == "v1" })

	// A directory renamed in is one report, for everything in it.
	src := filepath.Join(a, "elsewhere")
	os.MkdirAll(filepath.Join(src, "deep"), 0o755)
	os.WriteFile(filepath.Join(src, "deep", "y"), []byte("y1"), 0o644)
	if err := os.Rename(src, filepath.Join(a, "proj", "moved")); err != nil {
		t.Fatal(err)
	}
	ka.changed(t, filepath.Join(a, "proj", "moved"))
	eventually(t, "a renamed directory's file to reach b", func() bool { return read(b, "proj/moved/deep/y") == "y1" })

	// LockFS's directories are watched at their backing, and a change
	// reported there is the file at its own path.
	eventually(t, "LockFS's backing to be watched", func() bool {
		return ka.watching(filepath.Join(backing, "settings.json"))
	})
	if ka.shares(filepath.Join(a, ".claude", "settings.json")) {
		t.Error("LockFS's directory registered for locks")
	}
	os.MkdirAll(filepath.Join(a, ".claude"), 0o755)
	os.WriteFile(filepath.Join(a, ".claude", "settings.json"), []byte(`{"theme":"dark"}`), 0o644)
	ka.changed(t, filepath.Join(backing, "settings.json"))
	eventually(t, "a change in LockFS's backing to reach b", func() bool {
		return read(b, ".claude/settings.json") == `{"theme":"dark"}`
	})

	// A report with no path is everything.
	os.WriteFile(filepath.Join(a, "proj", "z"), []byte("z1"), 0o644)
	ka.changed(t, "")
	eventually(t, "everything to be looked at", func() bool { return read(b, "proj/z") == "z1" })
}
