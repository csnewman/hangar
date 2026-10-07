package vm

import (
	"fmt"
	"os"
	"path/filepath"
)

// base is the root filesystem of a held copy: a pulled copy's EROFS image,
// mounted read-only, or a local copy's directory.
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
	return filepath.Join(h.dir, "rootfs"), nil
}

// removeCopy deletes a copy's directory, unmounting its image first. It
// fails rather than delete through a mount it could not take down.
func removeCopy(h heldCopy) error {
	target := filepath.Join(h.dir, "rootfs")
	if h.image && isEROFS(target) {
		if err := unmount(target); err != nil {
			return fmt.Errorf("unmounting %s: %w", target, err)
		}
	}
	return os.RemoveAll(h.dir)
}
