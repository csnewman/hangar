package vm

import (
	"fmt"
	"os/exec"
	"strings"

	"golang.org/x/sys/unix"
)

func unmount(target string) error {
	return unix.Unmount(target, unix.MNT_DETACH)
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
