//go:build !linux

package vm

import (
	"errors"
)

// Only Linux mounts EROFS: elsewhere a copy's image is read with
// internal/erofs instead.
var errNoMount = errors.New("mounting images needs Linux")

// mountsImages is whether a copy's EROFS image is mounted for its root
// filesystem.
const mountsImages = false

func unmount(string) error { return errNoMount }

func mountEROFS(string, string) error { return errNoMount }

func isEROFS(string) bool { return false }
