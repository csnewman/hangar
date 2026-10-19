package vm

import (
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

// removeCopy deletes a copy's directory.
func removeCopy(h heldCopy) error {
	return os.RemoveAll(h.dir)
}
