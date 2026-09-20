//go:build !linux

package snapshot

import (
	"context"
	"errors"
)

// Hangar runs on Linux. This stub exists so the tree still builds, vets and
// loads in an editor elsewhere; overlayfs and the mount syscalls it needs are
// Linux-only, and host.Detect refuses before anything reaches here.
var errNotLinux = errors.New("the image store requires a Linux host")

// Store has no working methods here.
type Store struct{}

// Open always fails here.
func Open(root string) (*Store, error) { return nil, errNotLinux }

// Close is a no-op here.
func (s *Store) Close() error { return nil }

// Pull always fails here.
func (s *Store) Pull(ctx context.Context, ref, platform string) (string, error) {
	return "", errNotLinux
}

// Mount always fails here.
func (s *Store) Mount(ctx context.Context, chainID, key string) (string, error) {
	return "", errNotLinux
}

// Unmount always fails here.
func (s *Store) Unmount(ctx context.Context, key string) error { return errNotLinux }
