//go:build !linux

package fsuser

// Do runs fn as the process itself: only Linux sets a thread's filesystem
// identity.
func Do(u User, fn func() error) error { return fn() }
