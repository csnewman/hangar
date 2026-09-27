package vm

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
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

	"github.com/opencontainers/go-digest"

	"github.com/csnewman/hangar/internal/api"
)

// ImageStore is the worker's local copy of the images its environments boot
// from.
//
// It holds copies, each of what a reference named at one moment: the
// manifest a registry's tag pointed to, or one build of a local source. A
// reference whose image moves on gets a second copy beside the first, so
// an environment made from the first keeps booting from it -- its writable
// layer was made over that root filesystem, and would find another's
// files changed beneath it. Current looks up what a reference names,
// which is the copy a new environment is made from.
//
// A copy is fetched the first time it is asked for and kept until it is
// removed. It is pulled from its registry, unless the worker's
// configuration names a local source for the reference, which is copied.
//
// Each copy is a directory holding files naming its reference and digest,
// and its root filesystem as rootfs/: for a pulled image, its layers stacked
// there (see layers.go); for a local build, a copy of it. A directory being
// fetched is named with ".fetching" and renamed into place when complete,
// so a copy interrupted half made is fetched again rather than booted.
type ImageStore struct {
	dir     string
	sources map[string]Image
	auth    map[string]RegistryAuth

	mu       sync.Mutex
	held     map[api.ImageCopy]heldCopy
	current  map[string]string
	fetching map[api.ImageCopy]*fetch
	hangar   *hangarRegistry
	// claimed are layers a fetch in progress wants, by how many.
	claimed map[digest.Digest]int
}

// hangarRegistry is Hangar's own registry: images named on host are pulled
// from url, beneath which is the distribution API's /v2/, with the worker's
// credential.
type hangarRegistry struct {
	host, url, credential string
}

// UseRegistry says where Hangar's own registry is. An empty host means there
// is none.
func (s *ImageStore) UseRegistry(host, url, credential string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if host == "" {
		s.hangar = nil
		return
	}
	s.hangar = &hangarRegistry{host: host, url: url, credential: credential}
}

func (s *ImageStore) registry() *hangarRegistry {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.hangar
}

type heldCopy struct {
	dir string
	// at is when the copy was taken, which orders copies of one reference.
	at time.Time
	// layers are the copy's layers, bottom first; none for a copy that is
	// a directory of its own.
	layers []digest.Digest
}

type fetch struct {
	done chan struct{}
	err  error

	mu       sync.Mutex
	progress FetchProgress
}

// A fetch goes through these stages: a pull downloads, then unpacks; an
// image with a local source is copied.
const (
	StageCopy     = "copy"
	StageDownload = "download"
	StageUnpack   = "unpack"
)

// FetchProgress is how far a fetch has got: its stage, and bytes done of
// the stage's total, where it has one. Unpacking counts the compressed
// bytes of the layers read so far, Layer being the one in hand.
type FetchProgress struct {
	Stage         string
	Done, Total   int64
	Layer, Layers int
}

func (f *fetch) report(p FetchProgress) {
	f.mu.Lock()
	f.progress = p
	f.mu.Unlock()
}

func (f *fetch) snapshot() FetchProgress {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.progress
}

// watchEvery is how often a caller waiting on a fetch is told how far it
// has got.
const watchEvery = 500 * time.Millisecond

