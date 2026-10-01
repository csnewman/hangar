package vm

import (
	"fmt"
	"os/exec"
	"strings"

	"golang.org/x/sys/unix"
)

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

// erofsMagic is EROFS's superblock magic, which statfs reports as its type.
const erofsMagic = 0xE0F5E1E2

// mountEROFS mounts an EROFS image file read-only at target: from the file
// itself where the kernel takes a file (6.12 on), and through a loop device
// where it wants a block device.
func mountEROFS(file, target string) error {
	err := unix.Mount(file, target, "erofs", unix.MS_RDONLY|unix.MS_NODEV|unix.MS_NOSUID, "")
	if err == nil {
		return nil
	}
	out, lerr := exec.Command("mount", "-t", "erofs", "-o", "ro,nodev,nosuid,loop", file, target).CombinedOutput()
	if lerr != nil {
		return fmt.Errorf("%w; through a loop device: %v: %s", err, lerr, strings.TrimSpace(string(out)))
	}
	return nil
}

// isEROFS is whether an EROFS image is mounted at path.
func isEROFS(path string) bool {
	var st unix.Statfs_t
	return unix.Statfs(path, &st) == nil && uint32(st.Type) == erofsMagic
}
