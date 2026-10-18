package nfs

import "golang.org/x/sys/unix"

// The lock commands: OFD locks belong to the open file description, so
// each open and lock owner holds its own, as NFS's do.
const (
	ofdSetLock = unix.F_OFD_SETLK
	ofdGetLock = unix.F_OFD_GETLK
)

// openRoot opens the server's root, for the *at calls.
func openRoot(root string) (int, error) {
	return unix.Open(root, unix.O_PATH|unix.O_DIRECTORY|unix.O_CLOEXEC, 0)
}

// openBeneath opens a directory beneath dir, through no symbolic link:
// for the *at calls, or to read if read.
func openBeneath(dir int, p string, read bool) (int, error) {
	flags := unix.O_PATH
	if read {
		flags = unix.O_RDONLY
	}
	return unix.Openat2(dir, p, &unix.OpenHow{
		Flags:   uint64(flags | unix.O_DIRECTORY | unix.O_CLOEXEC),
		Resolve: unix.RESOLVE_BENEATH | unix.RESOLVE_NO_SYMLINKS | unix.RESOLVE_NO_MAGICLINKS,
	})
}

// The special nanoseconds utimensat takes: the time left alone, and now.
const (
	utimeOmit = unix.UTIME_OMIT
	utimeNow  = unix.UTIME_NOW
)

// allocate reserves space for a range, growing the file to hold it, or if
// punch punches a hole in it and keeps its size.
func allocate(fd int, punch bool, off, n int64) error {
	mode := uint32(0)
	if punch {
		mode = unix.FALLOC_FL_PUNCH_HOLE | unix.FALLOC_FL_KEEP_SIZE
	}
	return unix.Fallocate(fd, mode, off, n)
}

// mknodAt makes a FIFO or socket in dir.
func mknodAt(dir int, name string, mode uint32) error {
	return unix.Mknodat(dir, name, mode, 0)
}
