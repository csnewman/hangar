// Package blob keeps the server's bulk bytes -- the files of users'
// profiles, the registry's blobs -- in an S3-compatible object store, while
// what they are and who has them stays in the database. Every replica of the
// server sees the same objects.
package blob

import (
	"context"
	"errors"
	"io"
	"time"
)

// ErrNotFound is an object that is not there.
var ErrNotFound = errors.New("no such object")

// Object is one object as a listing shows it.
type Object struct {
	Key      string
	Size     int64
	Modified time.Time
}

// Reader reads an object, seeking within it as ranged requests do.
type Reader interface {
	io.ReadSeekCloser
	Size() int64
}

// Store holds objects by key.
type Store interface {
	// Put writes an object, replacing any with its key. A size of -1 is
	// unknown, and read to the end.
	Put(ctx context.Context, key string, r io.Reader, size int64) error
	// Get opens an object, or returns ErrNotFound.
	Get(ctx context.Context, key string) (Reader, error)
	// Stat returns an object's size and time, or ErrNotFound.
	Stat(ctx context.Context, key string) (Object, error)
	// Copy writes src's contents as dst, within the store.
	Copy(ctx context.Context, src, dst string) error
	// Delete removes an object; one that is not there is not an error.
	Delete(ctx context.Context, key string) error
	// List returns the objects whose keys start with prefix, in key order.
	List(ctx context.Context, prefix string) ([]Object, error)
}

// Versioned holds objects by key in a bucket that keeps each write as a
// version of its own, so a reader asks for exactly the version it was told
// of, and a writer deletes the version it replaced.
type Versioned interface {
	// PutVersion writes a new version of an object, returning its ID.
	PutVersion(ctx context.Context, key string, r io.Reader, size int64) (string, error)
	// GetVersion opens one version of an object, or returns ErrNotFound.
	GetVersion(ctx context.Context, key, version string) (Reader, error)
	// DeleteVersion removes one version of an object; one that is not
	// there is not an error.
	DeleteVersion(ctx context.Context, key, version string) error
	// DeleteAll removes every version of every object whose key starts
	// with prefix.
	DeleteAll(ctx context.Context, prefix string) error
}

// ReadAll returns an object's contents.
func ReadAll(ctx context.Context, s Store, key string) ([]byte, error) {
	r, err := s.Get(ctx, key)
	if err != nil {
		return nil, err
	}
	defer r.Close()
	return io.ReadAll(r)
}

// ReadVersion returns one version of an object's contents.
func ReadVersion(ctx context.Context, s Versioned, key, version string) ([]byte, error) {
	r, err := s.GetVersion(ctx, key, version)
	if err != nil {
		return nil, err
	}
	defer r.Close()
	return io.ReadAll(r)
}
