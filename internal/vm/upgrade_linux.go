package vm

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"

	"golang.org/x/sys/unix"
)

// mountReadOnly mounts an environment's writable disk at dir without
// writing to it: noload leaves a journal a machine that did not shut down
// cleanly left behind unreplayed.
func mountReadOnly(disk, dir string) (func(), error) {
	out, err := exec.Command("mount", "-t", "ext4", "-o", "ro,noload,loop", disk, dir).CombinedOutput()
	if err != nil {
		return nil, fmt.Errorf("%w: %s", err, strings.TrimSpace(string(out)))
	}
	return func() { exec.Command("umount", dir).Run() }, nil
}

// packageDatabases are where package managers record what is installed. A
// copy in an environment's writable layer describes the image it was made
// over plus what was installed on it, and hides the newer image's own.
var packageDatabases = []string{
	"var/lib/dpkg/status",
	"var/lib/rpm/rpmdb.sqlite",
	"var/lib/rpm/Packages",
	"usr/lib/sysimage/rpm/rpmdb.sqlite",
}

// ignoredPaths are left out of the comparison: scratch space, logs and
// caches, which nothing depends on, and the stamps systemd compares with
// /usr to decide whether to redo its post-update work (ldconfig among it),
// which an older stamp over a newer image makes it do.
var ignoredPaths = []string{"tmp", "var/tmp", "var/log", "var/cache", "etc/.updated", "var/.updated"}

// compareLayer lists what in an overlay's upper directory would hide a
// change between two copies of the image beneath it: a file it replaces or
// deletes that the copies differ in, or a directory it makes opaque whose
// entries they differ in. What it adds that neither copy has, or changes
// that both copies have alike, hides nothing new. packages says one of them
// is a package manager's database.
func compareLayer(upper, oldRoot, newRoot string) (conflicts []string, packages bool, err error) {
	oldFD, err := unix.Open(oldRoot, unix.O_PATH|unix.O_DIRECTORY|unix.O_CLOEXEC, 0)
	if err != nil {
		return nil, false, fmt.Errorf("opening %s: %w", oldRoot, err)
	}
	defer unix.Close(oldFD)
	newFD, err := unix.Open(newRoot, unix.O_PATH|unix.O_DIRECTORY|unix.O_CLOEXEC, 0)
	if err != nil {
		return nil, false, fmt.Errorf("opening %s: %w", newRoot, err)
	}
	defer unix.Close(newFD)

	err = filepath.WalkDir(upper, func(p string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		rel, err := filepath.Rel(upper, p)
		if err != nil || rel == "." {
			return err
		}
		if slices.Contains(ignoredPaths, rel) {
			if d.IsDir() {
				return filepath.SkipDir
			}
			return nil
		}
		var conflict bool
		if d.IsDir() {
			if !opaque(p) {
				return nil
			}
			conflict, err = entriesDiffer(oldFD, newFD, rel)
			rel += "/"
		} else {
			// A whiteout, which deletes the file beneath, is a character
			// device numbered 0:0, and is compared like anything else: what
			// matters is whether the copies differ in the path it hides.
			conflict, err = pathsDiffer(oldFD, newFD, rel)
		}
		if err != nil {
			return fmt.Errorf("%s: %w", rel, err)
		}
		if conflict {
			conflicts = append(conflicts, "/"+rel)
			if slices.Contains(packageDatabases, rel) {
				packages = true
			}
		}
		return nil
	})
	return conflicts, packages, err
}

// opaque is whether overlayfs hides everything beneath the directory.
func opaque(dir string) bool {
	buf := make([]byte, 8)
	n, err := unix.Lgetxattr(dir, "trusted.overlay.opaque", buf)
	return err == nil && string(buf[:n]) == "y"
}

// lookup opens rel inside the root at rootFD, resolving it as the guest
// would -- an absolute symlink stays inside the root -- without following
// its last component. It returns -1 for a path that is not there.
func lookup(rootFD int, rel string, flags uint64) (int, error) {
	fd, err := unix.Openat2(rootFD, rel, &unix.OpenHow{
		Flags:   flags | unix.O_NOFOLLOW | unix.O_CLOEXEC,
		Resolve: unix.RESOLVE_IN_ROOT | unix.RESOLVE_NO_MAGICLINKS,
	})
	switch {
	case errors.Is(err, unix.ENOENT), errors.Is(err, unix.ENOTDIR):
		return -1, nil
	case err != nil:
		return -1, err
	}
	return fd, nil
}

// pathsDiffer is whether the two roots differ at rel: one has it and the
// other not, or they have different kinds of file, targets or contents.
func pathsDiffer(oldFD, newFD int, rel string) (bool, error) {
	a, err := lookup(oldFD, rel, unix.O_PATH)
	if err != nil {
		return false, err
	}
	if a >= 0 {
		defer unix.Close(a)
	}
	b, err := lookup(newFD, rel, unix.O_PATH)
	if err != nil {
		return false, err
	}
	if b >= 0 {
		defer unix.Close(b)
	}
	if a < 0 || b < 0 {
		return (a < 0) != (b < 0), nil
	}
	var sa, sb unix.Stat_t
	if err := unix.Fstat(a, &sa); err != nil {
		return false, err
	}
	if err := unix.Fstat(b, &sb); err != nil {
		return false, err
	}
	if sa.Mode&unix.S_IFMT != sb.Mode&unix.S_IFMT || sa.Mode != sb.Mode || sa.Uid != sb.Uid || sa.Gid != sb.Gid {
		return true, nil
	}
	switch sa.Mode & unix.S_IFMT {
	case unix.S_IFLNK:
		ta, err := readlinkFD(a)
		if err != nil {
			return false, err
		}
		tb, err := readlinkFD(b)
		if err != nil {
			return false, err
		}
		return ta != tb, nil
	case unix.S_IFREG:
		if sa.Size != sb.Size {
			return true, nil
		}
		return contentsDiffer(oldFD, newFD, rel)
	case unix.S_IFCHR, unix.S_IFBLK:
		return sa.Rdev != sb.Rdev, nil
	}
	return false, nil
}

