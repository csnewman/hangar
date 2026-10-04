//go:build !linux

package profile

// detachStale has nothing to do where LockFS does not run.
func detachStale(string) error { return nil }
