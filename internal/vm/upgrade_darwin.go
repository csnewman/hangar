package vm

import (
	"context"
	"fmt"
	"os"

	"golang.org/x/sys/unix"
)

// Only Linux mounts the writable disk, so the files a newer image's would be
// hidden by are not listed on macOS: the upgrade is offered without them.
func mountReadOnly(disk, dir string) (func(), error) {
	return nil, fmt.Errorf("listing what the writable disk hides needs Linux to mount it: %w", errUnsupported)
}

func compareLayer(upper, oldRoot, newRoot string) ([]string, bool, error) {
	return nil, false, fmt.Errorf("comparing layers is %w", errUnsupported)
}

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
