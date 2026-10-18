//go:build darwin && !cgo

package nfs

import "golang.org/x/sys/unix"

// mknodAt needs mknodat, which macOS offers through its C library alone.
func mknodAt(int, string, uint32) error { return unix.ENOTSUP }

// punchHole zeros the blocks, which stay allocated: F_PUNCHHOLE takes a
// structure x/sys/unix cannot pass.
func punchHole(fd int, off, n int64) error { return zero(fd, off, off+n) }
