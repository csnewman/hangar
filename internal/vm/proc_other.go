//go:build !linux

package vm

import "syscall"

// sysProcAttr has nothing to add where environments cannot run anyway:
// preflight refuses every host but Linux.
func sysProcAttr() *syscall.SysProcAttr { return nil }
