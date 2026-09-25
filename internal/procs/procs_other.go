//go:build !linux

package procs

import "errors"

// Processes are read from Linux's /proc; guests are always Linux.
var errNotLinux = errors.New("the process list needs Linux")

func (s *Server) list() ([]Process, int, int64, error) { return nil, 0, 0, errNotLinux }

func kill(pid int, signal string) error { return errNotLinux }
