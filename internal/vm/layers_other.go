//go:build !linux

package vm

import (
	"errors"
)

// Stacking layers needs Linux's overlayfs, which a worker running
// environments has.
var errNoOverlay = errors.New("layered images need Linux")

func mountOverlay(string, []string) error { return errNoOverlay }

func unmount(string) error { return errNoOverlay }

func isOverlay(string) bool { return false }

func mountEROFS(string, string) error { return errNoOverlay }

func isEROFS(string) bool { return false }
