package vm

import (
	"fmt"
	"os"
	"path/filepath"
)

// base is the root filesystem of a held copy: a local copy's directory. A
// pulled copy has none: it is its EROFS image, read where it is needed.
func (s *ImageStore) base(h heldCopy) (string, error) {
	if h.image {
		return "", nil
	}
	return filepath.Join(h.dir, "rootfs"), nil
}

// removeCopy deletes a copy's directory, taking down a mount of its image
// at rootfs/ first, which an older worker made. It fails rather than delete
// through a mount it could not take down.
func removeCopy(h heldCopy) error {
	target := filepath.Join(h.dir, "rootfs")
	if h.image && isEROFS(target) {
		if err := unmount(target); err != nil {
			return fmt.Errorf("unmounting %s: %w", target, err)
		}
	}
	return os.RemoveAll(h.dir)
}
