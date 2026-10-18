//go:build !linux && !darwin

package vm

import (
	"context"
	"fmt"
)

func copySparse(ctx context.Context, src, dst string, progress func(done, total int64)) (int64, error) {
	return 0, fmt.Errorf("copying a disk is %w", errUnsupported)
}
