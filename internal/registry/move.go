package registry

import (
	"context"
	"errors"
	"io/fs"
	"os"
	"path/filepath"

	"github.com/opencontainers/go-digest"

	"github.com/csnewman/hangar/internal/blob"
)

// MoveFromDir moves the blobs of a registry kept in a directory,
// blobs/<algorithm>/<first two>/<hex>, into the blob store, deleting each
// file once it is there, and drops the directory's uploads in progress. It
// returns how many blobs it moved.
func (reg *Registry) MoveFromDir(ctx context.Context, dir string) (int, error) {
	moved := 0
	root := filepath.Join(dir, "blobs")
	err := filepath.WalkDir(root, func(path string, e fs.DirEntry, err error) error {
		if errors.Is(err, fs.ErrNotExist) {
			return nil
		}
		if err != nil || e.IsDir() {
			return err
		}
		rel, _ := filepath.Rel(root, path)
		alg, hex := filepath.Dir(filepath.Dir(rel)), filepath.Base(rel)
		d := digest.NewDigestFromEncoded(digest.Algorithm(alg), hex)
		if d.Validate() != nil {
			return nil
		}
		if _, err := reg.store.Stat(ctx, blobKey(d)); errors.Is(err, blob.ErrNotFound) {
			f, err := os.Open(path)
			if err != nil {
				return err
			}
			st, err := f.Stat()
			if err == nil {
				err = reg.store.Put(ctx, blobKey(d), f, st.Size())
			}
			f.Close()
			if err != nil {
				return err
			}
		} else if err != nil {
			return err
		}
		moved++
		return os.Remove(path)
	})
	if err != nil {
		return moved, err
	}
	if err := os.RemoveAll(filepath.Join(dir, "uploads")); err != nil {
		return moved, err
	}
	return moved, nil
}
