package vm

import "syscall"

// sysProcAttr ties the monitor to the worker: if the worker dies without
// shutting it down, the kernel stops the monitor too, rather than leaving a
// guest running that nothing supervises or reports.
func sysProcAttr() *syscall.SysProcAttr {
	return &syscall.SysProcAttr{Pdeathsig: syscall.SIGKILL}
}
