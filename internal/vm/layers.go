package vm

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/opencontainers/go-digest"
)

// A pulled copy is an EROFS image of the tree its layers describe, written
// as it is pulled (stream.go), and its root filesystem is that image mounted
// read-only on the host, for the worker's own reading of it.
//
// A copy pulled before that is its layers, each unpacked once into the
// store's layers directory, with overlayfs's whiteouts, and stacked
// read-only with overlayfs into the copy's root filesystem. Such a copy lists
// its layers, bottom first, in its layers file; a layer no copy lists is
// deleted when the copies change.

// layerDir is where a layer is unpacked: layers/<algorithm>-<hex>.
func (s *ImageStore) layerDir(d digest.Digest) string {
	return filepath.Join(s.dir, "layers", string(d.Algorithm())+"-"+d.Encoded())
}

// pruneLayers deletes the layers no copy lists, and anything left half
// unpacked.
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
	s.mu.Unlock()
	var errs []error
	for _, e := range entries {
		name := e.Name()
		if keep[name] {
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

// base is the root filesystem of a held copy, stacking its layers first if
// it is a stack and they are not stacked. A single layer is the root
// filesystem as it is.
func (s *ImageStore) base(h heldCopy) (string, error) {
	if h.image {
		target := filepath.Join(h.dir, "rootfs")
		if isEROFS(target) {
			return target, nil
		}
		if err := os.MkdirAll(target, 0o755); err != nil {
			return "", err
		}
		if err := mountEROFS(filepath.Join(h.dir, erofsFile), target); err != nil {
			return "", fmt.Errorf("mounting the image: %w", err)
		}
		return target, nil
	}
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
	target := filepath.Join(h.dir, "rootfs")
	if (h.image && isEROFS(target)) || (len(h.layers) > 1 && isOverlay(target)) {
		if err := unmount(target); err != nil {
			return fmt.Errorf("unmounting %s: %w", target, err)
		}
	}
	return os.RemoveAll(h.dir)
}
