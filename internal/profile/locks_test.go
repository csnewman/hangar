package profile_test

import (
	"context"
	"net"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"testing"
	"time"

	"github.com/csnewman/hangar/internal/hangarsync"
	"github.com/csnewman/hangar/internal/profile"
)

// fakeKernel is the kernel's end of /dev/hangar-sync for one guest.
type fakeKernel struct {
	conn    net.Conn
	mu      sync.Mutex
	paths   []string
	watches []string
	cleared int // ClearPaths messages read
	replies map[uint64]chan int32
}

// serveFakeKernel serves a guest's locks; with changes, the kernel reports
// changes to shared files too, as under hangarfs.
func serveFakeKernel(t *testing.T, g *profile.Guest, changes bool) *fakeKernel {
	kernel, agent := net.Pipe()
	t.Cleanup(func() { kernel.Close(); agent.Close() })
	k := &fakeKernel{conn: kernel, replies: map[uint64]chan int32{}}
	go g.ServeLocks(agent, changes)
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
	if _, err := k.conn.Write(m.Marshal()); err != nil {
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
	if _, err := k.conn.Write(hangarsync.Msg{Op: hangarsync.OpChanged, Path: path}.Marshal()); err != nil {
		t.Fatal(err)
	}
}

func (k *fakeKernel) idle(t *testing.T, path string) {
	t.Helper()
	if _, err := k.conn.Write(hangarsync.Msg{Op: hangarsync.OpIdle, Path: path}.Marshal()); err != nil {
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
	ka, kb := serveFakeKernel(t, ga, false), serveFakeKernel(t, gb, false)
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

// Under hangarfs the kernel reports what changes, and the files are not
// watched: a change reaches the profile when it is reported.
func TestKernelReportsChanges(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	_, a, ga := p.env(t, "a", false)
	_, b, _ := p.env(t, "b", false)
	backing := t.TempDir()
	ga.SetBacking(map[string]string{".claude": backing})
	ka := serveFakeKernel(t, ga, true)
	eventually(t, "the kernel to be spoken to", func() bool {
		ka.mu.Lock()
		defer ka.mu.Unlock()
		return ka.cleared > 0
	})
	// Shared once the kernel reports changes, so nothing watches it.
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
