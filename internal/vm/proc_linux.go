package vm

import (
	"io/fs"
	"syscall"
)

// sysProcAttr ties the monitor to the worker: if the worker dies without
// shutting it down, the kernel stops the monitor too, rather than leaving a
// guest running that nothing supervises or reports.
func sysProcAttr() *syscall.SysProcAttr {
	return &syscall.SysProcAttr{Pdeathsig: syscall.SIGKILL}
}

// diskUsage is the space a file takes on disk: its allocated blocks, which
// for a sparse image is what it holds rather than how big it claims to be.
func diskUsage(fi fs.FileInfo) int64 {
	if st, ok := fi.Sys().(*syscall.Stat_t); ok {
		return st.Blocks * 512
	}
	return fi.Size()
}
