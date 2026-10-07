//go:build !linux

package vm

import (
	"errors"
)

// Mounting images needs Linux, which a worker running environments has.
var errNoMount = errors.New("mounting images needs Linux")

func unmount(string) error { return errNoMount }

func mountEROFS(string, string) error { return errNoMount }

func isEROFS(string) bool { return false }
