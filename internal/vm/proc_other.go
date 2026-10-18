//go:build !linux && !darwin

package vm

import (
	"io/fs"
	"syscall"
)

// sysProcAttr has nothing to add where environments cannot run:
// preflight refuses every host but Linux and macOS.
func sysProcAttr() *syscall.SysProcAttr { return nil }

// diskUsage is a file's size where allocated blocks are not to hand.
func diskUsage(fi fs.FileInfo) int64 { return fi.Size() }

// guestRAM is unknown where environments cannot run.
func guestRAM(int) int64 { return 0 }

// processTicks is unknown where environments cannot run.
func processTicks(int) uint64 { return 0 }