func readlinkFD(fd int) (string, error) {
	buf := make([]byte, 4096)
	n, err := unix.Readlinkat(fd, "", buf)
	if err != nil {
		return "", err
	}
	return string(buf[:n]), nil
}

func contentsDiffer(oldFD, newFD int, rel string) (bool, error) {
	a, err := lookup(oldFD, rel, unix.O_RDONLY)
	if err != nil || a < 0 {
		return true, err
	}
	fa := os.NewFile(uintptr(a), rel)
	defer fa.Close()
	b, err := lookup(newFD, rel, unix.O_RDONLY)
	if err != nil || b < 0 {
		return true, err
	}
	fb := os.NewFile(uintptr(b), rel)
	defer fb.Close()
	ba, bb := make([]byte, 64<<10), make([]byte, 64<<10)
	for {
		na, ea := io.ReadFull(fa, ba)
		nb, eb := io.ReadFull(fb, bb)
		if na != nb || !bytes.Equal(ba[:na], bb[:nb]) {
			return true, nil
		}
		aDone := ea == io.EOF || ea == io.ErrUnexpectedEOF
		bDone := eb == io.EOF || eb == io.ErrUnexpectedEOF
		if aDone || bDone {
			return aDone != bDone, nil
		}
		if ea != nil {
			return false, ea
		}
		if eb != nil {
			return false, eb
		}
	}
}

// entriesDiffer is whether the two roots' directories at rel hold different
// names, or one has the directory and the other not.
func entriesDiffer(oldFD, newFD int, rel string) (bool, error) {
	names := func(root int) ([]string, bool, error) {
		fd, err := lookup(root, rel, unix.O_RDONLY|unix.O_DIRECTORY)
		if err != nil || fd < 0 {
			return nil, false, err
		}
		f := os.NewFile(uintptr(fd), rel)
		defer f.Close()
		n, err := f.Readdirnames(-1)
		slices.Sort(n)
		return n, true, err
	}
	a, hasA, err := names(oldFD)
	if err != nil {
		return false, err
	}
	b, hasB, err := names(newFD)
	if err != nil {
		return false, err
	}
	return hasA != hasB || !slices.Equal(a, b), nil
}

// copySparse copies a sparse disk image, only the parts that hold data, and
// reports how much of that is done. It returns the bytes copied.
func copySparse(ctx context.Context, src, dst string, progress func(done, total int64)) (int64, error) {
	in, err := os.Open(src)
	if err != nil {
		return 0, err
	}
	defer in.Close()
	var st unix.Stat_t
	if err := unix.Fstat(int(in.Fd()), &st); err != nil {
		return 0, err
	}
	out, err := os.OpenFile(dst, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o644)
	if err != nil {
		return 0, err
	}
	defer out.Close()
	if err := out.Truncate(st.Size); err != nil {
		return 0, err
	}
	total := st.Blocks * 512
	var done int64
	const chunk = 64 << 20
	for off := int64(0); off < st.Size; {
		if err := ctx.Err(); err != nil {
			return done, err
		}
		data, err := unix.Seek(int(in.Fd()), off, unix.SEEK_DATA)
		if errors.Is(err, unix.ENXIO) {
			break
		}
		if err != nil {
			return done, err
		}
		hole, err := unix.Seek(int(in.Fd()), data, unix.SEEK_HOLE)
		if err != nil {
			return done, err
		}
		for data < hole {
			if err := ctx.Err(); err != nil {
				return done, err
			}
			rOff, wOff := data, data
			n, err := unix.CopyFileRange(int(in.Fd()), &rOff, int(out.Fd()), &wOff, int(min(hole-data, chunk)), 0)
			if err != nil {
				return done, err
			}
			if n == 0 {
				return done, io.ErrUnexpectedEOF
			}
			// Gigabytes through the page cache would fragment the host's free
			// memory, and guests started meanwhile would find no 2 MB blocks
			// for theirs, so each chunk is written out and dropped from it.
			if err := unix.SyncFileRange(int(out.Fd()), data, int64(n),
				unix.SYNC_FILE_RANGE_WAIT_BEFORE|unix.SYNC_FILE_RANGE_WRITE|unix.SYNC_FILE_RANGE_WAIT_AFTER); err != nil {
				return done, err
			}
			unix.Fadvise(int(out.Fd()), data, int64(n), unix.FADV_DONTNEED)
			unix.Fadvise(int(in.Fd()), data, int64(n), unix.FADV_DONTNEED)
			data += int64(n)
			done += int64(n)
			progress(done, max(total, done))
		}
		off = hole
	}
	if err := out.Sync(); err != nil {
		return done, err
	}
	progress(done, done)
	return done, nil
}
