package vm

import (
	"context"
	"errors"
	"io"
	"os"

	"golang.org/x/sys/unix"
)

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
