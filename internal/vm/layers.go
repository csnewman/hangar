package vm

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"

	"github.com/opencontainers/go-digest"
)

// A pulled image is its layers, each unpacked once into the store's layers
// directory whatever images share it, and stacked read-only with overlayfs
// into the copy's root filesystem. Images built on one base share its
// layers on disk, and -- the overlay reading through to the one file
// beneath -- in the host's page cache, which DAX maps into every guest.
//
// A layer is unpacked with overlayfs's whiteouts, so it can sit in a stack
// as it is. A copy lists its layers, bottom first, in its layers file; a
// layer no copy lists is deleted when the copies change.

// layerDir is where a layer is unpacked: layers/<algorithm>-<hex>.
func (s *ImageStore) layerDir(d digest.Digest) string {
	return filepath.Join(s.dir, "layers", string(d.Algorithm())+"-"+d.Encoded())
}

// hasLayer is whether a layer is unpacked in the store.
func (s *ImageStore) hasLayer(d digest.Digest) bool {
	_, err := os.Stat(s.layerDir(d))
	return err == nil
}

// addLayer unpacks a layer, read uncompressed from r, into the store. A
// layer unpacked meanwhile by another fetch stands, and this one is
// dropped.
func (s *ImageStore) addLayer(ctx context.Context, d digest.Digest, r io.Reader) error {
	parent := filepath.Join(s.dir, "layers")
	if err := os.MkdirAll(parent, 0o755); err != nil {
		return err
	}
	tmp, err := os.MkdirTemp(parent, ".unpacking-")
	if err != nil {
		return err
	}
	if err := unpackLayer(ctx, tmp, r); err != nil {
		os.RemoveAll(tmp)
		return err
	}
	if err := os.Rename(tmp, s.layerDir(d)); err != nil {
		os.RemoveAll(tmp)
		if s.hasLayer(d) {
			return nil
		}
		return err
	}
	return nil
}

// claimLayers marks layers as wanted by a fetch in progress, so pruning
// leaves them until the copy that lists them is held. The returned func
// releases them.
func (s *ImageStore) claimLayers(ds []digest.Digest) func() {
	s.mu.Lock()
	for _, d := range ds {
		s.claimed[d]++
	}
	s.mu.Unlock()
	return func() {
		s.mu.Lock()
		for _, d := range ds {
			if s.claimed[d]--; s.claimed[d] <= 0 {
				delete(s.claimed, d)
			}
		}
		s.mu.Unlock()
	}
}

// pruneLayers deletes the layers no copy lists and no fetch has claimed,
// and anything left half unpacked.
func (s *ImageStore) pruneLayers() error {
	parent := filepath.Join(s.dir, "layers")
	entries, err := os.ReadDir(parent)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	s.mu.Lock()
	keep := map[string]bool{}
	for _, h := range s.held {
		for _, d := range h.layers {
			keep[filepath.Base(s.layerDir(d))] = true
		}
	}
	for d := range s.claimed {
		keep[filepath.Base(s.layerDir(d))] = true
	}
	unpacking := len(s.fetching) > 0
	s.mu.Unlock()
	var errs []error
	for _, e := range entries {
		name := e.Name()
		if keep[name] || strings.HasPrefix(name, ".unpacking-") && unpacking {
			continue
		}
		if err := os.RemoveAll(filepath.Join(parent, name)); err != nil {
			errs = append(errs, err)
		}
	}
	return errors.Join(errs...)
}

// readLayers reads a copy's layers file, bottom first. A copy without one is
// a single directory, not a stack.
func readLayers(copyDir string) ([]digest.Digest, error) {
	b, err := os.ReadFile(filepath.Join(copyDir, "layers"))
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	var out []digest.Digest
	for _, line := range strings.Fields(string(b)) {
		d, err := digest.Parse(line)
		if err != nil {
			return nil, fmt.Errorf("%s: %w", filepath.Join(copyDir, "layers"), err)
		}
		out = append(out, d)
	}
	return out, nil
}

func writeLayers(copyDir string, ds []digest.Digest) error {
	var b strings.Builder
	for _, d := range ds {
		b.WriteString(d.String())
		b.WriteByte('\n')
	}
	return os.WriteFile(filepath.Join(copyDir, "layers"), []byte(b.String()), 0o644)
}

// base is the root filesystem of a held copy, stacking its layers first if
// it is a stack and they are not stacked. A single layer is the root
// filesystem as it is.
func (s *ImageStore) base(h heldCopy) (string, error) {
	switch len(h.layers) {
	case 0:
		return filepath.Join(h.dir, "rootfs"), nil
	case 1:
		return s.layerDir(h.layers[0]), nil
	}
	target := filepath.Join(h.dir, "rootfs")
	if isOverlay(target) {
		return target, nil
	}
	if err := os.MkdirAll(target, 0o755); err != nil {
		return "", err
	}
	// overlayfs takes its lower layers top first.
	lowers := make([]string, len(h.layers))
	for i, d := range h.layers {
		lowers[len(h.layers)-1-i] = s.layerDir(d)
	}
	if err := mountOverlay(target, lowers); err != nil {
		return "", fmt.Errorf("stacking the image's layers: %w", err)
	}
	return target, nil
}

// removeCopy deletes a copy's directory, unstacking its layers first. It
// fails rather than delete through a stack it could not take down, which
// would reach into the layers other copies share.
func removeCopy(h heldCopy) error {
	if len(h.layers) > 1 {
		target := filepath.Join(h.dir, "rootfs")
		if isOverlay(target) {
			if err := unmount(target); err != nil {
				return fmt.Errorf("unstacking %s: %w", target, err)
			}
		}
	}
	return os.RemoveAll(h.dir)
}
