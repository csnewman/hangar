//go:build !linux

package vm

import (
	"context"
	"errors"
	"io"
)

// Stacking layers needs Linux's overlayfs, which a worker running
// environments has.
var errNoOverlay = errors.New("layered images need Linux")

func unpackLayer(context.Context, string, io.Reader) error { return errNoOverlay }

func mountOverlay(string, []string) error { return errNoOverlay }

func unmount(string) error { return errNoOverlay }

func isOverlay(string) bool { return false }
