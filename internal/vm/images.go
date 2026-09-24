package vm

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"io/fs"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
	"sync"

	"github.com/csnewman/hangar/internal/api"
)

// store is the worker's local copy of the images its environments boot
// from.
//
// An image is fetched the first time an environment needs it and kept until
// it is removed. It is pulled from its registry, unless the worker's
// configuration names a local source for the reference, which is copied.
// An environment boots from the local copy, never from the source, so
// removing a copy and fetching it again is safe whatever the source does
// meanwhile. A tag that moves in the registry is pulled again only once the
// copy is removed.
//
// Each image is a directory named for its reference, holding the base -- the
// image's root filesystem, as rootfs/ -- and a file naming the reference. A
// directory being fetched is named with ".fetching" and renamed into place
// when complete, so a copy interrupted half made is fetched again rather
// than booted.
type store struct {
	dir     string
	sources map[string]Image
	auth    map[string]RegistryAuth

	mu       sync.Mutex
	fetching map[string]*fetch
}

type fetch struct {
	done chan struct{}
	err  error
}

func newStore(dir string, sources map[string]Image, auth map[string]RegistryAuth) (*store, error) {
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return nil, err
	}
	// Anything left mid-fetch by an earlier run is started again from
	// scratch when next asked for.
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil, err
	}
	for _, e := range entries {
		if strings.HasSuffix(e.Name(), ".fetching") {
			os.RemoveAll(filepath.Join(dir, e.Name()))
		}
	}
	return &store{dir: dir, sources: sources, auth: auth, fetching: map[string]*fetch{}}, nil
}

var unsafeChars = regexp.MustCompile(`[^A-Za-z0-9._-]+`)

// path is where an image lives in the store: its reference made safe for a
// file name, with a short hash so two references that differ only in
// unsafe characters do not collide.
func (s *store) path(ref string) string {
	sum := sha256.Sum256([]byte(ref))
	name := strings.Trim(unsafeChars.ReplaceAllString(ref, "_"), "_")
	if len(name) > 80 {
		name = name[:80]
	}
	return filepath.Join(s.dir, name+"-"+hex.EncodeToString(sum[:4]))
}

// get returns the local copy of an image, fetching it first if the store
// does not hold it. Environments asking for the same image at once share one
// fetch.
func (s *store) get(ctx context.Context, ref string) (Image, error) {
	dst := s.path(ref)
	local := Image{Base: filepath.Join(dst, "rootfs")}

	for {
		if _, err := os.Stat(filepath.Join(dst, "ref")); err == nil {
			return local, nil
		}
		s.mu.Lock()
		f, busy := s.fetching[ref]
		if !busy {
			f = &fetch{done: make(chan struct{})}
			s.fetching[ref] = f
			s.mu.Unlock()
			f.err = s.fetch(ctx, ref, dst)
			s.mu.Lock()
			delete(s.fetching, ref)
			s.mu.Unlock()
			close(f.done)
			if f.err != nil {
				return Image{}, f.err
			}
			continue
		}
		s.mu.Unlock()
		select {
		case <-f.done:
			if f.err != nil {
				return Image{}, f.err
			}
		case <-ctx.Done():
			return Image{}, ctx.Err()
		}
	}
}

// fetch brings an image into the store: copied from its local source if
// the configuration names one, and pulled from its registry otherwise.
func (s *store) fetch(ctx context.Context, ref, dst string) error {
	tmp := dst + ".fetching"
	os.RemoveAll(tmp)
	if err := os.MkdirAll(tmp, 0o755); err != nil {
		return err
	}
	err := func() error {
		rootfs := filepath.Join(tmp, "rootfs")
		if src, ok := s.sources[ref]; ok {
			// Owners, modes, links and extended attributes are the image,
			// and virtio-fs passes them to the guest as they are, so the
			// copy keeps them all.
			out, err := exec.CommandContext(ctx, "cp", "-a", "--reflink=auto", "--", src.Base, rootfs).
				CombinedOutput()
			if err != nil {
				return fmt.Errorf("fetching %s: copying %s: %v: %s", ref, src.Base, err, strings.TrimSpace(string(out)))
			}
		} else {
			blobs := filepath.Join(tmp, "blobs")
			digest, err := pull(ctx, ref, rootfs, blobs, s.auth)
			if err != nil {
				return fmt.Errorf("pulling %s: %w", ref, err)
			}
			if err := os.RemoveAll(blobs); err != nil {
				return err
			}
			if err := os.WriteFile(filepath.Join(tmp, "digest"), []byte(digest), 0o644); err != nil {
				return err
			}
		}
		return os.WriteFile(filepath.Join(tmp, "ref"), []byte(ref), 0o644)
	}()
	if err != nil {
		os.RemoveAll(tmp)
		return err
	}
	return os.Rename(tmp, dst)
}

// list reports every image in the store, and the ones being fetched.
func (s *store) list() []api.LocalImage {
	entries, err := os.ReadDir(s.dir)
	if err != nil {
		return []api.LocalImage{}
	}
	out := []api.LocalImage{}
	for _, e := range entries {
		if !e.IsDir() || strings.HasSuffix(e.Name(), ".fetching") {
			continue
		}
		dir := filepath.Join(s.dir, e.Name())
		ref, err := os.ReadFile(filepath.Join(dir, "ref"))
		if err != nil {
			continue
		}
		out = append(out, api.LocalImage{Ref: string(ref), SizeBytes: allocated(dir), State: "ready",
			Environments: []string{}})
	}
	s.mu.Lock()
	for ref := range s.fetching {
		out = append(out, api.LocalImage{Ref: ref, SizeBytes: allocated(s.path(ref) + ".fetching"),
			State: "fetching", Environments: []string{}})
	}
	s.mu.Unlock()
	sort.Slice(out, func(i, j int) bool { return out[i].Ref < out[j].Ref })
	return out
}

// remove deletes an image's local copy.
func (s *store) remove(ref string) error {
	s.mu.Lock()
	_, busy := s.fetching[ref]
	s.mu.Unlock()
	if busy {
		return nil
	}
	return os.RemoveAll(s.path(ref))
}

// allocated is the space a file or tree takes on disk, which for a sparse
// image is what it holds rather than how big it claims to be.
func allocated(path string) int64 {
	var total int64
	filepath.WalkDir(path, func(p string, d fs.DirEntry, err error) error {
		if err != nil {
			return nil
		}
		if info, err := d.Info(); err == nil && !info.IsDir() {
			total += diskUsage(info)
		}
		return nil
	})
	return total
}
