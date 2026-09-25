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
	"time"

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

	mu       sync.Mutex
	progress fetchProgress
}

// A fetch goes through these stages: a pull downloads, then unpacks; an
// image with a local source is copied.
const (
	stageCopy     = "copy"
	stageDownload = "download"
	stageUnpack   = "unpack"
)

// fetchProgress is how far a fetch has got: its stage, and bytes done of
// the stage's total, where it has one. Unpacking counts the compressed
// bytes of the layers read so far, layer being the one in hand.
type fetchProgress struct {
	stage         string
	done, total   int64
	layer, layers int
}

func (f *fetch) report(p fetchProgress) {
	f.mu.Lock()
	f.progress = p
	f.mu.Unlock()
}

func (f *fetch) snapshot() fetchProgress {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.progress
}

// watchEvery is how often a caller waiting on a fetch is told how far it
// has got.
const watchEvery = 500 * time.Millisecond

// watch tells fn how far f has got until it is done or stop closes.
func (f *fetch) watch(fn func(fetchProgress), stop <-chan struct{}) {
	t := time.NewTicker(watchEvery)
	defer t.Stop()
	for {
		if p := f.snapshot(); p.stage != "" {
			fn(p)
		}
		select {
		case <-t.C:
		case <-f.done:
			return
		case <-stop:
			return
		}
	}
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
// fetch, and each is told how far it has got through watch.
func (s *store) get(ctx context.Context, ref string, watch func(fetchProgress)) (Image, error) {
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
			stop := make(chan struct{})
			go f.watch(watch, stop)
			f.err = s.fetch(ctx, ref, dst, f.report)
			close(stop)
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
		stop := make(chan struct{})
		go f.watch(watch, stop)
		select {
		case <-f.done:
			if f.err != nil {
				return Image{}, f.err
			}
		case <-ctx.Done():
			close(stop)
			return Image{}, ctx.Err()
		}
		close(stop)
	}
}

// fetch brings an image into the store: copied from its local source if
// the configuration names one, and pulled from its registry otherwise.
func (s *store) fetch(ctx context.Context, ref, dst string, report func(fetchProgress)) error {
	tmp := dst + ".fetching"
	os.RemoveAll(tmp)
	if err := os.MkdirAll(tmp, 0o755); err != nil {
		return err
	}
	err := func() error {
		rootfs := filepath.Join(tmp, "rootfs")
		if src, ok := s.sources[ref]; ok {
			report(fetchProgress{stage: stageCopy})
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
			digest, err := pull(ctx, ref, rootfs, blobs, s.auth, report)
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
