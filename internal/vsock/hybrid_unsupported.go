//go:build !linux

package vsock

import (
	"fmt"
	"os"
	"time"
)

// HybridListener has no working methods here. See vsock_unsupported.go.
type HybridListener struct{}

// ListenHybrid always fails here.
func ListenHybrid(base string, port, cid uint32) (*HybridListener, error) {
	return nil, errNotLinux
}

// HybridPath is pure string handling, so it answers the same everywhere.
func HybridPath(base string, port uint32) string {
	return fmt.Sprintf("%s_%d", base, port)
}

// Accept always fails here.
func (l *HybridListener) Accept(timeout time.Duration) (*os.File, uint32, error) {
	return nil, 0, errNotLinux
}

// Close is a no-op here.
func (l *HybridListener) Close() error { return nil }
