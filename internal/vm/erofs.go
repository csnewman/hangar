package vm

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"sync"

	"github.com/csnewman/hangar/internal/erofs"
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
// time (internal/erofs). It is uncompressed, with 4 KiB blocks, so a guest
// can map its files directly (DAX) and every guest shares the host's page
// cache of it.
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
	err := writeEROFS(tmp, img.Base)
	if err == nil {
		err = padEROFS(tmp)
	}
	if err == nil {
		err = os.Rename(tmp, path)
	}
	if err != nil {
		os.Remove(tmp)
		return "", fmt.Errorf("building the image's EROFS image: %w", err)
	}
	return path, nil
}

func writeEROFS(out, dir string) error {
	f, err := os.Create(out)
	if err != nil {
		return err
	}
	w := erofs.NewWriter(f)
	err = w.AddDir(dir)
	if err == nil {
		err = w.Close()
	}
	if cerr := f.Close(); err == nil {
		err = cerr
	}
	return err
}
