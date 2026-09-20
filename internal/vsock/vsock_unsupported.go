//go:build !linux

package vsock

import (
	"errors"
	"os"
	"time"
)

// Hangar runs on Linux. These stubs exist so the tree still builds, vets and
// loads in an editor on another operating system; nothing here can succeed,
// and host.Detect refuses before anything reaches them.
var errNotLinux = errors.New("vsock requires a Linux host")

// Dial always fails here.
func Dial(cid, port uint32) (*os.File, error) { return nil, errNotLinux }

// Listener has no working methods here.
type Listener struct{}

// Listen always fails here.
func Listen(cid, port uint32) (*Listener, error) { return nil, errNotLinux }

// Accept always fails here.
func (l *Listener) Accept(timeout time.Duration) (*os.File, uint32, error) {
	return nil, 0, errNotLinux
}

// Close is a no-op here.
func (l *Listener) Close() error { return nil }

// LocalCID always fails here.
func LocalCID() (uint32, error) { return 0, errNotLinux }

// SuggestGuestCID returns the first non-reserved CID here.
func SuggestGuestCID() uint32 { return FirstGuestCID }
