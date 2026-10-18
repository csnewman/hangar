//go:build !linux

package vsock

import (
	"errors"
	"os"
	"time"
)

// AF_VSOCK is Linux's: a guest uses it, and so does a Linux host. These
// stubs keep the tree building elsewhere; a macOS host reaches its guests
// through hybrid sockets (ListenHybrid) instead.
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
