//go:build darwin && krun

package main

import "syscall"

func syscallErr(n int) error { return syscall.Errno(n) }
