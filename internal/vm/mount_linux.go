package vm

import "golang.org/x/sys/unix"

func unmount(target string) error {
	return unix.Unmount(target, unix.MNT_DETACH)
}

// erofsMagic is EROFS's superblock magic, which statfs reports as its type.
const erofsMagic = 0xE0F5E1E2

// isEROFS is whether an EROFS image is mounted at path.
func isEROFS(path string) bool {
	var st unix.Statfs_t
	return unix.Statfs(path, &st) == nil && uint32(st.Type) == erofsMagic
}
