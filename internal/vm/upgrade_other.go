//go:build !linux

package vm

import (
	"context"
	"fmt"
)

func mountReadOnly(disk, dir string) (func(), error) {
	return nil, fmt.Errorf("mounting a disk is %w", errUnsupported)
}

func compareLayer(upper, oldRoot, newRoot string) ([]string, bool, error) {
	return nil, false, fmt.Errorf("comparing layers is %w", errUnsupported)
}

func copySparse(ctx context.Context, src, dst string, progress func(done, total int64)) (int64, error) {
	return 0, fmt.Errorf("copying a disk is %w", errUnsupported)
}
