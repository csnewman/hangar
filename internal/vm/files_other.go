//go:build !linux

package vm

import (
	"context"
	"errors"
)

type sharedFiles struct{}

func serveSharedFiles(context.Context, string, uint32, InstanceConfig) (*sharedFiles, error) {
	return nil, errors.New("the shared files are only served on Linux")
}

func (*sharedFiles) Close() error { return nil }
