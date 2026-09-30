package vm

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"sync"
)

// erofsFile is where a copy keeps its EROFS image, in the copy's directory,
// so it goes when the copy does.
const erofsFile = "image.erofs"

// erofsAlign is what an EROFS image's size is rounded up to: a
// persistent-memory region is aligned to 2 MiB.
const erofsAlign = 2 << 20

// erofsLocks serialises the building of each copy's image, so machines
// starting on a copy together build it once.
var erofsLocks sync.Map

// EROFS returns an EROFS image of an image's base, building it the first
// time. It is uncompressed, with 4 KiB blocks, so a guest can map its files
// directly (DAX) and every guest shares the host's page cache of it.
func (s *ImageStore) EROFS(ctx context.Context, img Image) (string, error) {
	if img.Copy == "" {
		return "", fmt.Errorf("the image at %s is not a copy in the store", img.Base)
	}
	path := filepath.Join(img.Copy, erofsFile)
	mu, _ := erofsLocks.LoadOrStore(path, &sync.Mutex{})
	mu.(*sync.Mutex).Lock()
	defer mu.(*sync.Mutex).Unlock()
	if _, err := os.Stat(path); err == nil {
		return path, nil
	}
	tmp := path + ".building"
	os.Remove(tmp)
	cmd := exec.CommandContext(ctx, "mkfs.erofs", "--quiet", "-b", "4096", tmp, img.Base)
	if out, err := cmd.CombinedOutput(); err != nil {
		os.Remove(tmp)
		return "", fmt.Errorf("building the image's EROFS image: %w: %s", err, lastLineOf(out))
	}
	st, err := os.Stat(tmp)
	if err != nil {
		return "", err
	}
	if rem := st.Size() % erofsAlign; rem != 0 {
		if err := os.Truncate(tmp, st.Size()+erofsAlign-rem); err != nil {
			os.Remove(tmp)
			return "", err
		}
	}
	if err := os.Rename(tmp, path); err != nil {
		os.Remove(tmp)
		return "", err
	}
	return path, nil
}

func lastLineOf(b []byte) string {
	for len(b) > 0 && (b[len(b)-1] == '\n' || b[len(b)-1] == ' ') {
		b = b[:len(b)-1]
	}
	for i := len(b) - 1; i >= 0; i-- {
		if b[i] == '\n' {
			return string(b[i+1:])
		}
	}
	return string(b)
}