// watch tells fn how far f has got until it is done or stop closes.
func (f *fetch) watch(fn func(FetchProgress), stop <-chan struct{}) {
	t := time.NewTicker(watchEvery)
	defer t.Stop()
	for {
		if p := f.snapshot(); p.Stage != "" && fn != nil {
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

// NewImageStore opens the store in dir, taking in the copies already
// there. sources maps a reference to a local build copied in for it, and
// auth signs pulls in to registries, by host.
func NewImageStore(dir string, sources map[string]Image, auth map[string]RegistryAuth) (*ImageStore, error) {
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return nil, err
	}
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil, err
	}
	s := &ImageStore{dir: dir, sources: sources, auth: auth, held: map[api.ImageCopy]heldCopy{},
		current: map[string]string{}, fetching: map[api.ImageCopy]*fetch{}, claimed: map[digest.Digest]int{}}
	for _, e := range entries {
		path := filepath.Join(dir, e.Name())
		// Anything left mid-fetch by an earlier run is started again from
		// scratch when next asked for.
		if strings.HasSuffix(e.Name(), ".fetching") {
			os.RemoveAll(path)
			continue
		}
		if !e.IsDir() {
			continue
		}
		ref, err := os.ReadFile(filepath.Join(path, "ref"))
		if err != nil {
			continue
		}
		st, err := os.Stat(filepath.Join(path, "ref"))
		if err != nil {
			continue
		}
		// A copy with no digest file is a copy of its own, digest "",
		// which no lookup of its reference names.
		dgst, _ := os.ReadFile(filepath.Join(path, "digest"))
		layers, err := readLayers(path)
		if err != nil {
			return nil, err
		}
		c := api.ImageCopy{Ref: string(ref), Digest: strings.TrimSpace(string(dgst))}
		s.held[c] = heldCopy{dir: path, at: st.ModTime(), layers: layers}
	}
	if err := s.pruneLayers(); err != nil {
		return nil, err
	}
	return s, nil
}

var unsafeChars = regexp.MustCompile(`[^A-Za-z0-9._-]+`)

// path is where a copy lives in the store: its reference made safe for a
// file name, with a short hash of the reference and digest, so two copies
// of one reference, or two references that differ only in unsafe
// characters, do not collide.
func (s *ImageStore) path(c api.ImageCopy) string {
	sum := sha256.Sum256([]byte(c.Ref + "@" + c.Digest))
	name := strings.Trim(unsafeChars.ReplaceAllString(c.Ref, "_"), "_")
	if len(name) > 80 {
		name = name[:80]
	}
	return filepath.Join(s.dir, name+"-"+hex.EncodeToString(sum[:4]))
}

// Current looks up what ref names: the digest of the manifest its
// registry has for it, or which build its local source holds. The store
// remembers the answer, as what List calls current.
func (s *ImageStore) Current(ctx context.Context, ref string) (string, error) {
	var digest string
	var err error
	if src, ok := s.sources[ref]; ok {
		digest, err = buildOf(src.Base)
	} else {
		digest, err = resolveDigest(ctx, ref, s.auth, s.registry())
	}
	if err != nil {
		return "", err
	}
	s.mu.Lock()
	s.current[ref] = digest
	s.mu.Unlock()
	return digest, nil
}

// buildOf identifies the build a local source holds: the ID `hangar build`
// gives each build, and for a root filesystem made some other way, the
// directory as it is on disk, so replacing it counts as a new build.
func buildOf(rootfs string) (string, error) {
	info, _, err := ReadImageInfo(rootfs)
	if err != nil {
		return "", err
	}
	if info.Build != "" {
		return "build:" + info.Build, nil
	}
	fp := fingerprint(rootfs)
	if fp == "" {
		return "", fmt.Errorf("local image %s is missing", rootfs)
	}
	sum := sha256.Sum256([]byte(fp))
	return "local:" + hex.EncodeToString(sum[:8]), nil
}

// Newest is the digest of the copy of ref taken last, if the store holds
// one.
func (s *ImageStore) Newest(ref string) (string, bool) {
	return s.pick(ref, func(a, b time.Time) bool { return a.After(b) })
}

// Oldest is the digest of the copy of ref taken first, if the store holds
// one.
func (s *ImageStore) Oldest(ref string) (string, bool) {
	return s.pick(ref, func(a, b time.Time) bool { return a.Before(b) })
}

func (s *ImageStore) pick(ref string, better func(a, b time.Time) bool) (string, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	var digest string
	var at time.Time
	found := false
	for c, h := range s.held {
		if c.Ref == ref && (!found || better(h.at, at)) {
			digest, at, found = c.Digest, h.at, true
		}
	}
	return digest, found
}

// Get returns the store's copy c, fetching it first if the store does not
// hold it. Callers asking for the same copy at once share one fetch, and
// each is told how far it has got through watch, which may be nil.
func (s *ImageStore) Get(ctx context.Context, c api.ImageCopy, watch func(FetchProgress)) (Image, error) {
	for {
		s.mu.Lock()
		if h, ok := s.held[c]; ok {
			s.mu.Unlock()
			base, err := s.base(h)
			if err != nil {
				return Image{}, err
			}
			img := Image{Base: base}
			if len(h.layers) > 1 {
				img.ID = c.Ref + "@" + c.Digest
			}
			return img, nil
		}
		f, busy := s.fetching[c]
		if !busy {
			f = &fetch{done: make(chan struct{})}
			s.fetching[c] = f
			s.mu.Unlock()
			stop := make(chan struct{})
			go f.watch(watch, stop)
			dst := s.path(c)
			layers, release, err := s.fetch(ctx, c, dst, f.report)
			f.err = err
			close(stop)
			s.mu.Lock()
			delete(s.fetching, c)
			if f.err == nil {
				s.held[c] = heldCopy{dir: dst, at: time.Now(), layers: layers}
			}
			s.mu.Unlock()
			release()
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
			close(stop)
			if f.err != nil {
				return Image{}, f.err
			}
		case <-ctx.Done():
			close(stop)
			return Image{}, ctx.Err()
		}
	}
}

// fetch brings a copy into the store: copied from its local source if the
// configuration names one, and pulled from its registry by digest
// otherwise.
//
// A pull returns the copy's layers, and a func releasing the claim it holds
// on them until the copy is held and so keeps them itself.
func (s *ImageStore) fetch(ctx context.Context, c api.ImageCopy, dst string, report func(FetchProgress)) (
	[]digest.Digest, func(), error) {
	release := func() {}
	var layers []digest.Digest
	tmp := dst + ".fetching"
	os.RemoveAll(tmp)
	if err := os.MkdirAll(tmp, 0o755); err != nil {
		return nil, release, err
	}
	err := func() error {
		rootfs := filepath.Join(tmp, "rootfs")
		if src, ok := s.sources[c.Ref]; ok {
			// A local source holds one build, and only that build can be
			// copied from it.
			build, err := buildOf(src.Base)
			if err != nil {
				return fmt.Errorf("fetching %s: %w", c.Ref, err)
			}
			if build != c.Digest {
				return fmt.Errorf("fetching %s: its local source holds %s, not %s, and this worker holds no copy of that",
					c.Ref, build, c.Digest)
			}
			report(FetchProgress{Stage: StageCopy})
			// Owners, modes, links and extended attributes are the image,
			// and virtio-fs passes them to the guest as they are, so the
			// copy keeps them all.
			out, err := exec.CommandContext(ctx, "cp", "-a", "--reflink=auto", "--", src.Base, rootfs).
				CombinedOutput()
			if err != nil {
				return fmt.Errorf("fetching %s: copying %s: %v: %s", c.Ref, src.Base, err, strings.TrimSpace(string(out)))
			}
		} else {
			blobs := filepath.Join(tmp, "blobs")
			var err error
			layers, release, err = pull(ctx, c.Ref, c.Digest, blobs, s.auth, s.registry(), s, report)
			if err != nil {
				return fmt.Errorf("pulling %s: %w", c.Ref, err)
			}
			if err := os.RemoveAll(blobs); err != nil {
				return err
			}
			if err := writeLayers(tmp, layers); err != nil {
				return err
			}
			if err := os.Mkdir(rootfs, 0o755); err != nil {
				return err
			}
		}
		if err := os.WriteFile(filepath.Join(tmp, "digest"), []byte(c.Digest), 0o644); err != nil {
			return err
		}
		return os.WriteFile(filepath.Join(tmp, "ref"), []byte(c.Ref), 0o644)
	}()
	if err != nil {
		os.RemoveAll(tmp)
		return nil, release, err
	}
	return layers, release, os.Rename(tmp, dst)
}

// isCurrent is whether c is what its reference names, as last looked up,
// or the newest copy of a reference not looked up since the store opened.
// The caller holds s.mu.
func (s *ImageStore) isCurrent(c api.ImageCopy) bool {
	if d, ok := s.current[c.Ref]; ok {
		return d == c.Digest
	}
	at := s.held[c].at
	for o, h := range s.held {
		if o.Ref == c.Ref && h.at.After(at) {
			return false
		}
	}
	return true
}

// List reports every copy in the store, and the ones being fetched. It
// leaves each one's Environments empty: which environments use a copy is
// the runtime's to say.
func (s *ImageStore) List() []api.LocalImage {
	s.mu.Lock()
	type entry struct {
		c       api.ImageCopy
		dir     string
		state   string
		current bool
		// layers are a stacked copy's, whose size is theirs; shared ones
		// count in every copy that has them.
		layers []string
	}
	var entries []entry
	for c, h := range s.held {
		e := entry{c: c, dir: h.dir, state: "ready", current: s.isCurrent(c)}
		for _, d := range h.layers {
			e.layers = append(e.layers, s.layerDir(d))
		}
		entries = append(entries, e)
	}
	for c := range s.fetching {
		entries = append(entries, entry{c: c, dir: s.path(c) + ".fetching", state: "fetching", current: s.current[c.Ref] == c.Digest})
	}
	s.mu.Unlock()

	out := make([]api.LocalImage, 0, len(entries))
	for _, e := range entries {
		size := allocated(e.dir)
		if len(e.layers) > 0 {
			// The stack itself is the layers' files, seen again.
			size = 0
			for _, l := range e.layers {
				size += allocated(l)
			}
		}
		out = append(out, api.LocalImage{Ref: e.c.Ref, Digest: e.c.Digest, SizeBytes: size,
			State: e.state, Current: e.current, Environments: []string{}})
	}
	sort.Slice(out, func(i, j int) bool {
		if out[i].Ref != out[j].Ref {
			return out[i].Ref < out[j].Ref
		}
		return out[i].Digest < out[j].Digest
	})
	return out
}

// Remove deletes a copy. One being fetched is left to finish.
func (s *ImageStore) Remove(c api.ImageCopy) error {
	s.mu.Lock()
	h, ok := s.held[c]
	if !ok {
		s.mu.Unlock()
		return nil
	}
	delete(s.held, c)
	s.mu.Unlock()
	if err := removeCopy(h); err != nil && !errors.Is(err, fs.ErrNotExist) {
		// Still there, and still held.
		s.mu.Lock()
		s.held[c] = h
		s.mu.Unlock()
		return err
	}
	return s.pruneLayers()
}

// Prune deletes every copy that is not current and that inUse says no
// environment uses: one a reference has moved on from, once the last
// environment made from it is gone. It reports the copies it deleted.
func (s *ImageStore) Prune(inUse func(api.ImageCopy) bool) ([]api.ImageCopy, error) {
	s.mu.Lock()
	var stale []api.ImageCopy
	for c := range s.held {
		if !s.isCurrent(c) && !inUse(c) {
			stale = append(stale, c)
		}
	}
	s.mu.Unlock()
	var errs []error
	var removed []api.ImageCopy
	for _, c := range stale {
		if err := s.Remove(c); err != nil {
			errs = append(errs, err)
			continue
		}
		removed = append(removed, c)
	}
	return removed, errors.Join(errs...)
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
