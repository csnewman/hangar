package vm

import (
	"context"
	"io"

	"github.com/containerd/containerd/v2/pkg/archive"
	"golang.org/x/sys/unix"
)

// unpackLayer applies a layer's tar stream onto an empty directory, keeping
// its whiteouts as overlayfs's -- a character device for a removed file, an
// xattr on a directory whose lower contents are hidden -- so the directory
// is a layer an overlay can stack.
func unpackLayer(ctx context.Context, dir string, r io.Reader) error {
	_, err := archive.Apply(ctx, dir, r, archive.WithConvertWhiteout(archive.OverlayConvertWhiteout))
	return err
}

// mountOverlay stacks lowers, top first, read-only at target. The layers
// are given one at a time, which has no limit on how many or how long
// their paths are, as a single lowerdir option has.
func mountOverlay(target string, lowers []string) error {
	fd, err := unix.Fsopen("overlay", unix.FSOPEN_CLOEXEC)
	if err != nil {
		return err
	}
	defer unix.Close(fd)
	for _, l := range lowers {
		if err := unix.FsconfigSetString(fd, "lowerdir+", l); err != nil {
			return err
		}
	}
	if err := unix.FsconfigCreate(fd); err != nil {
		return err
	}
	mfd, err := unix.Fsmount(fd, unix.FSMOUNT_CLOEXEC, unix.MOUNT_ATTR_RDONLY)
	if err != nil {
		return err
	}
	defer unix.Close(mfd)
	return unix.MoveMount(mfd, "", unix.AT_FDCWD, target, unix.MOVE_MOUNT_F_EMPTY_PATH)
}

func unmount(target string) error {
	return unix.Unmount(target, unix.MNT_DETACH)
}

// isOverlay is whether an overlay is mounted at path.
func isOverlay(path string) bool {
	var st unix.Statfs_t
	return unix.Statfs(path, &st) == nil && st.Type == unix.OVERLAYFS_SUPER_MAGIC
}
