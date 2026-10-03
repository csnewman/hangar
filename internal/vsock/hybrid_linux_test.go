//go:build linux

package vsock_test

import (
	"errors"
	"net"
	"path/filepath"
	"testing"
	"time"

	"github.com/csnewman/hangar/internal/vsock"
)

func listen(t *testing.T) (*vsock.HybridListener, string) {
	t.Helper()
	base := filepath.Join(t.TempDir(), "v")
	l, err := vsock.ListenHybrid(base, 1024, 7)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { l.Close() })
	return l, vsock.HybridPath(base, 1024)
}

func TestHybridAcceptTimesOut(t *testing.T) {
	l, _ := listen(t)
	start := time.Now()
	_, _, err := l.Accept(100 * time.Millisecond)
	if !errors.Is(err, vsock.ErrAcceptTimeout) {
		t.Fatalf("Accept: %v, want ErrAcceptTimeout", err)
	}
	if d := time.Since(start); d < 100*time.Millisecond || d > 2*time.Second {
		t.Fatalf("Accept gave up after %v", d)
	}
}

func TestHybridAcceptsWhileWaiting(t *testing.T) {
	l, path := listen(t)
	go func() {
		time.Sleep(100 * time.Millisecond)
		c, err := net.Dial("unix", path)
		if err == nil {
			c.Write([]byte("hi"))
			c.Close()
		}
	}()
	f, cid, err := l.Accept(5 * time.Second)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	if cid != 7 {
		t.Fatalf("cid %d, want 7", cid)
	}
	buf := make([]byte, 2)
	f.SetReadDeadline(time.Now().Add(5 * time.Second))
	if _, err := f.Read(buf); err != nil || string(buf) != "hi" {
		t.Fatalf("read %q, %v", buf, err)
	}
}

func TestHybridAcceptsMany(t *testing.T) {
	l, path := listen(t)
	for i := range 3 {
		c, err := net.Dial("unix", path)
		if err != nil {
			t.Fatal(err)
		}
		defer c.Close()
		f, _, err := l.Accept(5 * time.Second)
		if err != nil {
			t.Fatalf("connection %d: %v", i, err)
		}
		f.Close()
	}
}

func TestHybridCloseEndsAccept(t *testing.T) {
	l, _ := listen(t)
	done := make(chan error, 1)
	go func() {
		_, _, err := l.Accept(0)
		done <- err
	}()
	time.Sleep(100 * time.Millisecond)
	l.Close()
	select {
	case err := <-done:
		if err == nil {
			t.Fatal("Accept succeeded on a closed listener")
		}
	case <-time.After(5 * time.Second):
		t.Fatal("Accept still waiting after Close")
	}
}
