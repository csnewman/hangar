package profile

import (
	"errors"
	"syscall"
)

// detachStale lets go of a mount at dir left by an agent that has gone: it
// answers nothing, and would be in the way of a new one.
func detachStale(dir string) error {
	var st syscall.Stat_t
	if err := syscall.Stat(dir, &st); errors.Is(err, syscall.ENOTCONN) {
		return syscall.Unmount(dir, syscall.MNT_DETACH)
	}
	return nil
}
