package vm

import (
	"io/fs"
	"syscall"
)

// sysProcAttr has nothing to add on macOS, which cannot tie a process to its
// parent's death: hangar-krun watches a pipe from the worker instead.
func sysProcAttr() *syscall.SysProcAttr { return nil }

// diskUsage is the space a file takes on disk: its allocated blocks, which
// for a sparse image is what it holds rather than how big it claims to be.
func diskUsage(fi fs.FileInfo) int64 {
	if st, ok := fi.Sys().(*syscall.Stat_t); ok {
		return st.Blocks * 512
	}
	return fi.Size()
}
