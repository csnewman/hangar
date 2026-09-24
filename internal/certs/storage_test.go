package certs_test

import (
	"context"
	"errors"
	"io/fs"
	"slices"
	"testing"
	"time"

	"github.com/csnewman/hangar/internal/certs"
	"github.com/csnewman/hangar/internal/dbtest"
)

func TestStorage(t *testing.T) {
	ctx := context.Background()
	s := certs.NewStorage(dbtest.Open(t))

	if _, err := s.Load(ctx, "missing"); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("loading a missing key: %v", err)
	}
	for k, v := range map[string]string{
		"certificates/acme/hangar.example.com/hangar.example.com.crt": "cert",
		"certificates/acme/hangar.example.com/hangar.example.com.key": "key",
		"certificates/acme/other/other.crt":                           "other",
		"acme/account.json":                                           "account",
	} {
		if err := s.Store(ctx, k, []byte(v)); err != nil {
			t.Fatal(err)
		}
	}
	if err := s.Store(ctx, "acme/account.json", []byte("replaced")); err != nil {
		t.Fatal(err)
	}
	if v, err := s.Load(ctx, "acme/account.json"); err != nil || string(v) != "replaced" {
		t.Fatalf("load after replace: %q %v", v, err)
	}

	top, err := s.List(ctx, "certificates/acme", false)
	if err != nil || !slices.Equal(top, []string{"certificates/acme/hangar.example.com", "certificates/acme/other"}) {
		t.Fatalf("list: %v %v", top, err)
	}
	all, err := s.List(ctx, "certificates", true)
	if err != nil || len(all) != 3 {
		t.Fatalf("recursive list: %v %v", all, err)
	}

	info, err := s.Stat(ctx, "certificates/acme/hangar.example.com/hangar.example.com.key")
	if err != nil || !info.IsTerminal || info.Size != 3 {
		t.Fatalf("stat key: %+v %v", info, err)
	}
	dir, err := s.Stat(ctx, "certificates/acme/hangar.example.com")
	if err != nil || dir.IsTerminal {
		t.Fatalf("stat directory: %+v %v", dir, err)
	}
	if s.Exists(ctx, "certificates/acme/nothing") {
		t.Fatal("a missing key exists")
	}

	if err := s.Delete(ctx, "certificates/acme/hangar.example.com"); err != nil {
		t.Fatal(err)
	}
	if s.Exists(ctx, "certificates/acme/hangar.example.com/hangar.example.com.crt") {
		t.Fatal("deleting a directory left a key in it")
	}
	if !s.Exists(ctx, "certificates/acme/other/other.crt") {
		t.Fatal("deleting a directory took a sibling's key")
	}

	// A lock is held by one taker at a time; the second waits for it.
	if err := s.Lock(ctx, "issue"); err != nil {
		t.Fatal(err)
	}
	short, cancel := context.WithTimeout(ctx, 1500*time.Millisecond)
	err = s.Lock(short, "issue")
	cancel()
	if !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("taking a held lock: %v", err)
	}
	if err := s.RenewLockLease(ctx, "issue", time.Minute); err != nil {
		t.Fatal(err)
	}
	got := make(chan error, 1)
	go func() { got <- s.Lock(ctx, "issue") }()
	time.Sleep(200 * time.Millisecond)
	if err := s.Unlock(ctx, "issue"); err != nil {
		t.Fatal(err)
	}
	select {
	case err := <-got:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("the waiting taker did not get the lock once it was released")
	}
	// An expired lease is anybody's.
	if err := s.RenewLockLease(ctx, "issue", -time.Minute); err != nil {
		t.Fatal(err)
	}
	quick, cancel := context.WithTimeout(ctx, 2*time.Second)
	defer cancel()
	if err := s.Lock(quick, "issue"); err != nil {
		t.Fatalf("taking an expired lock: %v", err)
	}
}
