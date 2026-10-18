package vm

import (
	"context"
	"fmt"
	"os"

	"golang.org/x/sys/unix"
)

// copySparse clones src to dst on APFS: the copy shares src's blocks until
// either is written, so it takes no time or space to make.
func copySparse(ctx context.Context, src, dst string, progress func(done, total int64)) (int64, error) {
	st, err := os.Stat(src)
	if err != nil {
		return 0, err
	}
	if err := unix.Clonefile(src, dst, unix.CLONE_NOFOLLOW); err != nil {
		return 0, fmt.Errorf("cloning %s: %w", src, err)
	}
	if progress != nil {
		progress(st.Size(), st.Size())
	}
	return st.Size(), nil
}
