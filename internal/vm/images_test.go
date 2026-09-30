package vm_test

import (
	"context"
	"encoding/binary"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"testing"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/vm"
)

// linuxOnly skips a test that copies local builds in: the store copies
// with GNU cp, as a worker's Linux host has.
func linuxOnly(t *testing.T) {
	if runtime.GOOS != "linux" {
		t.Skip("the store copies with GNU cp")
	}
}

// build writes a root filesystem into dir, replacing what was there: a
// file saying which build it is and, if id is set, an image.json naming
// the build.
func build(t *testing.T, dir, id, content string) {
	t.Helper()
	if err := os.RemoveAll(dir); err != nil {
		t.Fatal(err)
	}
	info := filepath.Join(dir, vm.ImageInfoPath)
	if err := os.MkdirAll(filepath.Dir(info), 0o755); err != nil {
		t.Fatal(err)
	}
	if id != "" {
		if err := os.WriteFile(info, []byte(`{"desktop": false, "build": "`+id+`"}`), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	if err := os.WriteFile(filepath.Join(dir, "which"), []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}
}

func which(t *testing.T, img vm.Image) string {
	t.Helper()
	b, err := os.ReadFile(filepath.Join(img.Base, "which"))
	if err != nil {
		t.Fatal(err)
	}
	return string(b)
}

// A rebuilt local source is a new copy beside the old one: an environment
// made from the old one keeps it, a new one gets the new build, and the old
// copy goes once nothing uses it.
func TestImageStoreKeepsEachBuild(t *testing.T) {
	linuxOnly(t)
	ctx := context.Background()
	src := filepath.Join(t.TempDir(), "rootfs")
	build(t, src, "one", "first")
	const ref = "example.com/img:tag"
	s, err := vm.NewImageStore(t.TempDir(), map[string]vm.Image{ref: {Base: src}}, nil)
	if err != nil {
		t.Fatal(err)
	}

	first, err := s.Current(ctx, ref)
	if err != nil || first != "build:one" {
		t.Fatalf("current: %q, %v; want build:one", first, err)
	}
	firstCopy := api.ImageCopy{Ref: ref, Digest: first}
	img, err := s.Get(ctx, firstCopy, nil)
	if err != nil {
		t.Fatal(err)
	}
	if got := which(t, img); got != "first" {
		t.Fatalf("the first copy holds %q", got)
	}

	build(t, src, "two", "second")
	second, err := s.Current(ctx, ref)
	if err != nil || second != "build:two" {
		t.Fatalf("current after a rebuild: %q, %v; want build:two", second, err)
	}
	secondCopy := api.ImageCopy{Ref: ref, Digest: second}
	img2, err := s.Get(ctx, secondCopy, nil)
	if err != nil {
		t.Fatal(err)
	}
	if got := which(t, img2); got != "second" {
		t.Fatalf("the second copy holds %q", got)
	}
	again, err := s.Get(ctx, firstCopy, nil)
	if err != nil || which(t, again) != "first" || again.Base != img.Base {
		t.Fatalf("the first copy after a rebuild: %+v, %v", again, err)
	}

	list := s.List()
	if len(list) != 2 {
		t.Fatalf("listed %d copies, want 2: %+v", len(list), list)
	}
	for _, l := range list {
		if want := l.Digest == second; l.Current != want {
			t.Errorf("copy %s: current %v, want %v", l.Digest, l.Current, want)
		}
	}

	// In use, the old copy stays; unused, it goes. The current copy stays
	// whatever.
	if removed, err := s.Prune(func(c api.ImageCopy) bool { return c == firstCopy }); err != nil || len(removed) != 0 {
		t.Fatalf("pruning with the old copy in use removed %v, %v", removed, err)
	}
	removed, err := s.Prune(func(api.ImageCopy) bool { return false })
	if err != nil || len(removed) != 1 || removed[0] != firstCopy {
		t.Fatalf("pruning with nothing in use removed %v, %v; want the first copy", removed, err)
	}
	if _, err := os.Stat(img.Base); !os.IsNotExist(err) {
		t.Fatalf("the pruned copy is still on disk: %v", err)
	}
	if len(s.List()) != 1 {
		t.Fatalf("after pruning: %+v", s.List())
	}

	// A build the source no longer holds cannot be copied again.
	if _, err := s.Get(ctx, firstCopy, nil); err == nil {
		t.Fatal("fetched a build the local source no longer holds")
	}
}

// A store opened again takes in the copies already there, the newest of
// each reference counting as current until the reference is looked up.
func TestImageStoreReopens(t *testing.T) {
	linuxOnly(t)
	ctx := context.Background()
	src := filepath.Join(t.TempDir(), "rootfs")
	dir := t.TempDir()
	const ref = "example.com/img:tag"
	sources := map[string]vm.Image{ref: {Base: src}}
	s, err := vm.NewImageStore(dir, sources, nil)
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range []string{"one", "two"} {
		build(t, src, id, id)
		d, err := s.Current(ctx, ref)
		if err != nil {
			t.Fatal(err)
		}
		if _, err := s.Get(ctx, api.ImageCopy{Ref: ref, Digest: d}, nil); err != nil {
			t.Fatal(err)
		}
	}

	s, err = vm.NewImageStore(dir, sources, nil)
	if err != nil {
		t.Fatal(err)
	}
	if d, ok := s.Newest(ref); !ok || d != "build:two" {
		t.Fatalf("newest: %q, %v", d, ok)
	}
	if d, ok := s.Oldest(ref); !ok || d != "build:one" {
		t.Fatalf("oldest: %q, %v", d, ok)
	}
	for _, l := range s.List() {
		if want := l.Digest == "build:two"; l.Current != want {
			t.Errorf("copy %s: current %v, want %v", l.Digest, l.Current, want)
		}
	}
	if _, ok := s.Newest("example.com/other:tag"); ok {
		t.Error("found a copy of a reference never fetched")
	}
}

// A local source without a build ID is told apart by the directory itself:
// replaced, it is a new build.
func TestImageStoreSourceWithoutBuildID(t *testing.T) {
	ctx := context.Background()
	src := filepath.Join(t.TempDir(), "rootfs")
	build(t, src, "", "first")
	const ref = "example.com/img:tag"
	s, err := vm.NewImageStore(t.TempDir(), map[string]vm.Image{ref: {Base: src}}, nil)
	if err != nil {
		t.Fatal(err)
	}
	first, err := s.Current(ctx, ref)
	if err != nil {
		t.Fatal(err)
	}
	if again, _ := s.Current(ctx, ref); again != first {
		t.Fatalf("the same directory looked up twice: %q then %q", first, again)
	}
	build(t, src, "", "second")
	second, err := s.Current(ctx, ref)
	if err != nil {
		t.Fatal(err)
	}
	if second == first {
		t.Fatalf("a replaced directory kept its digest %q", first)
	}
}

// A copy's EROFS image is made once, where the copy is, padded to what a
// persistent-memory region is aligned to, and handed out again after.
func TestImageStoreEROFS(t *testing.T) {
	linuxOnly(t)
	if _, err := exec.LookPath("mkfs.erofs"); err != nil {
		t.Skip("mkfs.erofs is not installed")
	}
	ctx := context.Background()
	src := filepath.Join(t.TempDir(), "rootfs")
	build(t, src, "one", "first")
	const ref = "example.com/img:tag"
	s, err := vm.NewImageStore(t.TempDir(), map[string]vm.Image{ref: {Base: src}}, nil)
	if err != nil {
		t.Fatal(err)
	}
	d, err := s.Current(ctx, ref)
	if err != nil {
		t.Fatal(err)
	}
	img, err := s.Get(ctx, api.ImageCopy{Ref: ref, Digest: d}, nil)
	if err != nil {
		t.Fatal(err)
	}
	f, err := s.EROFS(ctx, img)
	if err != nil {
		t.Fatal(err)
	}
	b, err := os.ReadFile(f)
	if err != nil {
		t.Fatal(err)
	}
	// EROFS's superblock is 1024 bytes in, and starts with its magic.
	if len(b) < 1028 || binary.LittleEndian.Uint32(b[1024:]) != 0xE0F5E1E2 {
		t.Fatalf("%s is not an EROFS image", f)
	}
	if len(b)%(2<<20) != 0 {
		t.Errorf("the image is %d bytes, not a multiple of 2 MiB", len(b))
	}
	st, _ := os.Stat(f)
	again, err := s.EROFS(ctx, img)
	if err != nil {
		t.Fatal(err)
	}
	st2, _ := os.Stat(again)
	if again != f || !st2.ModTime().Equal(st.ModTime()) {
		t.Error("the image was made again")
	}
}
