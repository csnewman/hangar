//go:build !linux

package ch

import "syscall"

// childAttrs is Linux's alone: backends run only on Linux.
func childAttrs() *syscall.SysProcAttr { return nil }
