package nfs

import "golang.org/x/sys/unix"

// The lock commands: OFD locks belong to the open file description, so
// each open and lock owner holds its own, as NFS's do. macOS defines them
// in sys/fcntl.h, as x/sys/unix does not yet.
const (
	ofdSetLock = 90 // F_OFD_SETLK
	ofdGetLock = 92 // F_OFD_GETLK
)

// O_RESOLVE_BENEATH, from sys/fcntl.h: resolution may not leave the
// directory it starts from.
const oResolveBeneath = 0x00001000

// openRoot opens the server's root, for the *at calls.
func openRoot(root string) (int, error) {
	return unix.Open(root, oSearch|unix.O_CLOEXEC, 0)
}

// openBeneath opens a directory beneath dir, through no symbolic link:
// for the *at calls, or to read if read.
func openBeneath(dir int, p string, read bool) (int, error) {
	flags := oSearch
	if read {
		flags = unix.O_RDONLY
	}
	return unix.Openat(dir, p, flags|unix.O_DIRECTORY|unix.O_CLOEXEC|unix.O_NOFOLLOW_ANY|oResolveBeneath, 0)
}

// O_SEARCH, from sys/fcntl.h: a directory opened to search, not read.
const oSearch = 0x40000000 | unix.O_DIRECTORY

// The special nanoseconds utimensat takes on macOS: the time left alone,
// and now.
const (
	utimeOmit = -2
	utimeNow  = -1
)

// allocate reserves space for a range, growing the file to hold it, or if
// punch makes the range read as zeros and keeps the file's size: the whole
// blocks in it are punched out, the parts of blocks at its ends zeroed.
func allocate(fd int, punch bool, off, n int64) error {
	var st unix.Stat_t
	if err := unix.Fstat(fd, &st); err != nil {
		return err
	}
	end := off + n
	if !punch {
		if end <= st.Size {
			return nil
		}
		fs := unix.Fstore_t{Flags: unix.F_ALLOCATEALL, Posmode: unix.F_PEOFPOSMODE, Length: end - st.Size}
		unix.FcntlFstore(uintptr(fd), unix.F_PREALLOCATE, &fs)
		return unix.Ftruncate(fd, end)
	}
	end = min(end, st.Size)
	if off >= end {
		return nil
	}
	bs := int64(st.Blksize)
	if bs <= 0 {
		bs = 4096
	}
	first := (off + bs - 1) / bs * bs
	last := end / bs * bs
	if first < last {
		if err := punchHole(fd, first, last-first); err != nil {
			return err
		}
	} else {
		first, last = end, end
	}
	if err := zero(fd, off, first); err != nil {
		return err
	}
	return zero(fd, max(last, off), end)
}

// zero writes zeros over [from, to).
func zero(fd int, from, to int64) error {
	buf := make([]byte, 64<<10)
	for from < to {
		k := min(int64(len(buf)), to-from)
		w, err := unix.Pwrite(fd, buf[:k], from)
		if err != nil {
			return err
		}
		from += int64(w)
	}
	return nil
}
