package ch

import "syscall"

// childAttrs ties a backend to the process that started it: when that
// process dies, however it dies, the kernel kills the backend too. Without
// it a backend outlives a crashed or killed supervisor, reparented to init.
// A backend that changes its credentials loses the signal; passt does, and
// ends with its monitor instead (see StartPasst).
func childAttrs() *syscall.SysProcAttr {
	return &syscall.SysProcAttr{Pdeathsig: syscall.SIGKILL}
}
