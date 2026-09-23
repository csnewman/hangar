//go:build !linux

package vm

import (
	"io/fs"
	"syscall"
)

// sysProcAttr has nothing to add where environments cannot run anyway:
// preflight refuses every host but Linux.
func sysProcAttr() *syscall.SysProcAttr { return nil }

// diskUsage is a file's size where allocated blocks are not to hand.
func diskUsage(fi fs.FileInfo) int64 { return fi.Size() }
