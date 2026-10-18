//go:build !linux && !darwin

package vm

import (
	"context"
	"errors"
)

type sharedFiles struct{}

func serveSharedFiles(context.Context, string, uint32, InstanceConfig) (*sharedFiles, error) {
	return nil, errors.New("the shared files are served on Linux and macOS")
}

func (*sharedFiles) Close() error { return nil }
